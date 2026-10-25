//! Peer-checked host-local gRPC transport shared by both daemons.
use hyper_util::rt::TokioIo;
use std::{
    fs,
    future::Future,
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};
use tonic::{
    codegen::{Service, tokio_stream::Stream},
    service::Routes,
};

/// Readiness of the local listener: serving, and its path still names the socket it bound.
#[derive(Default)]
pub struct Serving {
    pub on: AtomicBool,
    socket: OnceLock<(PathBuf, (u64, u64))>,
}

impl Serving {
    pub fn ready(&self) -> bool {
        self.on.load(Ordering::Acquire)
            && self.socket.get().is_none_or(|(path, identity)| {
                fs::symlink_metadata(path).is_ok_and(|metadata| {
                    metadata.file_type().is_socket()
                        && (metadata.dev(), metadata.ino()) == *identity
                })
            })
    }
}

pub struct Local {
    listener: UnixListener,
    path: PathBuf,
    identity: (u64, u64),
    allowed_uids: Vec<u32>,
    serving: Arc<Serving>,
    backoff: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Local {
    pub fn bind(
        path: &Path,
        serving: Arc<Serving>,
        mode: u32,
        allowed_uids: Vec<u32>,
    ) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                match std::os::unix::net::UnixStream::connect(path) {
                    Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(path)?
                    }
                    Err(err) => return Err(err),
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "local socket is already serving",
                        ));
                    }
                }
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "local socket path is not a socket",
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let listener = UnixListener::bind(path)?;
        let metadata = fs::symlink_metadata(path)?;
        let _ = serving
            .socket
            .set((path.into(), (metadata.dev(), metadata.ino())));
        let local = Self {
            listener,
            path: path.into(),
            identity: (metadata.dev(), metadata.ino()),
            allowed_uids,
            serving,
            backoff: None,
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(local)
    }

    fn allows_uid(&self, uid: u32) -> bool {
        self.allowed_uids.contains(&uid)
    }

    pub async fn serve(self, routes: Routes, shutdown: impl Future<Output = ()>) -> io::Result<()> {
        self.serving.on.store(true, Ordering::Release);
        let serving = self.serving.clone();
        tonic::transport::Server::builder()
            .add_routes(routes)
            .serve_with_incoming_shutdown(self, async move {
                shutdown.await;
                serving.on.store(false, Ordering::Release);
            })
            .await
            .map_err(io::Error::other)
    }
}

impl Stream for Local {
    type Item = io::Result<UnixStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(backoff) = &mut this.backoff {
                ready!(backoff.as_mut().poll(cx));
                this.backoff = None;
            }
            // Never yield Err: tonic would re-poll a still-failing listener at once.
            let stream = match ready!(this.listener.poll_accept(cx)) {
                Ok((stream, _)) => stream,
                Err(err) => {
                    this.backoff =
                        accept_backoff(&err).map(|delay| Box::pin(tokio::time::sleep(delay)));
                    continue;
                }
            };
            // Fail closed before any HTTP/2 or RPC processing, including credential lookup errors.
            if stream
                .peer_cred()
                .is_ok_and(|cred| this.allows_uid(cred.uid()))
            {
                return Poll::Ready(Some(Ok(stream)));
            }
        }
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        self.serving.on.store(false, Ordering::Release);
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone)]
struct Connector(PathBuf);

impl Service<http::Uri> for Connector {
    type Response = TokioIo<UnixStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: http::Uri) -> Self::Future {
        let path = self.0.clone();
        Box::pin(async move { UnixStream::connect(path).await.map(TokioIo::new) })
    }
}

pub async fn channel(
    path: &Path,
    timeout: Option<Duration>,
) -> Result<tonic::transport::Channel, Box<dyn std::error::Error>> {
    let mut endpoint = tonic::transport::Endpoint::from_static("http://localhost")
        .connect_timeout(Duration::from_secs(5));
    if let Some(timeout) = timeout {
        endpoint = endpoint.timeout(timeout);
    }
    Ok(endpoint
        .connect_with_connector(Connector(path.into()))
        .await?)
}

/// Accept errors other than a peer giving up (EMFILE, ENFILE, ENOBUFS) persist:
/// back off instead of spinning on a listener that keeps failing.
pub fn accept_backoff(err: &io::Error) -> Option<Duration> {
    use io::ErrorKind::{ConnectionAborted, ConnectionRefused, ConnectionReset};
    if matches!(
        err.kind(),
        ConnectionAborted | ConnectionRefused | ConnectionReset
    ) {
        return None;
    }
    tracing::warn!(event = "accept-failed", err = %err);
    Some(Duration::from_secs(1))
}

/// UID of this process (the admin socket owner).
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments and cannot fail.
    unsafe { libc::geteuid() }
}

/// Resolve a fixed service identity without falling back to a broader peer policy.
pub fn user_id(name: &std::ffi::CStr) -> io::Result<u32> {
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        // SAFETY: the name is NUL-terminated; entry, found and buffer are writable
        // for this call. Only read entry after a successful, non-null result.
        let code = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if code == libc::ERANGE {
            let size = buffer
                .len()
                .checked_mul(2)
                .ok_or_else(|| io::Error::other("user lookup buffer overflow"))?;
            buffer.resize(size, 0);
            continue;
        }
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code));
        }
        if found.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("system user {} is missing", name.to_string_lossy()),
            ));
        }
        // SAFETY: getpwnam_r succeeded and populated entry.
        return Ok(unsafe { entry.assume_init().pw_uid });
    }
}
/// Structured local RPC errors, including correlation outside the public middleware.
pub fn rpc_error(code: tonic::Code, reason: &str, message: impl Into<String>) -> tonic::Status {
    let metadata: std::collections::HashMap<String, String> = crate::new_id()
        .map(|id| [("request_id".into(), id)].into_iter().collect())
        .unwrap_or_default();
    let mut details = tonic_types::ErrorDetails::new();
    details.set_error_info(reason, "kajiba", metadata);
    <tonic::Status as tonic_types::StatusExt>::with_error_details(code, message, details)
}

pub fn version_error(local: &str, remote: &str) -> Option<tonic::Status> {
    (local != remote).then(|| {
        use tonic_types::StatusExt;
        let err = rpc_error(
            tonic::Code::FailedPrecondition,
            "VERSION_INCOMPATIBLE",
            format!("incompatible versions: local {local}, remote {remote}"),
        );
        let mut details = err.get_error_details();
        let mut metadata = details.error_info().unwrap().metadata.clone();
        metadata.insert("local".into(), local.into());
        metadata.insert("remote".into(), remote.into());
        details.set_error_info("VERSION_INCOMPATIBLE", "kajiba", metadata);
        tonic::Status::with_error_details(err.code(), err.message(), details)
    })
}

pub fn group_id(name: &std::ffi::CStr) -> io::Result<u32> {
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::group>::uninit();
        let mut found = std::ptr::null_mut();
        // SAFETY: name is NUL-terminated and all output storage is writable.
        let code = unsafe {
            libc::getgrnam_r(
                name.as_ptr(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if code == libc::ERANGE {
            let size = buffer
                .len()
                .checked_mul(2)
                .ok_or_else(|| io::Error::other("group lookup buffer overflow"))?;
            buffer.resize(size, 0);
            continue;
        }
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code));
        }
        if found.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("system group {} is missing", name.to_string_lossy()),
            ));
        }
        // SAFETY: getgrnam_r succeeded and populated entry.
        return Ok(unsafe { entry.assume_init().gr_gid });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tonic::codegen::tokio_stream::StreamExt;

    #[tokio::test]
    async fn git_peer_policy_admits_each_service_and_refuses_foreign_uid() {
        let dir = std::env::temp_dir().join(format!("kj-peer-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("local.sock");
        let owner = effective_uid();
        let other = owner.wrapping_add(1);
        let foreign = owner.wrapping_add(2);
        // Inject numeric identities so the test needs neither service accounts nor root.
        for allowed in [vec![owner, other], vec![other, owner], vec![other, foreign]] {
            let accepted = allowed.contains(&owner);
            let mut local = Local::bind(&path, Arc::default(), 0o660, allowed).unwrap();
            assert!(local.allows_uid(other));
            let mut client = UnixStream::connect(&path).await.unwrap();
            if accepted {
                let stream = tokio::time::timeout(Duration::from_secs(2), local.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(stream.peer_cred().unwrap().uid(), owner);
            } else {
                let check_eof = async {
                    let mut byte = [0];
                    assert_eq!(client.read(&mut byte).await.unwrap(), 0);
                };
                tokio::time::timeout(Duration::from_secs(2), async {
                    tokio::select! {
                        _ = check_eof => {},
                        _ = local.next() => panic!("foreign uid was admitted"),
                    }
                })
                .await
                .unwrap();
            }
            drop(local);
            assert!(!path.exists());
        }
        let local = Local::bind(&path, Arc::default(), 0o660, vec![1234, 5678]).unwrap();
        assert!(!local.allows_uid(0));
        drop(local);
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn resolves_system_uid_and_refuses_missing_identity() {
        assert_eq!(user_id(c"root").unwrap(), 0);
        assert_eq!(
            group_id(c"root").or_else(|_| group_id(c"wheel")).unwrap(),
            0
        );
        assert!(group_id(c"kajiba-nonexistent-test-group").is_err());
        assert_eq!(
            user_id(c"kajiba-nonexistent-test-user").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
