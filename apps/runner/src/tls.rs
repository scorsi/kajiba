//! The runner's connector to the forge's runner listener (0124): TLS 1.3 with the runner's SVID,
//! reloaded before each handshake (0125), SNI the URL's host, ALPN `h2`; the forge is verified
//! by the bundle and `tls.server_id`, never by a DNS name.

use hyper_util::rt::TokioIo;
use kajiba_core::{config::RunnerTlsConfig, svid};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream, rustls::pki_types::ServerName};
use tonic::{codegen::http::Uri, transport::Endpoint};

/// The channel's endpoint and its connector for the runner URL.
///
/// tonic refuses an `https` URI with a custom connector once its own TLS feature is built (the
/// CLI enables it in the workspace), so the origin is `http://` plus the URL's authority and
/// [`Tls`] adds the TLS; the requests' `:scheme` reads `http`, which the forge ignores.
pub fn endpoint(url: &str, tls: &RunnerTlsConfig) -> Result<(Endpoint, Tls), String> {
    let uri: Uri = url.parse().map_err(|err| format!("url: {err}"))?;
    let authority = uri.authority().ok_or("url: missing host")?;
    let endpoint = Endpoint::from_shared(format!("http://{authority}"))
        .map_err(|err| format!("url: {err}"))?;
    let source = svid::Source::load(svid::Files {
        trust_domain: tls.trust_domain.clone(),
        svid_file: tls.svid_file.clone(),
        key_file: tls.key_file.clone(),
        bundle_file: tls.bundle_file.clone(),
    })
    .map_err(|err| format!("tls: {err}"))?;
    Ok((
        endpoint,
        Tls(Arc::new(source.client_config(&tls.server_id))),
    ))
}

/// TCP to the URI's host and port (443 when absent), then TLS with SNI that host.
#[derive(Clone)]
pub struct Tls(Arc<tokio_rustls::rustls::ClientConfig>);

impl tonic::codegen::Service<Uri> for Tls {
    type Response = TokioIo<TlsStream<TcpStream>>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let config = self.0.clone();
        let host = uri
            .host()
            .unwrap_or_default()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let port = uri.port_u16().unwrap_or(443);
        Box::pin(async move {
            let name = ServerName::try_from(host.clone())
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
            let tcp = TcpStream::connect((host.as_str(), port)).await?;
            let tls = TlsConnector::from(config).connect(name, tcp).await?;
            Ok(TokioIo::new(tls))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_rustls::TlsAcceptor;
    use tonic::codegen::Service;

    fn files(leaf: &str) -> RunnerTlsConfig {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/kajiba-core/tests/fixtures/svid");
        RunnerTlsConfig {
            trust_domain: "example.org".into(),
            server_id: "spiffe://example.org/forge/kajiba-server".into(),
            svid_file: dir.join(format!("{leaf}.pem")),
            key_file: dir.join(format!("{leaf}.key")),
            bundle_file: dir.join("ca.pem"),
        }
    }

    /// The forge's side of one handshake: the SNI, the ALPN and the runner's ID it saw.
    async fn forge(
        listener: &tokio::net::TcpListener,
    ) -> std::io::Result<(Option<String>, Option<Vec<u8>>, String)> {
        let forge = files("forge");
        let source = svid::Source::load(svid::Files {
            trust_domain: forge.trust_domain,
            svid_file: forge.svid_file,
            key_file: forge.key_file,
            bundle_file: forge.bundle_file,
        })
        .unwrap();
        let (tcp, _) = listener.accept().await?;
        let tls = TlsAcceptor::from(Arc::new(source.server_config()))
            .accept(tcp)
            .await?;
        let (_, connection) = tls.get_ref();
        let leaf = &connection.peer_certificates().unwrap()[0];
        Ok((
            connection.server_name().map(str::to_owned),
            connection.alpn_protocol().map(<[u8]>::to_vec),
            svid::peer(leaf, "example.org").unwrap().id,
        ))
    }

    #[tokio::test]
    async fn the_connector_presents_the_svid_and_checks_the_forge_id() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("https://localhost:{port}");

        let (endpoint, mut tls) = endpoint(&url, &files("runner")).unwrap();
        assert_eq!(
            endpoint.uri().to_string(),
            format!("http://localhost:{port}/")
        );
        let (client, server) = tokio::join!(tls.call(endpoint.uri().clone()), forge(&listener));
        client.unwrap();
        let (sni, alpn, id) = server.unwrap();
        assert_eq!(sni.as_deref(), Some("localhost"));
        assert_eq!(alpn.as_deref(), Some(&b"h2"[..]));
        assert!(id.ends_with("/kajiba-runner"), "{id}");

        // A forge whose ID is not `tls.server_id` is refused, though its chain verifies.
        let mut wrong = files("runner");
        wrong.server_id = "spiffe://example.org/other/kajiba-server".into();
        let (endpoint, mut tls) = super::endpoint(&url, &wrong).unwrap();
        let (client, _) = tokio::join!(tls.call(endpoint.uri().clone()), forge(&listener));
        assert!(client.is_err());
    }
}
