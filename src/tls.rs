use std::{fmt, path::Path, sync::Arc};

use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio_rustls::{TlsAcceptor, rustls};

/// An error building a [`TlsConfig`] (unreadable file, malformed PEM, missing
/// certificate or key, or a key that does not match the certificate).
#[derive(Debug)]
pub struct TlsError {
    message: String,
}

impl TlsError {
    fn new(context: &str, source: impl fmt::Display) -> Self {
        Self {
            message: format!("{context}: {source}"),
        }
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TlsError {}

/// Server-side TLS settings: a certificate chain and its private key.
///
/// TLS 1.2 and 1.3 are enabled with rustls' safe defaults and the `ring`
/// crypto provider; the server speaks HTTP/1.1 only and does not request
/// client certificates.
#[derive(Clone)]
pub struct TlsConfig {
    config: Arc<rustls::ServerConfig>,
}

impl TlsConfig {
    /// Reads a PEM certificate chain and a PEM private key (PKCS#8, PKCS#1 or
    /// SEC1) from files.
    pub fn from_pem_files(cert: impl AsRef<Path>, key: impl AsRef<Path>) -> Result<Self, TlsError> {
        let cert = std::fs::read(cert.as_ref())
            .map_err(|error| TlsError::new("reading the certificate file", error))?;
        let key = std::fs::read(key.as_ref())
            .map_err(|error| TlsError::new("reading the private key file", error))?;
        Self::from_pem(&cert, &key)
    }

    /// Reads the same from in-memory PEM bytes.
    pub fn from_pem(cert: &[u8], key: &[u8]) -> Result<Self, TlsError> {
        let certs = CertificateDer::pem_slice_iter(cert)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| TlsError::new("parsing the certificate PEM", error))?;
        if certs.is_empty() {
            return Err(TlsError::new(
                "parsing the certificate PEM",
                "no certificate found",
            ));
        }
        let key = PrivateKeyDer::from_pem_slice(key)
            .map_err(|error| TlsError::new("parsing the private key PEM", error))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| TlsError::new("selecting TLS protocol versions", error))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|error| TlsError::new("using the certificate and key", error))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            config: Arc::new(config),
        })
    }

    #[allow(dead_code)] // used by `AppRuntime::serve_tls`
    pub(crate) fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(Arc::clone(&self.config))
    }
}

impl fmt::Debug for TlsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TlsConfig").finish_non_exhaustive()
    }
}
