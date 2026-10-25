//! Fixed-cardinality Prometheus text exposition (0022, 0054).
use crate::repositories::Repositories;
use axum::{Router, routing::get};
use kajiba_proto::git::v1::SessionKind;
use std::{fmt::Write, net::SocketAddr, sync::Arc};

pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const KINDS: [(SessionKind, &str); 4] = [
    (SessionKind::ReceivePack, "receive_pack"),
    (SessionKind::UploadPack, "upload_pack"),
    (SessionKind::Import, "import"),
    (SessionKind::Maintenance, "maintenance"),
];

pub fn kind_name(kind: i32) -> &'static str {
    kind_index(kind)
        .map(|i| KINDS[i].1)
        .unwrap_or("unspecified")
}

pub fn kind_index(kind: i32) -> Option<usize> {
    KINDS.iter().position(|(value, _)| *value as i32 == kind)
}

pub fn exposition(version: &str, active: [u64; 4], completed: [[u64; 2]; 4]) -> String {
    let version = version
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let mut out = String::from(
        "# HELP kajiba_git_build_info Running build version.\n# TYPE kajiba_git_build_info gauge\n",
    );
    writeln!(out, "kajiba_git_build_info{{version=\"{version}\"}} 1").unwrap();
    out.push_str("# HELP kajiba_git_sessions_active Active sessions.\n# TYPE kajiba_git_sessions_active gauge\n");
    for ((_, kind), count) in KINDS.iter().zip(active) {
        writeln!(out, "kajiba_git_sessions_active{{kind=\"{kind}\"}} {count}").unwrap();
    }
    out.push_str("# HELP kajiba_git_sessions_total Completed sessions.\n# TYPE kajiba_git_sessions_total counter\n");
    for ((_, kind), counts) in KINDS.iter().zip(completed) {
        for (result, count) in ["ok", "error"].iter().zip(counts) {
            writeln!(
                out,
                "kajiba_git_sessions_total{{kind=\"{kind}\",result=\"{result}\"}} {count}"
            )
            .unwrap();
        }
    }
    out
}

pub fn router(repositories: Arc<Repositories>) -> Router {
    Router::new().route(
        "/metrics",
        get(move || {
            let repositories = repositories.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, CONTENT_TYPE)],
                    repositories.metrics(),
                )
            }
        }),
    )
}

pub async fn bind(listen: Option<SocketAddr>) -> std::io::Result<Option<tokio::net::TcpListener>> {
    match listen {
        Some(addr) => tokio::net::TcpListener::bind(addr).await.map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
