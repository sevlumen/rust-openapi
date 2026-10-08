use std::{fmt, path::Path, sync::Arc};

use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio_rustls::{TlsAcceptor, rustls};

use crate::runtime::accept_next;
use crate::*;

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

    pub(crate) fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(Arc::clone(&self.config))
    }
}

impl fmt::Debug for TlsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TlsConfig").finish_non_exhaustive()
    }
}

impl<S: Send + Sync + 'static> AppRuntime<S> {
    /// Sets how long a client may take to finish the TLS handshake before the
    /// connection is dropped (default [`DEFAULT_HANDSHAKE_TIMEOUT`]).
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Serves HTTPS on `listener` until `shutdown` completes, then waits for
    /// in-flight requests like [`AppRuntime::serve_listener`], bounded by
    /// [`AppRuntime::shutdown_timeout`]. HTTP/1.1 only.
    ///
    /// The handshake runs inside the per-connection task, so a slow client
    /// never blocks the accept loop; a failed or timed-out handshake closes
    /// only that connection.
    pub async fn serve_tls<F>(
        self,
        listener: tokio::net::TcpListener,
        tls: TlsConfig,
        shutdown: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let shutdown_timeout = self.shutdown_timeout;
        let handshake_timeout = self.handshake_timeout;
        let nodelay = self.tcp_nodelay;
        let runtime = self.inner;
        let acceptor = tls.acceptor();
        // `GracefulShutdown::watch` needs an already-built connection, which
        // does not exist yet during the handshake, so connections are tracked
        // with a shutdown signal plus a completion channel instead.
        let (signal, _) = tokio::sync::watch::channel(());
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);
        tokio::pin!(shutdown);
        while let Some(stream) = accept_next(&listener, &mut shutdown, nodelay).await? {
            let acceptor = acceptor.clone();
            let connection = ConnectionRuntime::new(Arc::clone(&runtime));
            let mut stop = signal.subscribe();
            let done = done_tx.clone();
            tokio::spawn(async move {
                // Dropped when the task ends; shutdown waits for every sender.
                let _done = done;
                let handshake = tokio::time::timeout(handshake_timeout, acceptor.accept(stream));
                let tls_stream = tokio::select! {
                    result = handshake => match result {
                        Ok(Ok(tls_stream)) => tls_stream,
                        // Failed or timed-out handshake: drop only this connection.
                        _ => return,
                    },
                    // Shutting down during the handshake.
                    _ = stop.changed() => return,
                };
                let io = hyper_util::rt::TokioIo::new(tls_stream);
                let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                    let prepared = connection.prepare(request);
                    async move { Ok::<_, Infallible>(prepared.await) }
                });
                let conn = hyper::server::conn::http1::Builder::new().serve_connection(io, service);
                tokio::pin!(conn);
                tokio::select! {
                    _ = conn.as_mut() => return,
                    _ = stop.changed() => conn.as_mut().graceful_shutdown(),
                }
                let _ = conn.await;
            });
        }
        let _ = signal.send(());
        drop(done_tx);
        let _ = tokio::time::timeout(shutdown_timeout, done_rx.recv()).await;
        Ok(())
    }
}
