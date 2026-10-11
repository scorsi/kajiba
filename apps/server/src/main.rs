use axum::{Router, routing::get};
use clap::Parser;

/// kajiba-server — the forge's HTTP/gRPC server. Empty shell until J1.
#[derive(Parser)]
#[command(version)]
struct Args {}

#[tokio::main]
async fn main() {
    Args::parse();

    let app = Router::new().route("/healthz", get(|| async { "OK" }));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("bind 0.0.0.0:8080");
    axum::serve(listener, app).await.expect("serve");
}
