use kajiba_core::local::{channel, rpc_error, version_error};
use kajiba_proto::git::v1::{HelloRequest, git_service_client::GitServiceClient};
use std::{path::Path, sync::Mutex, time::Duration};
use tonic::{Status, transport::Channel};
use tonic_types::StatusExt;

/// Startup mutations connect on demand; a read-only startup needs no daemon.
#[cfg_attr(test, derive(Clone))]
pub struct Repositories {
    socket: std::path::PathBuf,
    pub generation: String,
    client: Option<GitServiceClient<Channel>>,
}

impl Repositories {
    pub fn new(socket: &Path) -> Self {
        Self {
            socket: socket.into(),
            generation: String::new(),
            client: None,
        }
    }

    pub async fn client(&mut self) -> Result<&mut GitServiceClient<Channel>, Status> {
        if self.client.is_none() {
            self.client = Some(GitServiceClient::new(
                channel(&self.socket, None).await.map_err(unavailable)?,
            ));
        }
        let client = self.client.as_mut().unwrap();
        let mut request = tonic::Request::new(HelloRequest {
            version: env!("CARGO_PKG_VERSION").into(),
        });
        request.set_timeout(Duration::from_secs(5));
        let hello = client
            .hello(request)
            .await
            .map_err(unavailable)?
            .into_inner();
        if let Some(err) = version_error(env!("CARGO_PKG_VERSION"), &hello.version) {
            return Err(unavailable(err));
        }
        if !kajiba_core::names::is_public_id(&hello.generation) {
            return Err(unavailable("invalid git-side generation"));
        }
        self.generation = hello.generation;
        Ok(client)
    }
}

pub fn unavailable(err: impl std::fmt::Display) -> Status {
    rpc_error(
        tonic::Code::Unavailable,
        "GIT_SIDE_UNAVAILABLE",
        format!("GIT_SIDE_UNAVAILABLE: {err}"),
    )
}

pub fn mutation_error(err: Status) -> String {
    if matches!(
        err.code(),
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded
    ) {
        unavailable(err).to_string()
    } else {
        err.to_string()
    }
}

#[derive(Default)]
pub struct Link {
    client: Mutex<Option<GitServiceClient<Channel>>>,
}

impl Link {
    pub fn ready(&self) -> bool {
        self.client.lock().unwrap().is_some()
    }

    #[allow(clippy::result_large_err)]
    pub fn client(&self) -> Result<GitServiceClient<Channel>, Status> {
        self.client.lock().unwrap().clone().ok_or_else(|| {
            rpc_error(
                tonic::Code::Unavailable,
                "GIT_SIDE_UNAVAILABLE",
                "the git side is unavailable",
            )
        })
    }

    pub async fn run(&self, path: &Path, recovery: Option<&super::local::Admin>) {
        struct Clear<'a>(&'a Link);
        impl Drop for Clear<'_> {
            fn drop(&mut self) {
                self.0.client.lock().unwrap().take();
            }
        }
        let _clear = Clear(self);
        let mut client = self.client().ok();
        let mut generation = None;
        let mut unavailable = false;
        let mut refused_version = None;
        let mut delay = Duration::from_millis(100);
        loop {
            let result = async {
                if client.is_none() {
                    client = Some(GitServiceClient::new(
                        channel(path, Some(Duration::from_secs(5)))
                            .await
                            .map_err(|err| {
                                rpc_error(
                                    tonic::Code::Unavailable,
                                    "GIT_SIDE_UNAVAILABLE",
                                    err.to_string(),
                                )
                            })?,
                    ));
                }
                let hello = client
                    .as_mut()
                    .unwrap()
                    .hello(HelloRequest {
                        version: env!("CARGO_PKG_VERSION").into(),
                    })
                    .await?
                    .into_inner();
                if let Some(err) = version_error(env!("CARGO_PKG_VERSION"), &hello.version) {
                    return Err(err);
                }
                if !kajiba_core::names::is_public_id(&hello.generation) {
                    return Err(rpc_error(
                        tonic::Code::Unavailable,
                        "GIT_SIDE_UNAVAILABLE",
                        "invalid git-side generation",
                    ));
                }
                Ok(hello)
            }
            .await;
            match result {
                Ok(hello) => {
                    if generation.as_ref() != Some(&hello.generation) || unavailable {
                        if let Some(admin) = recovery {
                            let _completion = admin.maintenance.completion().await;
                            let mut writer = admin.writer.lock().await;
                            let result = snapshot(
                                client.as_mut().unwrap(),
                                &admin.gate,
                                &mut writer,
                                &hello.generation,
                            )
                            .await;
                            if let Err(err) = result {
                                self.client.lock().unwrap().take();
                                client = None;
                                unavailable = true;
                                tracing::warn!(event = "git-side-unavailable", err = %err);
                                drop(writer);
                                delay = (delay * 2).min(Duration::from_secs(5));
                                tokio::time::sleep(delay).await;
                                continue;
                            }
                            admin
                                .gate
                                .recover_once(
                                    &mut writer,
                                    &admin.bus,
                                    &admin.state_dir,
                                    &kajiba_gitstore::Gix::shared(&admin.state_dir.join("repos")),
                                )
                                .await;
                        }
                        tracing::info!(
                            event = "git-side-connected",
                            version = hello.version,
                            generation = hello.generation
                        );
                    }
                    generation = Some(hello.generation);
                    *self.client.lock().unwrap() = client.clone();
                    unavailable = false;
                    refused_version = None;
                    delay = Duration::from_secs(1);
                }
                Err(err) => {
                    self.client.lock().unwrap().take();
                    client = None;
                    let details = err.get_error_details();
                    if let Some(info) = details
                        .error_info()
                        .filter(|info| info.reason == "VERSION_INCOMPATIBLE")
                    {
                        let remote = if info.metadata.get("local").map(String::as_str)
                            == Some(env!("CARGO_PKG_VERSION"))
                        {
                            info.metadata.get("remote")
                        } else {
                            info.metadata.get("local")
                        }
                        .cloned()
                        .unwrap_or_else(|| "unknown".into());
                        if refused_version.as_ref() != Some(&remote) {
                            tracing::error!(
                                event = "git-side-version-refused",
                                local = env!("CARGO_PKG_VERSION"),
                                remote
                            );
                        }
                        refused_version = Some(remote);
                    }
                    if !unavailable {
                        tracing::warn!(event = "git-side-unavailable", err = %err);
                    }
                    unavailable = true;
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
            }
            tokio::time::sleep(delay).await;
        }
    }
}

#[cfg(test)]
pub(super) mod tests;

#[cfg(test)]
pub(crate) mod fake;

/// The writer lock excludes OpenSession and SessionExited while rebuilding the gate.
pub async fn snapshot(
    client: &mut GitServiceClient<Channel>,
    gate: &crate::db::pushes::Gate,
    writer: &mut sqlx::SqliteConnection,
    generation: &str,
) -> Result<(), Status> {
    let mut request = tonic::Request::new(kajiba_proto::git::v1::ListSessionsRequest {});
    request.set_timeout(Duration::from_secs(5));
    let snapshot = client.list_sessions(request).await?.into_inner();
    if !kajiba_core::names::is_public_id(&snapshot.generation) || snapshot.generation != generation
    {
        return Err(unavailable("invalid git-side generation"));
    }
    gate.rebuild(writer, &snapshot.generation, snapshot.sessions)
        .await
        .map_err(unavailable)
}
