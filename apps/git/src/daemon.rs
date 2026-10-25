use kajiba_core::{
    config::GitConfig,
    local::{Local, user_id},
};
use std::{ffi::OsStr, io, path::Path};
use tokio::signal::unix::{SignalKind, signal};
use tonic::service::Routes;

const SOCKET_PATH: &str = "/run/kajiba-git/local.sock";

pub async fn serve(config: GitConfig) -> Result<(), String> {
    tracing::info!(event = "start", version = env!("CARGO_PKG_VERSION"));
    let mut terminate = signal(SignalKind::terminate()).map_err(|err| err.to_string())?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|err| err.to_string())?;
    let peers = [c"kajiba-server", c"kajiba-git"]
        .into_iter()
        .map(user_id)
        .collect::<io::Result<Vec<_>>>()
        .map_err(|err| err.to_string())?;
    let notify = std::env::var_os("NOTIFY_SOCKET");
    run(
        &config.state_dir,
        &config.server_socket,
        Path::new(SOCKET_PATH),
        peers,
        notify.as_deref(),
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
    state_dir: &Path,
    server_socket: &Path,
    socket: &Path,
    peers: Vec<u32>,
    notify: Option<&OsStr>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<(), String> {
    let _lock = crate::state::prepare(state_dir)?;
    let local = Local::bind(socket, Default::default(), 0o660, peers)
        .map_err(|err| format!("{}: {err}", socket.display()))?;
    let generation = kajiba_core::new_id().map_err(|err| err.to_string())?;
    notify_ready(notify).map_err(|err| format!("notify readiness: {err}"))?;
    tracing::info!(event = "ready");
    let routes = Routes::new(
        kajiba_proto::git::v1::git_service_server::GitServiceServer::new(crate::service::Git {
            generation: generation.clone(),
        }),
    );
    tokio::select! {
        result = local.serve(routes, shutdown) => result.map_err(|err| err.to_string()),
        () = crate::service::connect_server(server_socket, &generation) => unreachable!("connection loop runs until shutdown"),
    }
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
