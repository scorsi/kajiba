use kajiba_core::{
    config::GitConfig,
    local::{Local, named_user},
};
use std::{ffi::OsStr, io, path::Path};
use tokio::signal::unix::{SignalKind, signal};
use tonic::service::Routes;

use crate::local::SOCKET_NAME;

pub async fn serve(config: GitConfig) -> Result<(), String> {
    tracing::info!(event = "start", version = env!("CARGO_PKG_VERSION"));
    let mut terminate = signal(SignalKind::terminate()).map_err(|err| err.to_string())?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|err| err.to_string())?;
    let peers = [&config.server_user, &config.user]
        .into_iter()
        .map(|name| named_user(name).map(|(uid, _)| uid))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|err| err.to_string())?;
    let notify = std::env::var_os("NOTIFY_SOCKET");
    run(
        &config,
        &config.run_dir.join(SOCKET_NAME),
        peers[1],
        peers,
        notify.as_deref(),
        crate::metrics::bind(config.metrics.listen),
        async move {
            tokio::select! {
                _ = terminate.recv() => {},
                _ = interrupt.recv() => {},
            }
            tracing::info!(event = "stopping");
        },
    )
    .await
}

async fn run(
    config: &GitConfig,
    socket: &Path,
    ssh_uid: u32,
    peers: Vec<u32>,
    notify: Option<&OsStr>,
    metrics: impl std::future::Future<Output = io::Result<Option<tokio::net::TcpListener>>>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<(), String> {
    let state_dir = &config.state_dir;
    let server_socket = &config.server_socket;
    let _lock = crate::state::prepare(state_dir)?;
    let local = Local::bind(socket, Default::default(), 0o660, peers)
        .map_err(|err| format!("{}: {err}", socket.display()))?;
    let ssh = Local::bind(
        &socket.with_file_name("ssh.sock"),
        Default::default(),
        0o600,
        vec![ssh_uid],
    )
    .map_err(|err| format!("ssh.sock: {err}"))?;
    let generation = kajiba_core::new_id().map_err(|err| err.to_string())?;
    let metrics = metrics
        .await
        .map_err(|err| format!("metrics.listen: {err}"))?;
    if let Some(listener) = &metrics {
        tracing::info!(event = "metrics-listening", addr = %listener.local_addr().map_err(|err| err.to_string())?);
    }
    notify_ready(notify).map_err(|err| format!("notify readiness: {err}"))?;
    tracing::info!(event = "ready");
    let repositories =
        std::sync::Arc::new(crate::repositories::Repositories::new(state_dir.to_owned()));
    let git = std::sync::Arc::new(crate::service::Git {
        generation: generation.clone(),
        repositories: repositories.clone(),
        server_socket: server_socket.clone(),
    });
    let routes = routes(git.clone());
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let mut metrics_stopped = stopped.clone();
    let mut ssh_stopped = stopped.clone();
    let app = crate::metrics::router(repositories.clone());
    let servers = async move {
        tokio::try_join!(
            async {
                tokio::select! {
                    result = crate::ssh::serve(ssh, git, &config.hooks_path) => result,
                    _ = ssh_stopped.changed() => Ok(()),
                }
            },
            local.serve(routes, async move {
                let _ = stopped.changed().await;
            }),
            async move {
                if let Some(listener) = metrics {
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = metrics_stopped.changed().await;
                        })
                        .await?;
                }
                Ok(())
            }
        )?;
        Ok::<_, io::Error>(())
    };
    tokio::pin!(servers);
    tokio::select! {
        result = &mut servers => result.map_err(|err| err.to_string()),
        () = shutdown => {
            stop.send_replace(true);
            servers.await.map_err(|err| err.to_string())
        },
        () = crate::service::connect_server(server_socket, &generation, &repositories) => unreachable!("connection loop runs until shutdown"),
    }
}

fn routes(git: std::sync::Arc<crate::service::Git>) -> Routes {
    Routes::new(kajiba_proto::git::v1::git_service_server::GitServiceServer::from_arc(git.clone()))
        .add_service(kajiba_proto::hook::v1::hook_service_server::HookServiceServer::from_arc(git))
        .into_axum_router()
        .layer(axum::middleware::from_fn(rpc_request))
        .into()
}

async fn rpc_request(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    let method = match request.uri().path() {
        "/kajiba.git.v1.GitService/Hello" => "/kajiba.git.v1.GitService/Hello",
        "/kajiba.git.v1.GitService/ListSessions" => "/kajiba.git.v1.GitService/ListSessions",
        "/kajiba.git.v1.GitService/CreateRepository" => {
            "/kajiba.git.v1.GitService/CreateRepository"
        }
        "/kajiba.git.v1.GitService/SetHead" => "/kajiba.git.v1.GitService/SetHead",
        "/kajiba.git.v1.GitService/QuarantineRepository" => {
            "/kajiba.git.v1.GitService/QuarantineRepository"
        }
        "/kajiba.git.v1.GitService/ListRemoteRefs" => "/kajiba.git.v1.GitService/ListRemoteRefs",
        "/kajiba.git.v1.GitService/Fetch" => "/kajiba.git.v1.GitService/Fetch",
        "/kajiba.git.v1.GitService/Spawn" => "/kajiba.git.v1.GitService/Spawn",
        "/kajiba.hook.v1.HookService/GetAuthKey" => "/kajiba.hook.v1.HookService/GetAuthKey",
        "/kajiba.hook.v1.HookService/PreReceive" => "/kajiba.hook.v1.HookService/PreReceive",
        "/kajiba.hook.v1.HookService/PostReceive" => "/kajiba.hook.v1.HookService/PostReceive",
        _ => "other",
    };
    kajiba_core::correlation::request(request, next, method, true, |_, _| {}).await
}

fn notify_ready(path: Option<&OsStr>) -> io::Result<()> {
    use std::os::unix::{ffi::OsStrExt, net::UnixDatagram};
    let Some(path) = path else {
        return Ok(());
    };
    let socket = UnixDatagram::unbound()?;
    if path.as_bytes().starts_with(b"@") {
        #[cfg(target_os = "linux")]
        {
            use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};
            socket.connect_addr(&SocketAddr::from_abstract_name(&path.as_bytes()[1..])?)?;
        }
        #[cfg(not(target_os = "linux"))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "abstract NOTIFY_SOCKET requires Linux",
        ));
    } else {
        socket.connect(path)?;
    }
    socket.send(b"READY=1")?;
    Ok(())
}

#[cfg(test)]
mod tests;
