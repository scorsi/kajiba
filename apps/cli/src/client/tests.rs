//! A fake in-process `kajiba.v1` server for the CLI's tests.
#![allow(clippy::result_large_err)]
use super::*;
use kajiba_proto::v1::{
    GetBlobRequest, GetBlobResponse, GetCommitDiffRequest, GetCommitDiffResponse, GetRepoRequest,
    GetRepoResponse, GetTreeRequest, GetTreeResponse, GetViewerRequest, GetViewerResponse,
    ListCommitsRequest, ListCommitsResponse, ListReposRequest, ListReposResponse, LogoutRequest,
    LogoutResponse, Repo,
    auth_service_server::{AuthService, AuthServiceServer},
    get_repo_request,
    repo_service_client::RepoServiceClient,
    repo_service_server::{RepoService, RepoServiceServer},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tonic::{Code, Response};

pub struct TempDir(pub PathBuf);
impl TempDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kj-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

pub const TOKEN: &str = "kjb_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// A refusal as the server builds it: ErrorInfo with the reason and a request id.
pub fn refusal(code: Code, reason: &str, message: &str) -> Status {
    let mut details = tonic_types::ErrorDetails::new();
    details.set_error_info(
        reason,
        "kajiba",
        HashMap::from([("request_id".into(), "01JBREQUEST0000000000000000".into())]),
    );
    Status::with_error_details(code, message, details)
}

#[derive(Default)]
pub struct State {
    /// Served in this order, `page` per ListRepos page; GetRepo finds one by public id,
    /// `owner/name` or a previous name.
    pub repos: Vec<Repo>,
    pub page: usize,
    /// Every call: its method and request metadata.
    pub calls: Vec<(&'static str, MetadataMap)>,
    /// Answered to every call when set.
    pub refusal: Option<Status>,
    pub server_version: Option<&'static str>,
    pub viewer: String,
}

#[derive(Clone, Default)]
pub struct Fake(pub Arc<Mutex<State>>);

impl Fake {
    /// Serves on a loopback port; returns its URL.
    pub async fn serve(&self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let incoming =
            tonic::transport::server::TcpIncoming::from_listener(listener, true, None).unwrap();
        let router = tonic::transport::Server::builder()
            .add_service(RepoServiceServer::new(self.clone()))
            .add_service(AuthServiceServer::new(self.clone()));
        tokio::spawn(router.serve_with_incoming(incoming));
        url
    }

    fn answer<T, R>(
        &self,
        method: &'static str,
        request: tonic::Request<R>,
        reply: impl FnOnce(&State, R) -> Result<T, Status>,
    ) -> Result<Response<T>, Status> {
        let mut state = self.0.lock().unwrap();
        let (metadata, _, message) = request.into_parts();
        state.calls.push((method, metadata));
        let result = match &state.refusal {
            Some(status) => Err(status.clone()),
            None => reply(&state, message),
        };
        let version = state.server_version;
        let with_version = |metadata: &mut MetadataMap| {
            if let Some(version) = version {
                metadata.insert(SERVER_VERSION, version.parse().unwrap());
            }
        };
        match result {
            Ok(message) => {
                let mut response = Response::new(message);
                with_version(response.metadata_mut());
                Ok(response)
            }
            Err(mut status) => {
                with_version(status.metadata_mut());
                Err(status)
            }
        }
    }
}

fn unimplemented<T>() -> Result<Response<T>, Status> {
    Err(Status::unimplemented("fake"))
}

#[tonic::async_trait]
impl RepoService for Fake {
    async fn get_repo(
        &self,
        request: tonic::Request<GetRepoRequest>,
    ) -> Result<Response<GetRepoResponse>, Status> {
        self.answer("GetRepo", request, |state, request| {
            let found = state.repos.iter().find(|repo| match &request.repo {
                Some(get_repo_request::Repo::PublicId(id)) => &repo.public_id == id,
                Some(get_repo_request::Repo::Name(name)) => {
                    format!("{}/{}", repo.owner, repo.name) == *name
                        || repo.previous_names.contains(name)
                }
                None => false,
            });
            match found {
                Some(repo) => Ok(GetRepoResponse {
                    repo: Some(repo.clone()),
                }),
                None => Err(refusal(
                    Code::NotFound,
                    "REPO_NOT_FOUND",
                    "no repository by this id or name",
                )),
            }
        })
    }

    async fn list_repos(
        &self,
        request: tonic::Request<ListReposRequest>,
    ) -> Result<Response<ListReposResponse>, Status> {
        self.answer("ListRepos", request, |state, request| {
            let start = match request.page_token.as_str() {
                "" => 0,
                token => token.parse().unwrap(),
            };
            let end = (start + state.page.max(1)).min(state.repos.len());
            Ok(ListReposResponse {
                repos: state.repos[start..end].to_vec(),
                next_page_token: if end < state.repos.len() {
                    end.to_string()
                } else {
                    String::new()
                },
            })
        })
    }

    async fn get_tree(
        &self,
        _: tonic::Request<GetTreeRequest>,
    ) -> Result<Response<GetTreeResponse>, Status> {
        unimplemented()
    }
    async fn get_blob(
        &self,
        _: tonic::Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        unimplemented()
    }
    async fn list_commits(
        &self,
        _: tonic::Request<ListCommitsRequest>,
    ) -> Result<Response<ListCommitsResponse>, Status> {
        unimplemented()
    }
    async fn get_commit_diff(
        &self,
        _: tonic::Request<GetCommitDiffRequest>,
    ) -> Result<Response<GetCommitDiffResponse>, Status> {
        unimplemented()
    }
}

#[tonic::async_trait]
impl AuthService for Fake {
    async fn logout(
        &self,
        _: tonic::Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        unimplemented()
    }
    async fn get_viewer(
        &self,
        request: tonic::Request<GetViewerRequest>,
    ) -> Result<Response<GetViewerResponse>, Status> {
        self.answer("GetViewer", request, |state, _| {
            Ok(GetViewerResponse {
                user: state.viewer.clone(),
                admin: false,
            })
        })
    }
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

#[tokio::test]
async fn every_call_sends_the_token_and_the_client_version() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let mut repos = RepoServiceClient::new(connect(&url, TOKEN).unwrap());
    for _ in 0..2 {
        answer(repos.list_repos(ListReposRequest::default()).await).unwrap();
    }
    let calls = &fake.0.lock().unwrap().calls;
    assert_eq!(calls.len(), 2);
    for (_, metadata) in calls {
        assert_eq!(
            metadata.get("authorization").unwrap(),
            &format!("Bearer {TOKEN}")
        );
        assert_eq!(metadata.get(CLIENT_VERSION).unwrap(), VERSION);
    }
}

#[tokio::test]
async fn errors_carry_the_reason_and_the_request_id() {
    let fake = Fake::default();
    fake.0.lock().unwrap().refusal = Some(refusal(
        Code::Unauthenticated,
        "CREDENTIAL_EXPIRED",
        "credential expired",
    ));
    let url = fake.serve().await;
    let mut repos = RepoServiceClient::new(connect(&url, TOKEN).unwrap());
    let err = answer(repos.list_repos(ListReposRequest::default()).await).unwrap_err();
    assert_eq!(
        err.to_string(),
        "credential expired (CREDENTIAL_EXPIRED, request 01JBREQUEST0000000000000000)"
    );
    // Without ErrorInfo, the gRPC code stands in for the reason.
    assert_eq!(
        Error::from(Status::unavailable("connection refused")).to_string(),
        "connection refused (Unavailable)"
    );
    assert_eq!(
        Error::from(Status::new(Code::Internal, "")).to_string(),
        "Internal error (Internal)"
    );
}

#[test]
fn token_file_missing_or_unreadable() {
    let dir = TempDir::new();
    let path = dir.0.join("token");
    assert_eq!(
        read_token(&path).unwrap_err().to_string(),
        format!("no token file at {}; run kajiba login", path.display())
    );
    // A directory cannot be read as a file.
    let err = read_token(&dir.0).unwrap_err().to_string();
    assert!(err.starts_with("cannot read token file "), "{err}");
    std::fs::write(&path, format!("{TOKEN}\n")).unwrap();
    assert_eq!(read_token(&path).unwrap(), TOKEN);
    assert!(connect("http://127.0.0.1:1", "kjb_\n").is_err());
}

#[test]
fn token_path_follows_xdg() {
    let env = |name: &str| match name {
        "XDG_CONFIG_HOME" => Some("/xdg".into()),
        "HOME" => Some("/home/alice".into()),
        _ => None,
    };
    assert_eq!(
        token_path(None, &env).unwrap(),
        PathBuf::from("/xdg/kajiba/token")
    );
    let home = |name: &str| (name == "HOME").then(|| "/home/alice".into());
    assert_eq!(
        token_path(None, &home).unwrap(),
        PathBuf::from("/home/alice/.config/kajiba/token")
    );
    assert!(token_path(None, &no_env).is_err());
    assert_eq!(
        token_path(Some(Path::new("/t")), &no_env).unwrap(),
        PathBuf::from("/t")
    );
}

#[test]
fn forge_url_option_then_environment_then_saved() {
    let dir = TempDir::new();
    let token = dir.0.join("token");
    let env = |name: &str| (name == "KAJIBA_URL").then(|| "https://env.example.org".into());
    assert_eq!(
        forge_url(Some("https://option.example.org"), &token, &env).unwrap(),
        "https://option.example.org"
    );
    assert_eq!(
        forge_url(None, &token, &env).unwrap(),
        "https://env.example.org"
    );
    assert_eq!(
        forge_url(None, &token, &no_env).unwrap_err().to_string(),
        "no forge URL: pass --url, set KAJIBA_URL or run kajiba login"
    );
    std::fs::write(url_path(&token), "https://saved.example.org\n").unwrap();
    assert_eq!(
        forge_url(None, &token, &no_env).unwrap(),
        "https://saved.example.org"
    );
}

#[test]
fn another_minor_from_one_point_zero_warns() {
    assert_eq!(warning("0.1.0", "0.2.0"), None);
    assert_eq!(warning("1.2.0", "1.2.7"), None);
    assert_eq!(
        warning("1.2.0", "1.3.0").unwrap(),
        "server version 1.3.0 differs from kajiba 1.2.0"
    );
    assert_eq!(warning("1.2.0", "2.2.0"), None);
    assert_eq!(warning("1.2.0", "garbage"), None);
    assert!(warning("1.2.0-rc.1", "1.3.0+build").is_some());
}

#[tokio::test]
async fn the_server_version_is_read_from_answers_and_refusals() {
    let fake = Fake::default();
    fake.0.lock().unwrap().server_version = Some("9.9.9");
    let url = fake.serve().await;
    let mut repos = RepoServiceClient::new(connect(&url, TOKEN).unwrap());
    let response = repos.list_repos(ListReposRequest::default()).await.unwrap();
    assert_eq!(server_version(response.metadata()).unwrap(), "9.9.9");
    fake.0.lock().unwrap().refusal = Some(Status::not_found("gone"));
    let status = repos
        .list_repos(ListReposRequest::default())
        .await
        .unwrap_err();
    assert_eq!(server_version(status.metadata()).unwrap(), "9.9.9");
}
