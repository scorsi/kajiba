//! X.509-SVIDs of the runner channel (0124): loads the SVID, key and bundle files, reloads them
//! when they change (0125) and verifies SPIFFE peers on both sides, using one immutable
//! material snapshot per handshake.

use std::fmt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{ResolvesClientCert, verify_server_cert_signed_by_trust_anchor};
use rustls::crypto::{CryptoProvider, ring};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{ClientHello, ParsedCertificate, ResolvesServerCert, WebPkiClientVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, OtherError,
    RootCertStore, ServerConfig, SignatureScheme,
};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

/// The three files of an identity and its trust domain (0124).
#[derive(Clone, Debug)]
pub struct Files {
    pub trust_domain: String,
    pub svid_file: PathBuf,
    pub key_file: PathBuf,
    pub bundle_file: PathBuf,
}

/// Why material or a peer was refused; its text names the file or the rule.
#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err(message: impl Into<String>) -> Error {
    Error(message.into())
}

/// Checks a trust domain: lowercase letters, digits, `.`, `-`, `_` (0124).
pub fn check_trust_domain(trust_domain: &str) -> Result<(), Error> {
    if trust_domain.is_empty()
        || !trust_domain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(err(
            "trust domain must be lowercase letters, digits, '.', '-' or '_'",
        ));
    }
    Ok(())
}

/// Checks `spiffe://<trust domain>/<host>/<service>` (0124).
pub fn check_id(id: &str, trust_domain: &str) -> Result<(), Error> {
    let shape = || {
        err(format!(
            "{id:?} is not spiffe://<trust domain>/<host>/<service>"
        ))
    };
    let (domain, path) = id
        .strip_prefix("spiffe://")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(shape)?;
    if domain != trust_domain {
        return Err(err(format!(
            "{id:?} is outside trust domain {trust_domain:?}"
        )));
    }
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    match path.split_once('/') {
        Some((host, service)) if segment(host) && segment(service) => Ok(()),
        _ => Err(shape()),
    }
}

/// A verified peer: its SPIFFE ID and when its SVID expires (0124: the stream ends then).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub id: String,
    pub not_after: SystemTime,
}

/// Reads the leaf's SPIFFE checks: not a CA, exactly one URI SAN, an ID in the trust domain.
pub fn peer(leaf: &CertificateDer<'_>, trust_domain: &str) -> Result<Peer, Error> {
    use x509_parser::extensions::GeneralName;
    let (_, cert) = x509_parser::parse_x509_certificate(leaf)
        .map_err(|e| err(format!("certificate does not parse: {e}")))?;
    if cert.is_ca() {
        return Err(err("certificate is a CA"));
    }
    let names = cert
        .subject_alternative_name()
        .map_err(|e| err(format!("subjectAltName: {e}")))?;
    let uris: Vec<&str> = names
        .iter()
        .flat_map(|san| &san.value.general_names)
        .filter_map(|name| match name {
            GeneralName::URI(uri) => Some(*uri),
            _ => None,
        })
        .collect();
    let [id] = uris[..] else {
        return Err(err(format!(
            "certificate has {} URI SANs, want 1",
            uris.len()
        )));
    };
    check_id(id, trust_domain)?;
    let not_after = u64::try_from(cert.validity().not_after.timestamp()).unwrap_or(0);
    Ok(Peer {
        id: id.to_owned(),
        not_after: UNIX_EPOCH + Duration::from_secs(not_after),
    })
}

/// Material in use: the own SVID and key, the bundle, and a client verifier built from it.
#[derive(Debug)]
struct Material {
    key: Arc<CertifiedKey>,
    roots: Arc<RootCertStore>,
    clients: Arc<dyn ClientCertVerifier>,
    not_after: SystemTime,
}

/// Device, inode and modification time of each file; `None` for a file that cannot be read.
type Stamps = [Option<(u64, u64, SystemTime)>; 3];

struct State {
    current: Arc<Material>,
    loaded: Stamps,
    failed: Option<Stamps>,
}

/// An identity reloaded from its files before each handshake (0125).
pub struct Source {
    files: Files,
    provider: Arc<CryptoProvider>,
    state: Mutex<State>,
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("files", &self.files)
            .finish()
    }
}

impl Source {
    /// Loads the files; an unreadable file, a key not matching the SVID, an SVID without a
    /// valid ID in the trust domain or an empty bundle is an error.
    pub fn load(files: Files) -> Result<Arc<Self>, Error> {
        let provider = Arc::new(ring::default_provider());
        let loaded = stamps(&files);
        let current = Arc::new(load(&files, &provider)?);
        Ok(Arc::new(Self {
            files,
            provider,
            state: Mutex::new(State {
                current,
                loaded,
                failed: None,
            }),
        }))
    }

    /// The own SVID's expiry.
    pub fn not_after(&self) -> SystemTime {
        self.lock().current.not_after
    }

    /// The trust domain of this identity and its peers.
    pub fn trust_domain(&self) -> &str {
        &self.files.trust_domain
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current material, reloaded first when a file changed since it was loaded; a failed
    /// load keeps the previous material and is logged once per change of the files.
    fn refresh(&self) -> Arc<Material> {
        let mut state = self.lock();
        let now = stamps(&self.files);
        if now != state.loaded {
            match load(&self.files, &self.provider) {
                Ok(material) => {
                    tracing::info!(
                        event = "svid-reloaded",
                        not_after = %crate::log::ts_from(unix(material.not_after), 0)
                    );
                    state.current = Arc::new(material);
                    state.loaded = now;
                    state.failed = None;
                }
                Err(e) if state.failed != Some(now) => {
                    tracing::error!(event = "svid-reload-failed", err = %e);
                    state.failed = Some(now);
                }
                Err(_) => {}
            }
        }
        state.current.clone()
    }

    /// TLS 1.3, ALPN `h2`, the current SVID served, a client SVID required and verified
    /// against the current bundle (`clientAuth`); the caller maps the peer's ID to a runner.
    /// Build a fresh configuration before each handshake to reload the files (0125).
    pub fn server_config(self: &Arc<Self>) -> ServerConfig {
        let verifier = Verifier::new(self.clone(), None);
        let resolver = Resolver(verifier.material.key.clone());
        let mut config = ServerConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("ring supports TLS 1.3")
            .with_client_cert_verifier(Arc::new(verifier))
            .with_cert_resolver(Arc::new(resolver));
        config.alpn_protocols = vec![b"h2".to_vec()];
        // Every connection must verify the current SVIDs and bundle (0124, 0125).
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        config.send_tls13_tickets = 0;
        config
    }

    /// TLS 1.3, ALPN `h2`, the current SVID presented, the server verified against the current
    /// bundle (`serverAuth`) and its ID required to equal `server_id`; no DNS name is checked.
    /// Build a fresh configuration before each handshake to reload the files (0125).
    pub fn client_config(self: &Arc<Self>, server_id: &str) -> ClientConfig {
        let verifier = Verifier::new(self.clone(), Some(server_id.to_owned()));
        let resolver = Resolver(verifier.material.key.clone());
        let mut config = ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("ring supports TLS 1.3")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_client_cert_resolver(Arc::new(resolver));
        config.alpn_protocols = vec![b"h2".to_vec()];
        config.resumption = rustls::client::Resumption::disabled();
        config
    }
}

fn unix(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn stamps(files: &Files) -> Stamps {
    [&files.svid_file, &files.key_file, &files.bundle_file].map(|path| {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.dev(), meta.ino(), meta.modified().ok()?))
    })
}

fn load(files: &Files, provider: &CryptoProvider) -> Result<Material, Error> {
    let read =
        |path: &PathBuf| std::fs::read(path).map_err(|e| err(format!("{}: {e}", path.display())));
    let pem =
        |path: &PathBuf, e: rustls_pki_types::pem::Error| err(format!("{}: {e}", path.display()));
    let chain = CertificateDer::pem_slice_iter(&read(&files.svid_file)?)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| pem(&files.svid_file, e))?;
    let leaf = chain
        .first()
        .ok_or_else(|| err(format!("{}: no certificate", files.svid_file.display())))?;
    let own = peer(leaf, &files.trust_domain)
        .map_err(|e| err(format!("{}: {e}", files.svid_file.display())))?;
    let key = PrivateKeyDer::from_pem_slice(&read(&files.key_file)?)
        .map_err(|e| pem(&files.key_file, e))?;
    let key = CertifiedKey::from_der(chain, key, provider).map_err(|e| {
        err(format!(
            "{}, {}: {e}",
            files.svid_file.display(),
            files.key_file.display()
        ))
    })?;
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(&read(&files.bundle_file)?) {
        roots
            .add(cert.map_err(|e| pem(&files.bundle_file, e))?)
            .map_err(|e| err(format!("{}: {e}", files.bundle_file.display())))?;
    }
    if roots.is_empty() {
        return Err(err(format!(
            "{}: no certificate",
            files.bundle_file.display()
        )));
    }
    let roots = Arc::new(roots);
    let clients =
        WebPkiClientVerifier::builder_with_provider(roots.clone(), provider.clone().into())
            .build()
            .map_err(|e| err(format!("{}: {e}", files.bundle_file.display())))?;
    Ok(Material {
        key: Arc::new(key),
        roots,
        clients,
        not_after: own.not_after,
    })
}

fn refused(e: Error) -> rustls::Error {
    rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(e))))
}

/// Serves and presents the current SVID.
#[derive(Debug)]
struct Resolver(Arc<CertifiedKey>);

impl ResolvesServerCert for Resolver {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

impl ResolvesClientCert for Resolver {
    fn resolve(&self, _: &[&[u8]], _: &[SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// Verifies a runner (server side, no expected ID) or the forge (client side, its expected ID).
#[derive(Debug)]
struct Verifier {
    source: Arc<Source>,
    material: Arc<Material>,
    server_id: Option<String>,
}

impl Verifier {
    fn new(source: Arc<Source>, server_id: Option<String>) -> Self {
        Self {
            material: source.refresh(),
            source,
            server_id,
        }
    }

    fn peer(&self, leaf: &CertificateDer<'_>) -> Result<Peer, Error> {
        let (_, cert) = x509_parser::parse_x509_certificate(leaf)
            .map_err(|e| err(format!("certificate does not parse: {e}")))?;
        let eku = cert
            .extended_key_usage()
            .map_err(|e| err(format!("extendedKeyUsage: {e}")))?
            .ok_or_else(|| err("certificate has no extendedKeyUsage"))?;
        let allowed = if self.server_id.is_some() {
            eku.value.server_auth
        } else {
            eku.value.client_auth
        };
        if !allowed {
            return Err(err("certificate lacks the peer role's extendedKeyUsage"));
        }
        peer(leaf, self.source.trust_domain())
    }

    fn signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.source.provider.signature_verification_algorithms,
        )
    }
}

impl ClientCertVerifier for Verifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        let material = &self.material;
        let verified = material
            .clients
            .verify_client_cert(end_entity, intermediates, now)?;
        self.peer(end_entity).map_err(refused)?;
        Ok(verified)
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.source
            .provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let material = &self.material;
        verify_server_cert_signed_by_trust_anchor(
            &ParsedCertificate::try_from(end_entity)?,
            &material.roots,
            intermediates,
            now,
            self.source.provider.signature_verification_algorithms.all,
        )?;
        let id = self.peer(end_entity).map_err(refused)?.id;
        let expected = self.server_id.as_deref().unwrap_or_default();
        if id != expected {
            return Err(refused(err(format!("server is {id}, want {expected}"))));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.source
            .provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests;
