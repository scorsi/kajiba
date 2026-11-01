use kajiba_core::local::channel;
use kajiba_proto::hook::v1::{
    GetAuthKeyRequest, PostReceiveRequest, PreReceiveRequest, PreReceiveResponse,
    hook_service_client::HookServiceClient,
};
use std::{path::Path, time::Duration};

/// The daemon's socket, in its `run_dir` (ADR 0093).
pub const SOCKET_NAME: &str = "local.sock";
/// Where `ssh-keys` and `hook`, run by sshd and git without a configuration, reach it.
pub const SOCKET_PATH: &str = "/run/kajiba-git/local.sock";
/// Set only on daemon-owned Git children; hooks stay with their session's generation.
pub const SESSION_SOCKET_ENV: &str = "KAJIBA_SESSION_SOCKET";

/// The user public id and canonical key of an active user's auth key; `None` when the
/// fingerprint names no such key.
pub async fn auth_key(
    path: &Path,
    fingerprint: String,
) -> Result<Option<(String, String)>, Box<dyn std::error::Error>> {
    let mut client = HookServiceClient::new(channel(path, Some(Duration::from_secs(5))).await?);
    match client.get_auth_key(GetAuthKeyRequest { fingerprint }).await {
        Ok(response) => {
            let response = response.into_inner();
            Ok(Some((response.user_public_id, response.public_key)))
        }
        Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
        Err(status) => Err(status.into()),
    }
}

/// The daemon enforces the shared verification deadline and kills its Git children (0064).
pub async fn pre_receive(
    path: &Path,
    request: PreReceiveRequest,
) -> Result<PreReceiveResponse, Box<dyn std::error::Error>> {
    let mut client = HookServiceClient::new(channel(path, None).await?);
    Ok(client.pre_receive(request).await?.into_inner())
}

pub async fn post_receive(
    path: &Path,
    request: PostReceiveRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = HookServiceClient::new(channel(path, Some(Duration::from_secs(5))).await?);
    client.post_receive(request).await?;
    Ok(())
}

/// The daemon owns the operation even if this client disconnects.
pub async fn gc(path: &Path) -> std::process::ExitCode {
    use kajiba_proto::git::v1::{GcRequest, git_service_client::GitServiceClient};
    let result = async {
        let mut client = GitServiceClient::new(channel(path, None).await?);
        Ok::<_, Box<dyn std::error::Error>>(client.gc(GcRequest {}).await?.into_inner().exit_status)
    }
    .await;
    match result {
        Ok(status) => {
            if status != 0 {
                kajiba_core::log::error(
                    "gc-failed",
                    &format!("git gc exited with status {status}"),
                );
            }
            std::process::ExitCode::from(u8::try_from(status).unwrap_or(1))
        }
        Err(err) => {
            let message = err
                .downcast_ref::<tonic::Status>()
                .map_or_else(|| err.to_string(), |s| s.message().to_owned());
            kajiba_core::log::error("gc-failed", &message);
            std::process::ExitCode::FAILURE
        }
    }
}
