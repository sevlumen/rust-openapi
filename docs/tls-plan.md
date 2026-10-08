# TLS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `AppRuntime::serve_tls` (feature `tls`) with PEM-based `TlsConfig`, a handshake timeout, and graceful shutdown that matches `serve_listener`.

**Architecture:** The accept loop (shutdown select, accept-error classification, backoff) is extracted into `accept_next` and shared. The plain path keeps hyper-util's `GracefulShutdown`. `GracefulShutdown::watch` needs an already-built connection, so the TLS path (which must handshake inside the per-connection task) tracks connections with a `tokio::sync::watch` shutdown signal plus an `mpsc` completion channel.

**Tech Stack:** Rust 1.88, `tokio-rustls` 0.26 (feature `ring`, `tls12`), `rustls-pki-types` 1 (PEM), dev: `rcgen` 0.14 (ring backend), Hyper 1, Tokio.

**Spec:** `docs/tls-design.md`

## Global Constraints

- No new dependency in the default feature set; everything TLS is behind feature `tls` (optional deps `tokio-rustls`, `rustls-pki-types`).
- `tokio-rustls` is used with `default-features = false, features = ["ring", "tls12"]` (no `aws-lc-rs`; note `ring` itself compiles a little C, so a C compiler is still needed at build time). `rcgen` (dev-dependency) must use `default-features = false, features = ["crypto", "ring", "pem"]` so dev builds do not pull `aws-lc-rs`.
- rustls/ring types never appear in the public API (`TlsConfig`, `TlsError` wrap them).
- MSRV 1.88. `cargo deny check` must pass with default features and `--all-features` (add the minimal `deny.toml` license allowances the new tree needs, each with a comment).
- Default path gate: `plaintext` ~250 ns/op with exactly 3 allocations/op; plain-TCP loopback throughput unchanged within noise (before/after on the same session).
- Verification: `bash scripts/verify-docker.sh` (this plan adds `tls` to its feature lists); quick loop: `cargo test --features "uuid test-util swagger multipart tls" <filter>`. Use `python` (not `python3`).

## Review Focus

1. A client that connects and sends nothing is closed after `handshake_timeout` and never blocks other connections from being accepted.
2. Plain HTTP or random bytes sent to the TLS port must not crash or hang the server; the next valid TLS client still works.
3. Shutdown while a request is in flight over TLS lets it finish (within `shutdown_timeout`); a connection still in the handshake never holds shutdown past that timeout.
4. A mismatched key/cert pair, an empty certificate file, and a key-only file give `TlsError`, never a panic.
5. A TLS 1.2-only client and a TLS 1.3 client both complete a request.

## Spec deviation (recorded)

`docs/tls-design.md` lists PKCS#8, PKCS#1 and SEC1 key tests. `rcgen` with `ring` only produces PKCS#8, and committing RSA/EC private-key fixtures would trip secret scanners, so the tests cover PKCS#8 and unparsable/mismatched input; PKCS#1/SEC1 parsing is delegated to `rustls-pki-types` (documented in the README). This is ledgered as a ruling at execution time.

---

### Task 1: Extract the shared accept loop (pure refactor) and add the feature scaffold

**Files:**
- Modify: `src/runtime.rs` (`serve_runtime`, new `accept_next`), `Cargo.toml` (feature `tls`, optional deps, dev-dep `rcgen`)

**Interfaces:**
- Produces: `pub(crate) async fn accept_next<F>(listener: &tokio::net::TcpListener, shutdown: &mut Pin<&mut F>) -> Result<Option<tokio::net::TcpStream>, std::io::Error> where F: Future<Output = ()>` — `Ok(None)` means the shutdown future completed; transient errors (`classify_accept_error` Retry/Backoff) are handled inside; `Fatal` errors are returned.

- [ ] **Step 1: Baseline**

Run: `cargo test --features "uuid test-util swagger multipart"`
Expected: all pass (this is a refactor; the graceful-shutdown tests and `accept_tests` are the safety net).

- [ ] **Step 2: Extract `accept_next`**

In `src/runtime.rs` add:

```rust
/// Waits for the next connection. `Ok(None)` means `shutdown` completed.
/// Transient accept errors are absorbed (see [`classify_accept_error`]); only
/// a broken listener is returned as an error.
pub(crate) async fn accept_next<F>(
    listener: &tokio::net::TcpListener,
    shutdown: &mut Pin<&mut F>,
) -> Result<Option<tokio::net::TcpStream>, std::io::Error>
where
    F: Future<Output = ()>,
{
    loop {
        tokio::select! {
            _ = shutdown.as_mut() => return Ok(None),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => return Ok(Some(stream)),
                Err(error) => match classify_accept_error(&error) {
                    AcceptAction::Retry => continue,
                    AcceptAction::Backoff => {
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                        continue;
                    }
                    AcceptAction::Fatal => return Err(error),
                },
            },
        }
    }
}
```

and rewrite the loop in `serve_runtime` as:

```rust
tokio::pin!(shutdown);
while let Some(stream) = accept_next(&listener, &mut shutdown).await? {
    let connection = ConnectionRuntime::new(Arc::clone(&runtime));
    let io = hyper_util::rt::TokioIo::new(stream);
    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
        let prepared = connection.prepare(request);
        async move { Ok::<_, Infallible>(prepared.await) }
    });
    let connection = hyper::server::conn::http1::Builder::new().serve_connection(io, service);
    let connection = graceful.watch(connection);
    tokio::spawn(async move {
        let _ = connection.await;
    });
}
let _ = tokio::time::timeout(shutdown_timeout, graceful.shutdown()).await;
Ok(())
```

- [ ] **Step 3: Add the feature scaffold**

`Cargo.toml`:

```toml
# [features]
tls = ["dep:tokio-rustls", "dep:rustls-pki-types"]

# [dependencies]
tokio-rustls = { version = "0.26", default-features = false, features = ["ring", "tls12"], optional = true }
rustls-pki-types = { version = "1", features = ["std"], optional = true }

# [dev-dependencies]
rcgen = { version = "0.14", default-features = false, features = ["crypto", "ring", "pem"] }
```

- [ ] **Step 4: Verify and measure, then commit**

Run: `cargo test --features "uuid test-util swagger multipart"` then `cargo check --features tls` (must compile with the new optional deps) then `bash scripts/verify-docker.sh`.
Expected: all pass. Run the plain-TCP loopback comparison (keep-alive and short connections, 3 alternating runs, `main` vs this branch) and the microbenchmark; the plain path must be unchanged within noise.

```bash
git add -A
git commit -m "refactor: share the accept loop; add the tls feature scaffold"
```

---

### Task 2: `TlsConfig` and `TlsError`

**Files:**
- Create: `src/tls.rs` (`#[cfg(feature = "tls")]`)
- Modify: `src/lib.rs` (`#[cfg(feature = "tls")] mod tls;` and re-exports), `Cargo.toml` (`[[test]] name = "tls"`, `required-features = ["tls", "test-util"]`)
- Test: `tests/tls.rs`

**Interfaces:**
- Produces: `TlsConfig::from_pem(cert: &[u8], key: &[u8]) -> Result<TlsConfig, TlsError>`, `TlsConfig::from_pem_files(cert: impl AsRef<Path>, key: impl AsRef<Path>) -> Result<TlsConfig, TlsError>`, `TlsError` (`Display` + `std::error::Error`, `Debug`), `TlsConfig: Clone`, `pub(crate) fn TlsConfig::acceptor(&self) -> tokio_rustls::TlsAcceptor`.

- [ ] **Step 1: Write the failing tests**

Create `tests/tls.rs`:

```rust
use oas_rs::{TlsConfig, TlsError};

pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn identity() -> Identity {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    Identity {
        cert_pem: certified.cert.pem(),
        key_pem: certified.signing_key.serialize_pem(),
    }
}

#[test]
fn loads_a_pkcs8_pem_pair() {
    let id = identity();
    TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
}

#[test]
fn loads_a_pem_pair_from_files() {
    let id = identity();
    let dir = std::env::temp_dir().join(format!("oas-rs-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert, &id.cert_pem).unwrap();
    std::fs::write(&key, &id.key_pem).unwrap();
    TlsConfig::from_pem_files(&cert, &key).unwrap();
    let missing = TlsConfig::from_pem_files(dir.join("nope.pem"), &key);
    assert!(missing.is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rejects_a_mismatched_key_and_certificate() {
    let (a, b) = (identity(), identity());
    let error: TlsError = TlsConfig::from_pem(a.cert_pem.as_bytes(), b.key_pem.as_bytes())
        .err()
        .expect("mismatched pair must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn rejects_empty_unparsable_and_key_only_input() {
    let id = identity();
    assert!(TlsConfig::from_pem(b"", id.key_pem.as_bytes()).is_err()); // no certificate
    assert!(TlsConfig::from_pem(id.cert_pem.as_bytes(), b"").is_err()); // no key
    assert!(TlsConfig::from_pem(b"not pem at all", b"also not pem").is_err());
    assert!(TlsConfig::from_pem(id.key_pem.as_bytes(), id.key_pem.as_bytes()).is_err()); // key where a cert is expected
}
```

- [ ] **Step 2: Confirm RED**

Add to `Cargo.toml`:

```toml
[[test]]
name = "tls"
required-features = ["tls", "test-util"]
```

Run: `cargo test --features "uuid test-util swagger multipart tls" --test tls`
Expected: FAIL to compile (`unresolved imports oas_rs::TlsConfig, oas_rs::TlsError`).

- [ ] **Step 3: Implement `src/tls.rs`**

```rust
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

    pub(crate) fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(Arc::clone(&self.config))
    }
}

impl fmt::Debug for TlsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TlsConfig").finish_non_exhaustive()
    }
}
```

`src/lib.rs`:

```rust
#[cfg(feature = "tls")]
mod tls;
#[cfg(feature = "tls")]
pub use tls::{TlsConfig, TlsError};
```

- [ ] **Step 4: Verify GREEN, then mutation-check**

Run: `cargo test --features "uuid test-util swagger multipart tls" --test tls`
Expected: 4 passed. Mutation: temporarily make `from_pem` skip the `certs.is_empty()` check and confirm `rejects_empty_unparsable_and_key_only_input` FAILS (rustls rejects an empty chain too, so if it still passes, record that and keep the explicit check for its clearer message), then restore.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: add TlsConfig and TlsError (feature tls)"
```

---

### Task 3: `serve_tls`, handshake timeout and graceful shutdown over TLS

**Files:**
- Modify: `src/runtime.rs` (`AppRuntime` field `handshake_timeout`), `src/tls.rs` (`serve_tls`, `handshake_timeout`), `src/app.rs` (`build()` initializes the field)
- Test: `tests/tls.rs`

**Interfaces:**
- Consumes: Task 1's `accept_next`, Task 2's `TlsConfig::acceptor`, `ConnectionRuntime::prepare`.
- Produces: `AppRuntime::serve_tls<F>(self, listener: TcpListener, tls: TlsConfig, shutdown: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>` and `AppRuntime::handshake_timeout(self, Duration) -> Self`; constant `DEFAULT_HANDSHAKE_TIMEOUT: Duration` (10 s), re-exported at the crate root under the feature.

- [ ] **Step 1: Write the failing tests**

Append to `tests/tls.rs`:

```rust
use std::{sync::Arc, time::{Duration, Instant}};

use oas_rs::App;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, crypto::ring, version},
};
use rustls_pki_types::{CertificateDer, ServerName, pem::PemObject};

async fn hello() -> &'static str {
    "hello"
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

struct Server {
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<Instant>,
    cert_pem: String,
}

async fn start(handshake_timeout: Option<Duration>) -> Server {
    let id = identity();
    let mut app = App::new();
    app.get("/", hello);
    app.get("/slow", slow);
    let mut runtime = app.build().unwrap();
    if let Some(timeout) = handshake_timeout {
        runtime = runtime.handshake_timeout(timeout);
    }
    let tls = TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let done = tokio::spawn(async move {
        runtime
            .serve_tls(listener, tls, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        Instant::now()
    });
    Server { addr, stop: Some(stop), done, cert_pem: id.cert_pem }
}

fn connector(cert_pem: &str, versions: &[&'static version::SupportedProtocolVersion]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

async fn https_get(server: &Server, versions: &[&'static version::SupportedProtocolVersion], path: &str) -> String {
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let mut tls = connector(&server.cert_pem, versions)
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    tls.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    // A close_notify-less close is reported as an error after the data; keep what was read.
    let _ = tls.read_to_string(&mut out).await;
    out
}

#[tokio::test]
async fn serves_https_with_tls_13_and_tls_12() {
    let server = start(None).await;
    for versions in [&[&version::TLS13][..], &[&version::TLS12][..]] {
        let out = https_get(&server, versions, "/").await;
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert!(out.ends_with("hello"), "{out}");
    }
}

#[tokio::test]
async fn keep_alive_works_over_one_tls_connection() {
    let server = start(None).await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let mut tls = connector(&server.cert_pem, &[&version::TLS13])
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    for _ in 0..2 {
        tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0u8; 256];
        while !seen.ends_with(b"hello") {
            let read = tls.read(&mut buffer).await.unwrap();
            assert!(read > 0, "connection closed early");
            seen.extend_from_slice(&buffer[..read]);
        }
        assert!(String::from_utf8_lossy(&seen).starts_with("HTTP/1.1 200"));
    }
}

#[tokio::test]
async fn garbage_and_plain_http_do_not_hurt_the_server() {
    let server = start(None).await;
    for payload in [&b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"[..], &[0u8, 1, 2, 3, 255, 254][..], &b""[..]] {
        let mut tcp = TcpStream::connect(server.addr).await.unwrap();
        tcp.write_all(payload).await.unwrap();
        let mut sink = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), tcp.read_to_end(&mut sink)).await;
    }
    let out = https_get(&server, &[&version::TLS13], "/").await;
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
}

#[tokio::test]
async fn a_stalled_handshake_times_out_without_blocking_others() {
    let server = start(Some(Duration::from_millis(200))).await;
    let mut stalled = TcpStream::connect(server.addr).await.unwrap(); // sends nothing
    // Others are served while the stalled connection is open.
    let out = https_get(&server, &[&version::TLS13], "/").await;
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    // The stalled connection is closed by the server after the timeout.
    let mut buffer = [0u8; 16];
    let closed = tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut buffer))
        .await
        .expect("stalled handshake was never closed");
    assert!(matches!(closed, Ok(0) | Err(_)));
}

#[tokio::test]
async fn shutdown_lets_an_in_flight_https_request_finish() {
    let mut server = start(None).await;
    let addr = server.addr;
    let cert = server.cert_pem.clone();
    let client = tokio::spawn(async move {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut tls = connector(&cert, &[&version::TLS13])
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        tls.write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut out = String::new();
        let _ = tls.read_to_string(&mut out).await;
        out
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let out = client.await.unwrap();
    let returned_at = server.done.await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200") && out.ends_with("done"), "{out}");
    assert!(returned_at.duration_since(shutdown_at) >= Duration::from_millis(100));
}

#[tokio::test]
async fn a_pending_handshake_does_not_hold_shutdown_past_the_timeout() {
    let mut server = start(None).await;
    let _stalled = TcpStream::connect(server.addr).await.unwrap(); // never handshakes
    tokio::time::sleep(Duration::from_millis(50)).await;
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let returned_at = tokio::time::timeout(Duration::from_secs(5), server.done)
        .await
        .expect("a pending handshake blocked shutdown")
        .unwrap();
    assert!(returned_at.duration_since(shutdown_at) < Duration::from_secs(2));
}
```

- [ ] **Step 2: Confirm RED**

Run: `cargo test --features "uuid test-util swagger multipart tls" --test tls`
Expected: FAIL to compile (`no method named serve_tls` / `handshake_timeout`).

- [ ] **Step 3: Implement**

`src/runtime.rs`: add `pub(crate) handshake_timeout: Duration` to `AppRuntime` (field initialized in `build()` to the constant below) and the constant:

```rust
/// How long a TLS connection may take to complete its handshake.
#[cfg(feature = "tls")]
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
```

Re-export it from `src/lib.rs` under `#[cfg(feature = "tls")]`. In `src/app.rs` `build()` set `handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT` (cfg-gate the field and initializer on `feature = "tls"` to keep the default struct unchanged).

`src/tls.rs`, add:

```rust
use crate::*;

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
        let runtime = self.inner;
        let acceptor = tls.acceptor();
        let (signal, _) = tokio::sync::watch::channel(());
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);
        tokio::pin!(shutdown);
        while let Some(stream) = accept_next(&listener, &mut shutdown).await? {
            let acceptor = acceptor.clone();
            let connection = ConnectionRuntime::new(Arc::clone(&runtime));
            let mut stop = signal.subscribe();
            let done = done_tx.clone();
            tokio::spawn(async move {
                let _done = done; // dropped when the task ends; shutdown waits for all of them
                let handshake = tokio::time::timeout(handshake_timeout, acceptor.accept(stream));
                let tls_stream = tokio::select! {
                    result = handshake => match result {
                        Ok(Ok(tls_stream)) => tls_stream,
                        _ => return, // failed or timed-out handshake: drop only this connection
                    },
                    _ = stop.changed() => return, // shutting down during the handshake
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
```

`done_rx.recv()` returns `None` once every task has dropped its sender, which is when the last connection finished.

- [ ] **Step 4: Verify GREEN, mutation-check, commit**

Run: `cargo test --features "uuid test-util swagger multipart tls" --test tls`
Expected: all pass. Mutation checks (each must make the named test FAIL, then restore): remove the `handshake` timeout wrapper (`a_stalled_handshake_times_out_without_blocking_others`), remove the `stop.changed()` arm of the handshake select (`a_pending_handshake_does_not_hold_shutdown_past_the_timeout`), drop `graceful_shutdown()` (`shutdown_lets_an_in_flight_https_request_finish` must still pass but an idle keep-alive connection would hold shutdown to the timeout; note the result in the ledger).

```bash
git add -A
git commit -m "feat: add serve_tls with handshake timeout and graceful shutdown"
```

---

### Task 4: Benchmarks and the performance gate

**Files:**
- Modify: `docs/tls-design.md` (Results)
- Scratch (not committed): loopback benchmark crates, as in earlier gates

- [ ] **Step 1: Default-path gate**

Run `cargo bench --bench router --features uuid,test-util,swagger` three times in the pinned Docker image, with and without `tls` enabled (`...,tls`). Gate: `plaintext` ~250 ns/op, exactly 3.0000 allocations/op, identical within noise.

- [ ] **Step 2: Plain TCP loopback before/after**

Build the loopback benchmark (16 connections, 3 s, keep-alive and short connections) against `main` and this branch; 3 alternating runs; report mean req/s. Gate: unchanged within noise (the accept loop was refactored in Task 1).

- [ ] **Step 3: TLS loopback**

Add a TLS variant of the loopback benchmark (client `tokio-rustls` trusting an `rcgen` certificate): keep-alive and short connections, report req/s and handshakes/s (short connections = one handshake each) versus plain TCP.

- [ ] **Step 4: Record and commit**

Append a "Results" section with the numbers to `docs/tls-design.md`.

```bash
git add docs/tls-design.md
git commit -m "bench: TLS and plain-path performance gates"
```

If the default or plain path regresses beyond noise, stop and fix before continuing.

---

### Task 5: Documentation, supply-chain checks and final verification

**Files:**
- Modify: `README.md` (new "TLS" section, feature lists), `CHANGELOG.md`, `CLAUDE.md`, `deny.toml` (only if `cargo deny` needs a new license allowance), `scripts/verify-docker.sh` and `.github/workflows/ci.yml` (add `tls` to the feature lists), `docs/tls-design.md` (status)

- [ ] **Step 1: Feature lists and README**

Add `tls` to every `--features "uuid test-util swagger multipart"` in `scripts/verify-docker.sh`, `.github/workflows/ci.yml`, README Verification and CLAUDE.md. README "TLS" section: the `tls` feature, `TlsConfig::from_pem_files` / `from_pem`, `serve_tls`, `handshake_timeout`, HTTP/1.1 only, TLS 1.2/1.3 with the `ring` provider, no client certificates, the PEM key formats (PKCS#8, PKCS#1, SEC1) handled by `rustls-pki-types`, and an example using `tokio::signal::ctrl_c()` as the shutdown future. Include a compile-checked test mirroring the README example if practical.

- [ ] **Step 2: Supply-chain**

Run (in Docker, as in CI): `cargo deny check` and `cargo deny --all-features check`.
Expected: both pass; if a license is rejected (typically `ring`), add the minimal allowance to `deny.toml` with a comment naming the crate and why.

- [ ] **Step 3: CHANGELOG, CLAUDE.md, spec status**

CHANGELOG (Unreleased): `serve_tls`, `TlsConfig`, `TlsError`, `handshake_timeout`, feature `tls`. CLAUDE.md: feature note and the TLS accept/drain design (shared `accept_next`, own tracking for TLS because `GracefulShutdown::watch` needs an existing connection). Set the spec status to implemented.

- [ ] **Step 4: Final verification and commit**

Run: `bash scripts/verify-docker.sh`
Expected: every gate passes.

```bash
git add -A
git commit -m "docs: document TLS and add it to the CI feature lists"
```
