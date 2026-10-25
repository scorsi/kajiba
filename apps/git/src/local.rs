use kajiba_core::local::channel;
use kajiba_proto::hook::v1::{
    GetAuthKeyRequest, PostReceiveRequest, PreReceiveRequest, PreReceiveResponse,
    hook_service_client::HookServiceClient,
};
use std::{path::Path, time::Duration};

pub const SOCKET_PATH: &str = "/run/kajiba-git/local.sock";

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
