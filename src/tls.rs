use std::{fmt, path::Path, sync::Arc};

use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio_rustls::{TlsAcceptor, rustls};

use crate::runtime::{
    Accepted, Activity, ActivityGuard, Tracked, accept_next, connection_limit, drive,
    http1_builder, report_connection_error,
};
#[cfg(feature = "http2")]
use crate::runtime::{drive_h2, http2_builder};
use crate::*;

/// An error building a [`TlsConfig`] (unreadable file, malformed PEM, missing
/// certificate or key, or a key that does not match the certificate).
#[derive(Debug)]
pub struct TlsError {
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl TlsError {
    /// An error without an underlying cause.
    fn new(context: &str, detail: impl fmt::Display) -> Self {
        Self {
            message: format!("{context}: {detail}"),
            source: None,
        }
    }

    /// An error that keeps its underlying cause, available through
    /// [`std::error::Error::source`] (for example an `io::Error` for a
    /// missing file).
    fn with_source(context: &str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message: format!("{context}: {source}"),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TlsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Server-side TLS settings: a certificate chain and its private key.
///
/// TLS 1.2 and 1.3 are enabled with rustls' safe defaults and the `ring`
/// crypto provider; the server speaks HTTP/1.1, plus HTTP/2 when the `http2`
/// feature is enabled (see `enable_http2`), and does not request client
/// certificates.
#[derive(Clone)]
pub struct TlsConfig {
    config: Arc<rustls::ServerConfig>,
}

/// The ALPN protocols offered to clients: `h2` first when HTTP/2 is enabled
/// (only possible with the `http2` feature), then `http/1.1`.
fn alpn_protocols(http2: bool) -> Vec<Vec<u8>> {
    #[cfg(feature = "http2")]
    if http2 {
        return vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    let _ = http2;
    vec![b"http/1.1".to_vec()]
}

impl TlsConfig {
    /// Reads a PEM certificate chain and a PEM private key (PKCS#8, PKCS#1 or
    /// SEC1) from files.
    pub fn from_pem_files(cert: impl AsRef<Path>, key: impl AsRef<Path>) -> Result<Self, TlsError> {
        let cert = std::fs::read(cert.as_ref())
            .map_err(|error| TlsError::with_source("reading the certificate file", error))?;
        let key = std::fs::read(key.as_ref())
            .map_err(|error| TlsError::with_source("reading the private key file", error))?;
        Self::from_pem(&cert, &key)
    }

    /// Reads the same from in-memory PEM bytes.
    pub fn from_pem(cert: &[u8], key: &[u8]) -> Result<Self, TlsError> {
        let certs = CertificateDer::pem_slice_iter(cert)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| TlsError::with_source("parsing the certificate PEM", error))?;
        if certs.is_empty() {
            return Err(TlsError::new(
                "parsing the certificate PEM",
                "no certificate found",
            ));
        }
        let key = PrivateKeyDer::from_pem_slice(key)
            .map_err(|error| TlsError::with_source("parsing the private key PEM", error))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| TlsError::with_source("selecting TLS protocol versions", error))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|error| TlsError::with_source("using the certificate and key", error))?;
        config.alpn_protocols = alpn_protocols(cfg!(feature = "http2"));
        Ok(Self {
            config: Arc::new(config),
        })
    }

    /// Offer HTTP/2 (`h2`) next to `http/1.1` through ALPN. On by default when
    /// the `http2` feature is enabled; pass `false` to serve HTTP/1.1 only.
    #[cfg(feature = "http2")]
    pub fn enable_http2(mut self, enabled: bool) -> Self {
        let mut config = (*self.config).clone();
        config.alpn_protocols = alpn_protocols(enabled);
        self.config = Arc::new(config);
        self
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

    /// Caps the streams a single HTTP/2 connection may have open at once
    /// (default: Hyper's 200). A stream opened over the limit is refused
    /// (`REFUSED_STREAM`), which well-behaved clients retry. Each
    /// open stream can buffer up to its route's body limit, so lowering this
    /// bounds the memory one client can pin.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is `Some(0)`.
    #[cfg(feature = "http2")]
    pub fn http2_max_concurrent_streams(mut self, limit: Option<u32>) -> Self {
        assert!(
            limit != Some(0),
            "http2_max_concurrent_streams must be at least 1"
        );
        self.http2_max_concurrent_streams = limit;
        self
    }

    /// Serves HTTPS on `listener` until `shutdown` completes, then waits for
    /// in-flight requests like [`AppRuntime::serve_listener`], bounded by
    /// [`AppRuntime::shutdown_timeout`]. HTTP/1.1, plus HTTP/2 when the
    /// `http2` feature is enabled and the client negotiates `h2` through ALPN
    /// (see [`TlsConfig::enable_http2`]).
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
        let header_read_timeout = self.header_read_timeout;
        let limit = connection_limit(self.max_connections);
        let connect_info = self.connect_info;
        let observer = self.connection_error_observer;
        #[cfg(feature = "http2")]
        let max_streams = self.http2_max_concurrent_streams;
        let runtime = self.inner;
        let acceptor = tls.acceptor();
        // `GracefulShutdown::watch` needs an already-built connection, which
        // does not exist yet during the handshake, so connections are tracked
        // with a shutdown signal plus a completion channel instead.
        let (signal, _) = tokio::sync::watch::channel(());
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);
        tokio::pin!(shutdown);
        while let Some(Accepted {
            io: stream,
            peer,
            slot,
        }) = accept_next(&listener, &mut shutdown, nodelay, limit.as_ref()).await?
        {
            let observer = observer.clone();
            let acceptor = acceptor.clone();
            let slot = slot.map(Arc::new);
            let connection =
                ConnectionRuntime::new(Arc::clone(&runtime), peer.filter(|_| connect_info))
                    .with_permit(slot.clone());
            let mut stop = signal.subscribe();
            let done = done_tx.clone();
            tokio::spawn(async move {
                // Dropped when the task ends; shutdown waits for every sender.
                let _done = done;
                let _slot = slot;
                let handshake = tokio::time::timeout(handshake_timeout, acceptor.accept(stream));
                let tls_stream = tokio::select! {
                    result = handshake => match result {
                        Ok(Ok(tls_stream)) => tls_stream,
                        // Failed or timed-out handshake: drop only this connection.
                        Ok(Err(error)) => {
                            report_connection_error(&observer, &error);
                            return;
                        }
                        Err(_) => {
                            let error = std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "TLS handshake timed out",
                            );
                            report_connection_error(&observer, &error);
                            return;
                        }
                    },
                    // Shutting down during the handshake.
                    _ = stop.changed() => return,
                };
                // The ALPN result picks the protocol; anything but `h2` is HTTP/1.1.
                #[cfg(feature = "http2")]
                let negotiated_h2 = tls_stream.get_ref().1.alpn_protocol() == Some(b"h2");
                #[cfg(feature = "http2")]
                let activity = negotiated_h2.then(Activity::new);
                #[cfg(not(feature = "http2"))]
                let activity: Option<Arc<Activity>> = None;
                let io = hyper_util::rt::TokioIo::new(Tracked {
                    inner: tls_stream,
                    activity: activity.clone(),
                });
                let service = {
                    let activity = activity.clone();
                    hyper::service::service_fn(move |request: Request<Incoming>| {
                        let guard = activity.as_ref().map(ActivityGuard::start);
                        let prepared = connection.prepare(request);
                        async move {
                            let response = prepared.await;
                            Ok::<_, Infallible>(ActivityGuard::finish_with(guard, response))
                        }
                    })
                };
                #[cfg(feature = "http2")]
                if negotiated_h2 {
                    let builder = http2_builder(max_streams);
                    let activity = activity.unwrap_or_else(Activity::new);
                    let result = drive_h2(
                        builder.serve_connection(io, service),
                        &mut stop,
                        &activity,
                        header_read_timeout,
                        |conn| conn.graceful_shutdown(),
                    )
                    .await;
                    if let Err(error) = result {
                        report_connection_error(&observer, &error);
                    }
                    return;
                }
                let conn = http1_builder(header_read_timeout).serve_connection(io, service);
                #[cfg(feature = "websocket")]
                let conn = conn.with_upgrades();
                let result = drive(conn, &mut stop, |conn| conn.graceful_shutdown()).await;
                if let Err(error) = result {
                    report_connection_error(&observer, &error);
                }
            });
        }
        let _ = signal.send(());
        drop(done_tx);
        let _ = tokio::time::timeout(shutdown_timeout, done_rx.recv()).await;
        Ok(())
    }
}
