use kajiba_core::local::{channel, rpc_error, version_error};
use kajiba_proto::git::v1::{HelloRequest, git_service_client::GitServiceClient};
use std::{path::Path, sync::Mutex, time::Duration};
use tonic::{Status, transport::Channel};
use tonic_types::StatusExt;

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

    pub async fn run(&self, path: &Path) {
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
