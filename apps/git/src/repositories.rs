//! The daemon is the only owner of repository filesystem mutations (0052, 0053).
#![allow(clippy::result_large_err)]
use kajiba_core::{
    local::rpc_error,
    names::{is_public_id, validate_ref},
    state::{real_dir, sync_dir},
};
use kajiba_gitstore::{GitStore, Gix};
use kajiba_proto::git::v1::{FetchRequest, Session};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tonic::{Code, Status};

pub struct Repositories {
    root: PathBuf,
    mutation: Mutex<()>,
    sessions: Mutex<Sessions>,
}

#[derive(Default)]
struct Sessions {
    running: std::collections::BTreeMap<String, (Session, String)>,
    exited: std::collections::BTreeMap<String, (Session, i32)>,
    completed: [[u64; 2]; 4],
}

impl Repositories {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            mutation: Mutex::new(()),
            sessions: Mutex::default(),
        }
    }

    fn path(&self, id: &str) -> Result<PathBuf, Status> {
        if !is_public_id(id) {
            return Err(rpc_error(
                Code::InvalidArgument,
                "INVALID_FIELD",
                "invalid repo_public_id",
            ));
        }
        real_dir(&self.root.join("repos")).map_err(internal)?;
        Ok(self.root.join("repos").join(format!("{id}.git")))
    }

    fn present(&self, id: &str) -> Result<PathBuf, Status> {
        let path = self.path(id)?;
        real_dir(&path).map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                rpc_error(Code::NotFound, "REPO_NOT_FOUND", "repository not found")
            } else {
                internal(err)
            }
        })?;
        Ok(path)
    }

    pub fn create(&self, id: &str, branch: &str) -> Result<(), Status> {
        let _guard = self.mutation.lock().unwrap();
        let path = self.path(id)?;
        branch_valid(branch)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(rpc_error(
                    Code::AlreadyExists,
                    "ALREADY_EXISTS",
                    "repository already exists",
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(internal(err)),
        }
        let tmp = self.root.join("tmp");
        real_dir(&tmp).map_err(internal)?;
        let staging = tmp.join(kajiba_core::new_id().map_err(internal)?);
        fs::create_dir(&staging).map_err(internal)?;
        Gix.init_bare(&staging, branch).map_err(internal)?;
        sync_tree(&staging).map_err(internal)?;
        fs::rename(&staging, &path).map_err(internal)?;
        sync_dir(&tmp).map_err(internal)?;
        sync_dir(&self.root.join("repos")).map_err(internal)?;
        tracing::info!(event = "repo-created", repo = id);
        Ok(())
    }

    pub fn set_head(&self, id: &str, branch: &str) -> Result<(), Status> {
        let _guard = self.mutation.lock().unwrap();
        let path = self.present(id)?;
        branch_valid(branch)?;
        Gix.set_head(&path, branch).map_err(internal)
    }

    pub fn check_commits(
        &self,
        request: &kajiba_proto::hook::v1::PreReceiveRequest,
    ) -> Result<
        (
            kajiba_gitstore::push::Facts,
            Vec<kajiba_proto::hook::v1::Refusal>,
        ),
        Status,
    > {
        use kajiba_gitstore::push::{Quarantine, Update};
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let allowed_signers = {
            let sessions = self.sessions.lock().unwrap();
            let (_, signers) = sessions
                .running
                .get(&request.session_id)
                .filter(|(s, _)| {
                    s.kind == kajiba_proto::git::v1::SessionKind::ReceivePack as i32
                        && s.user_public_id == request.user_public_id
                        && s.repo_public_id == request.repo_public_id
                })
                .ok_or_else(|| {
                    rpc_error(
                        Code::FailedPrecondition,
                        "INVALID_STATE",
                        "active receive-pack session required",
                    )
                })?;
            signers.clone()
        };
        let path = self.present(&request.repo_public_id)?;
        let updates: Vec<_> = request
            .updates
            .iter()
            .map(|u| Update {
                old_id: &u.old_id,
                new_id: &u.new_id,
            })
            .collect();
        let context = request.quarantine.clone().unwrap_or_default();
        let quarantine = Quarantine {
            object_directory: &context.object_directory,
            alternate_object_directories: &context.alternate_object_directories,
            quarantine_path: &context.quarantine_path,
        };
        let facts = Gix
            .push_facts(&path, &updates, &quarantine)
            .map_err(internal)?;
        if facts.commits.is_empty() {
            return Ok((facts, Vec::new()));
        }
        let signers_path = self
            .root
            .join("tmp")
            .join(kajiba_core::new_id().map_err(internal)?);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&signers_path)
            .map_err(internal)?;
        let result = (|| {
            file.write_all(allowed_signers.as_bytes())
                .map_err(internal)?;
            Gix.verify_commits(&path, &updates, &facts, &quarantine, &signers_path)
                .map_err(internal)
        })();
        let cleanup = fs::remove_file(&signers_path).map_err(internal);
        let refusals = result?;
        cleanup?;
        Ok((
            facts,
            refusals
                .into_iter()
                .map(|r| kajiba_proto::hook::v1::Refusal {
                    ref_name: request.updates[r.update_index].ref_name.clone(),
                    reason: format!("commit {}: {}", r.commit, r.reason),
                })
                .collect(),
        ))
    }

    pub fn sessions(&self) -> Vec<Session> {
        self.sessions
            .lock()
            .unwrap()
            .running
            .values()
            .map(|(session, _)| session.clone())
            .collect()
    }

    /// `allowed_signers` is the OpenSession response, valid only for this session (0053).
    pub fn register(&self, session: Session, allowed_signers: String) -> Result<(), Status> {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.running.contains_key(&session.session_id)
            || sessions.exited.contains_key(&session.session_id)
        {
            return Err(rpc_error(
                Code::FailedPrecondition,
                "INVALID_STATE",
                "session already used",
            ));
        }
        sessions
            .running
            .insert(session.session_id.clone(), (session, allowed_signers));
        Ok(())
    }

    pub fn finished(&self, session_id: &str, status: i32) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some((session, _)) = sessions.running.remove(session_id) {
            if let Some(kind) = crate::metrics::kind_index(session.kind) {
                let count = &mut sessions.completed[kind][usize::from(status != 0)];
                *count = count.saturating_add(1);
            }
            tracing::info!(
                event = "session-exited",
                session = session.session_id,
                kind = "import",
                repo = session.repo_public_id,
                status
            );
            sessions.exited.insert(session_id.into(), (session, status));
        }
    }

    pub fn exits(&self) -> Vec<(Session, i32)> {
        self.sessions
            .lock()
            .unwrap()
            .exited
            .values()
            .cloned()
            .collect()
    }

    pub fn acknowledged(&self, session: &str) {
        self.sessions.lock().unwrap().exited.remove(session);
    }

    pub fn metrics(&self) -> String {
        let sessions = self.sessions.lock().unwrap();
        let mut active = [0; 4];
        for (session, _) in sessions.running.values() {
            if let Some(kind) = crate::metrics::kind_index(session.kind) {
                active[kind] += 1;
            }
        }
        crate::metrics::exposition(env!("CARGO_PKG_VERSION"), active, sessions.completed)
    }

    pub fn fetch(&self, request: FetchRequest) -> Result<(), Status> {
        // Keep the empty-target check and the fetch atomic against other mutations.
        let _guard = self.mutation.lock().unwrap();
        let path = self.present(&request.repo_public_id)?;
        if !Gix.refs(&path).map_err(internal)?.is_empty() {
            return Err(rpc_error(
                Code::FailedPrecondition,
                "INVALID_STATE",
                "repository already has refs",
            ));
        }
        let mut names = std::collections::HashSet::new();
        let mut refs = Vec::new();
        for r in request.refs {
            if !(r.ref_name.starts_with("refs/heads/") || r.ref_name.starts_with("refs/tags/"))
                || validate_ref(&r.ref_name).is_err()
                || !names.insert(r.ref_name.clone())
                || r.object_id.len() != 40
                || !r.object_id.bytes().all(|b| b.is_ascii_hexdigit())
                || r.object_id.bytes().all(|b| b == b'0')
            {
                return Err(rpc_error(
                    Code::InvalidArgument,
                    "INVALID_FIELD",
                    "invalid import ref or object id",
                ));
            }
            refs.push(kajiba_gitstore::Ref {
                name: r.ref_name,
                target: r.object_id,
            });
        }
        if refs.is_empty() {
            return Err(rpc_error(
                Code::InvalidArgument,
                "INVALID_FIELD",
                "import refs are empty",
            ));
        }
        Gix.fetch(&path, &request.source, &request.credentials, &refs)
            .map_err(source_error)
    }

    pub fn quarantine(&self, id: &str) -> Result<PathBuf, Status> {
        self.quarantine_with_sync(id, &mut sync_dir)
    }

    fn quarantine_with_sync(
        &self,
        id: &str,
        sync: &mut impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<PathBuf, Status> {
        let _guard = self.mutation.lock().unwrap();
        let path = self.present(id)?;
        real_dir(&self.root.join("tmp")).map_err(internal)?;
        let orphans = self.root.join("tmp/orphans");
        real_dir(&orphans).map_err(internal)?;
        let name = format!("{id}.git");
        let mut destination = orphans.join(&name);
        let mut suffix = 0_u64;
        loop {
            match fs::symlink_metadata(&destination) {
                Ok(_) => {
                    suffix += 1;
                    destination = orphans.join(format!("{name}.{suffix}"));
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => break,
                Err(err) => return Err(internal(err)),
            }
        }
        fs::rename(&path, &destination).map_err(internal)?;
        sync(&self.root.join("repos")).map_err(internal)?;
        sync(&orphans).map_err(internal)?;
        tracing::warn!(event = "repo-quarantined", path = %path.display(), destination = %destination.display());
        Ok(destination)
    }
}

fn branch_valid(branch: &str) -> Result<(), Status> {
    validate_ref(&format!("refs/heads/{branch}")).map_err(|err| {
        rpc_error(
            Code::InvalidArgument,
            "INVALID_FIELD",
            format!("invalid branch: {err}"),
        )
    })
}

fn internal(err: impl std::fmt::Display) -> Status {
    rpc_error(Code::Internal, "INTERNAL", err.to_string())
}

fn sync_tree(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            fs::File::open(entry.path())?.sync_all()?;
        }
    }
    sync_dir(path)
}

#[cfg(test)]
mod tests;

pub(crate) fn source_error(err: kajiba_gitstore::Error) -> Status {
    rpc_error(
        Code::FailedPrecondition,
        "INVALID_STATE",
        kajiba_gitstore::import::reason(&err.to_string()),
    )
}
