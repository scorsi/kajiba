#![allow(clippy::result_large_err)]
use super::*;
use crate::session::Identity;
use kajiba_gitstore::GitStore;
use kajiba_proto::runner::v1::{
    Capabilities, ConnectRequest, ConnectResponse, FetchRunBundleResponse,
    runner_service_server::{RunnerService, RunnerServiceServer},
};
use std::{pin::Pin, time::Duration};
use tokio_stream::Stream;
use tonic::{Response, Status, Streaming, transport::Endpoint};

type Bundle = Pin<Box<dyn Stream<Item = Result<FetchRunBundleResponse, Status>> + Send>>;

/// Serves the bundles of `source` the forge would: each attempt's commit, minus the
/// `have` commits; `corrupt` attempts get bytes that are no bundle.
#[derive(Clone, Default)]
struct Fake {
    source: PathBuf,
    attempts: Arc<Mutex<BTreeMap<String, String>>>,
    corrupt: Arc<Mutex<Vec<String>>>,
    requests: Arc<Mutex<Vec<FetchRunBundleRequest>>>,
}

#[tonic::async_trait]
impl RunnerService for Fake {
    type ConnectStream = Pin<Box<dyn Stream<Item = Result<ConnectResponse, Status>> + Send>>;
    type FetchRunBundleStream = Bundle;

    async fn connect(
        &self,
        _: Request<Streaming<ConnectRequest>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        Err(Status::unimplemented("not faked"))
    }

    async fn fetch_run_bundle(
        &self,
        request: Request<FetchRunBundleRequest>,
    ) -> Result<Response<Bundle>, Status> {
        assert_eq!(
            request.metadata().get("authorization").unwrap(),
            "Bearer s3cret"
        );
        let request = request.into_inner();
        self.requests.lock().unwrap().push(request.clone());
        let mut data = Vec::new();
        if self.corrupt.lock().unwrap().contains(&request.attempt_id) {
            data.extend(b"# v2 git bundle\nnot a pack");
        } else {
            let sha = self.attempts.lock().unwrap()[&request.attempt_id].clone();
            kajiba_gitstore::Gix
                .bundle(&self.source, &sha, &request.have, &mut data)
                .unwrap();
        }
        let chunks: Vec<_> = data
            .chunks(1000)
            .map(|data| {
                Ok(FetchRunBundleResponse {
                    data: data.to_vec(),
                })
            })
            .collect();
        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }
}

fn run(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.org")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.org")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub(crate) struct Setup {
    pub(crate) dir: PathBuf,
    fake: Fake,
    pub(crate) cache: Arc<Cache>,
    /// Commit `n` sets `file` to `n`, each a child of the previous one.
    commits: Vec<String>,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const REPO: &str = "00000000000000000000000001";

pub(crate) async fn setup(name: &str, commits: usize) -> Setup {
    let dir =
        std::env::temp_dir().join(format!("kajiba-runner-cache-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let source = dir.join("source");
    std::fs::create_dir_all(&source).unwrap();
    run(&source, &["init", "-q"]);
    let commits = (1..=commits)
        .map(|n| {
            std::fs::write(source.join("file"), n.to_string()).unwrap();
            run(&source, &["add", "file"]);
            run(&source, &["commit", "-q", "-m", &n.to_string()]);
            run(&source, &["rev-parse", "HEAD"])
        })
        .collect();
    let fake = Fake {
        source,
        ..Fake::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming =
        tonic::transport::server::TcpIncoming::from_listener(listener, true, None).unwrap();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(RunnerServiceServer::new(fake.clone()))
            .serve_with_incoming(incoming),
    );
    let channel = Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_lazy();
    let identity = Identity {
        name: "build-1".into(),
        hostname: "builder.example.org".into(),
        capabilities: Capabilities::default(),
        secret: "s3cret".into(),
    };
    let cache = Arc::new(Cache::new(
        &dir.join("state"),
        RunnerServiceClient::new(channel),
        identity,
    ));
    Setup {
        dir,
        fake,
        cache,
        commits,
    }
}

impl Setup {
    /// Attempt `0…0<n>` of `REPO` at commit `index` (0-based).
    pub(crate) fn assignment(&self, n: usize, index: usize) -> Assignment {
        let attempt = format!("{n:0>26}");
        self.fake
            .attempts
            .lock()
            .unwrap()
            .insert(attempt.clone(), self.commits[index].clone());
        Assignment {
            attempt_id: attempt,
            repo_public_id: REPO.into(),
            sha: self.commits[index].clone(),
            system: "x86_64-linux".into(),
        }
    }

    fn haves(&self) -> Vec<Vec<String>> {
        self.fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.have.clone())
            .collect()
    }

    fn cached(&self) -> PathBuf {
        self.dir.join(format!("state/cache/{REPO}.git"))
    }
}

#[tokio::test]
async fn the_first_fetch_is_full_and_later_ones_incremental() {
    let setup = setup("incremental", 2).await;
    let checkout = setup.cache.prepare(&setup.assignment(1, 0)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(checkout.dir.join("file")).unwrap(),
        "1"
    );
    assert_eq!(run(&checkout.dir, &["rev-parse", "HEAD"]), setup.commits[0]);
    let scratch = checkout.dir.clone();
    drop(checkout);
    assert!(!scratch.exists());
    let checkout = setup.cache.prepare(&setup.assignment(2, 1)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(checkout.dir.join("file")).unwrap(),
        "2"
    );
    drop(checkout);
    // A commit already cached needs no transfer.
    drop(setup.cache.prepare(&setup.assignment(3, 0)).await.unwrap());
    assert_eq!(setup.haves(), [vec![], vec![setup.commits[0].clone()]]);
}

#[tokio::test]
async fn attempts_on_one_repository_are_serialized() {
    let setup = setup("serialized", 1).await;
    let first = setup.cache.prepare(&setup.assignment(1, 0)).await.unwrap();
    let (cache, assignment) = (setup.cache.clone(), setup.assignment(2, 0));
    let second = tokio::spawn(async move {
        cache
            .prepare(&assignment)
            .await
            .map(|checkout| checkout.dir.clone())
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!second.is_finished());
    drop(first);
    let scratch = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // The second checkout ended with its task.
    assert!(!scratch.exists());
}

#[tokio::test]
async fn the_scratch_is_removed_when_a_cancelled_attempt_is_dropped() {
    let setup = setup("cancelled", 1).await;
    let (cache, assignment) = (setup.cache.clone(), setup.assignment(1, 0));
    let (prepared, ready) = tokio::sync::oneshot::channel();
    let attempt = tokio::spawn(async move {
        let checkout = cache.prepare(&assignment).await.unwrap();
        let _ = prepared.send(checkout.dir.clone());
        std::future::pending::<()>().await;
    });
    let scratch = ready.await.unwrap();
    assert!(scratch.join("file").exists());
    attempt.abort();
    let _ = attempt.await;
    assert!(!scratch.exists());
    assert!(
        !setup
            .dir
            .join("state/scratch")
            .read_dir()
            .unwrap()
            .any(|_| true)
    );
}

#[tokio::test]
async fn a_corrupt_bundle_fails_the_attempt_and_leaves_the_cache_usable() {
    let setup = setup("corrupt", 2).await;
    drop(setup.cache.prepare(&setup.assignment(1, 0)).await.unwrap());
    let corrupt = setup.assignment(2, 1);
    setup
        .fake
        .corrupt
        .lock()
        .unwrap()
        .push(corrupt.attempt_id.clone());
    assert!(setup.cache.prepare(&corrupt).await.is_err());
    assert!(
        !setup
            .dir
            .join("state/scratch")
            .join(&corrupt.attempt_id)
            .exists()
    );
    let checkout = setup.cache.prepare(&setup.assignment(3, 1)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(checkout.dir.join("file")).unwrap(),
        "2"
    );
    let first = setup.commits[0].clone();
    assert_eq!(setup.haves(), [vec![], vec![first.clone()], vec![first]]);
}

#[tokio::test]
async fn the_cache_keeps_the_sixteen_most_recent_commits() {
    let setup = setup("bounded", KEPT + 1).await;
    for index in 0..=KEPT {
        drop(
            setup
                .cache
                .prepare(&setup.assignment(index + 1, index))
                .await
                .unwrap(),
        );
    }
    let kept = run(
        &setup.cached(),
        &["for-each-ref", "--format=%(objectname)", "refs/kept/"],
    );
    let mut expected: Vec<&str> = setup.commits[1..].iter().map(String::as_str).collect();
    expected.sort();
    assert_eq!(kept.lines().collect::<Vec<_>>(), expected);
    assert_eq!(setup.haves().last().unwrap().len(), KEPT);
    // The commit dropped last is fetched again, incrementally.
    drop(
        setup
            .cache
            .prepare(&setup.assignment(KEPT + 2, 0))
            .await
            .unwrap(),
    );
    assert_eq!(setup.haves().last().unwrap()[0], setup.commits[KEPT]);
}

#[tokio::test]
async fn malformed_assignments_touch_no_path() {
    let setup = setup("malformed", 1).await;
    let mut assignment = setup.assignment(1, 0);
    assignment.repo_public_id = "../escape".into();
    assert!(setup.cache.prepare(&assignment).await.is_err());
    assert!(!setup.dir.join("state").exists());
}
