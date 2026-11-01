//! The runner's source cache (0106): one bare repository per forge repository under
//! `state_dir/cache`, fed by `FetchRunBundle`, and a scratch checkout per attempt under
//! `state_dir/scratch`, removed when the attempt ends.

use kajiba_core::names::is_public_id;
use kajiba_proto::runner::v1::{
    Assignment, FetchRunBundleRequest, runner_service_client::RunnerServiceClient,
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::OwnedMutexGuard;
use tonic::{Request, transport::Channel};

/// ADR 0106: the cache keeps refs for the 16 most recently fetched commits.
pub const KEPT: usize = 16;
/// Kept commits, most recently fetched first, one id per line.
const RECENT: &str = "kajiba-recent";
const INCOMING: &str = "kajiba-incoming.bundle";
/// The single ref name of every bundle head the server sends (0106).
const BUNDLE_REF: &str = "refs/kajiba/run";

pub struct Cache {
    dir: PathBuf,
    client: RunnerServiceClient<Channel>,
    locks: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// An attempt's sources checked out at its commit. The repository's cache stays locked
/// while it lives; dropping it removes the directory, whatever the attempt's outcome.
pub struct Checkout {
    pub dir: PathBuf,
    _lock: OwnedMutexGuard<()>,
    pub preserved: bool,
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if !self.preserved {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn is_commit_id(id: &str) -> bool {
    id.len() == 40
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `git` without system or user configuration, killed if the attempt is dropped.
async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|err| format!("git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

impl Cache {
    pub fn new(state_dir: &Path, client: RunnerServiceClient<Channel>) -> Self {
        Self {
            dir: state_dir.to_owned(),
            client,
            locks: Mutex::default(),
        }
    }

    /// Bring the assignment's commit into its repository's cache, fetching only what the
    /// cache lacks, and check it out into a fresh scratch directory.
    pub async fn prepare(&self, assignment: &Assignment) -> Result<Checkout, String> {
        let (repo, attempt, sha) = (
            assignment.repo_public_id.as_str(),
            assignment.attempt_id.as_str(),
            assignment.sha.as_str(),
        );
        // The ids name paths and refs.
        if !is_public_id(repo) || !is_public_id(attempt) || !is_commit_id(sha) {
            return Err("assignment: malformed repository, attempt or commit id".into());
        }
        let lock = self
            .locks
            .lock()
            .unwrap()
            .entry(repo.into())
            .or_default()
            .clone();
        let checkout = Checkout {
            dir: self.dir.join("scratch").join(attempt),
            _lock: lock.lock_owned().await,
            preserved: false,
        };
        let cache = self.dir.join("cache").join(format!("{repo}.git"));
        if !cache.join("HEAD").exists() {
            std::fs::create_dir_all(&cache).map_err(|err| format!("cache: {err}"))?;
            git(&cache, &["init", "-q", "--bare"]).await?;
        }
        let kept = format!("refs/kept/{sha}");
        let mut recent: Vec<String> = std::fs::read_to_string(cache.join(RECENT))
            .unwrap_or_default()
            .lines()
            .filter(|id| is_commit_id(id) && *id != sha)
            .map(str::to_owned)
            .collect();
        if git(&cache, &["rev-parse", "-q", "--verify", &kept])
            .await
            .is_err()
        {
            self.fetch(&cache, attempt, sha, &recent).await?;
        }
        recent.insert(0, sha.into());
        recent.truncate(KEPT);
        let incoming = cache.join(format!("{RECENT}.tmp"));
        let mut file = std::fs::File::create(&incoming).map_err(|err| format!("cache: {err}"))?;
        file.write_all(recent.join("\n").as_bytes())
            .and_then(|_| file.sync_all())
            .and_then(|_| std::fs::rename(&incoming, cache.join(RECENT)))
            .and_then(|_| std::fs::File::open(&cache)?.sync_all())
            .map_err(|err| format!("cache: {err}"))?;
        // An interrupted eviction can leave refs that are no longer in RECENT.
        let refs = git(
            &cache,
            &["for-each-ref", "--format=%(refname)", "refs/kept/"],
        )
        .await?;
        let dropped: Vec<_> = refs
            .lines()
            .filter(|name| !recent.iter().any(|id| *name == format!("refs/kept/{id}")))
            .collect();
        for id in &dropped {
            git(&cache, &["update-ref", "-d", id]).await?;
        }
        if !dropped.is_empty() {
            git(&cache, &["gc", "-q", "--prune=now"]).await?;
        }
        std::fs::create_dir_all(&checkout.dir).map_err(|err| format!("scratch: {err}"))?;
        let source = cache.to_str().ok_or("cache: path is not UTF-8")?;
        git(&checkout.dir, &["init", "-q"]).await?;
        git(&checkout.dir, &["fetch", "-q", "--no-tags", source, &kept]).await?;
        git(&checkout.dir, &["checkout", "-q", "--detach", sha]).await?;
        Ok(checkout)
    }

    /// The attempt's bundle, incremental over the `have` commits, under the kept ref.
    async fn fetch(
        &self,
        cache: &Path,
        attempt: &str,
        sha: &str,
        have: &[String],
    ) -> Result<(), String> {
        let request = Request::new(FetchRunBundleRequest {
            attempt_id: attempt.into(),
            have: have.to_vec(),
        });
        let status = |status: tonic::Status| format!("fetch bundle: {}", status.message());
        let mut stream = self
            .client
            .clone()
            .fetch_run_bundle(request)
            .await
            .map_err(status)?
            .into_inner();
        // One file per bundle, in the order received (0097: base commits, then the run's).
        let mut files: Vec<(u32, PathBuf, std::fs::File)> = Vec::new();
        let written = async {
            while let Some(message) = stream.message().await.map_err(status)? {
                if files
                    .last()
                    .is_none_or(|(bundle, ..)| *bundle != message.bundle)
                {
                    if files.iter().any(|(bundle, ..)| *bundle >= message.bundle) {
                        return Err("bundle: out of order".to_owned());
                    }
                    let path = cache.join(format!("{INCOMING}.{}", message.bundle));
                    let file =
                        std::fs::File::create(&path).map_err(|err| format!("bundle: {err}"))?;
                    files.push((message.bundle, path, file));
                }
                let (_, _, file) = files.last_mut().expect("pushed above");
                file.write_all(&message.data)
                    .map_err(|err| format!("bundle: {err}"))?;
            }
            Ok(())
        }
        .await;
        let paths: Vec<PathBuf> = files.into_iter().map(|(_, path, _)| path).collect();
        let kept = format!("refs/kept/{sha}");
        let mut fetched = written;
        for (index, path) in paths.iter().enumerate() {
            if fetched.is_err() {
                break;
            }
            let bundle = path.to_str().ok_or("cache: path is not UTF-8")?;
            // The last bundle holds the commit; earlier ones only its base, kept by it.
            let refspec = if index + 1 == paths.len() {
                format!("{BUNDLE_REF}:{kept}")
            } else {
                BUNDLE_REF.to_owned()
            };
            fetched = git(
                cache,
                &["fetch", "-q", "--no-write-fetch-head", bundle, &refspec],
            )
            .await
            .map(|_| ());
        }
        for path in &paths {
            let _ = std::fs::remove_file(path);
        }
        fetched?;
        if git(cache, &["rev-parse", &kept]).await? != sha {
            git(cache, &["update-ref", "-d", &kept]).await?;
            return Err(format!("bundle: not the commit {sha}"));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;
