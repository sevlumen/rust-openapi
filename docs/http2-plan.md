# HTTP/2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `serve_tls` serves HTTP/2 to clients that negotiate ALPN `h2`, behind a new `http2` feature, with identical graceful shutdown, handshake timeout and `TCP_NODELAY` behavior.

**Architecture:** After the TLS handshake the negotiated ALPN protocol picks `hyper::server::conn::http2` (Tokio executor) or the existing HTTP/1.1 connection; both are driven by the same watch/mpsc drain logic already used by `serve_tls`. The plain HTTP/1.1 path is untouched.

**Tech Stack:** Rust 1.88, `hyper` 1 (`http2`), `hyper-util` (`http2`, `TokioExecutor`), `tokio-rustls`; dev: `hyper` client.

**Spec:** `docs/http2-design.md`

## Global Constraints

- No new dependency without the `http2` feature; `http2 = ["tls", "hyper/http2", "hyper-util/http2"]`.
- Plain-HTTP/1.1 and TLS-HTTP/1.1 paths must not regress (microbenchmark ~250 ns / 3 allocations; loopback within noise).
- MSRV 1.88; `cargo deny check` passes for default features and `--all-features`.
- Verification: `bash scripts/verify-docker.sh` (this plan adds `http2` to its feature lists); quick loop: `cargo test --features "uuid test-util swagger multipart tls http2" --test http2`. Use `python` (not `python3`).
- Every `unsafe` needs `// SAFETY:` (none expected).

## Review Focus

1. An `h2`-only client against a server with HTTP/2 disabled is rejected at the handshake and the server keeps serving others.
2. An HTTP/1.1-only TLS client still works with the feature on.
3. Shutdown with an in-flight h2 stream lets it finish; an idle h2 connection never blocks shutdown past `shutdown_timeout`.
4. Many concurrent streams on one connection are served concurrently (not serialized).
5. HEAD, 404, 405 and a layer/`BearerAuth` behave the same over h2 as over HTTP/1.1 (no `Host` header on h2).

---

### Task 1: Feature scaffold and ALPN negotiation

**Files:**
- Modify: `Cargo.toml` (feature `http2`, dev-dependency `hyper` client, `[[test]] name = "http2"`), `src/tls.rs` (`TlsConfig::enable_http2`, ALPN list)
- Test: `tests/http2.rs`

**Interfaces:**
- Produces: `TlsConfig::enable_http2(self, enabled: bool) -> Self` (only with feature `http2`); with the feature on, the default ALPN list is `["h2", "http/1.1"]`.

- [ ] **Step 1: Write the failing tests**

Create `tests/http2.rs` with helpers and the ALPN tests:

```rust
use std::{sync::Arc, time::{Duration, Instant}};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use oas_rs::{App, TlsConfig};
use rustls_pki_types::{CertificateDer, ServerName, pem::PemObject};
use tokio::{net::{TcpListener, TcpStream}, sync::oneshot};
use tokio_rustls::{TlsConnector, rustls::{ClientConfig, RootCertStore, crypto::ring}};

pub struct Identity { pub cert_pem: String, pub key_pem: String }

pub fn identity() -> Identity {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    Identity { cert_pem: certified.cert.pem(), key_pem: certified.signing_key.serialize_pem() }
}

fn connector(cert_pem: &str, alpn: &[&[u8]]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    TlsConnector::from(Arc::new(config))
}

async fn hello() -> &'static str { "hello" }

struct Server {
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<Instant>,
    cert_pem: String,
}

async fn start(app: App, http2: bool) -> Server {
    let id = identity();
    let runtime = app.build().unwrap();
    let tls = TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes())
        .unwrap()
        .enable_http2(http2);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let done = tokio::spawn(async move {
        runtime.serve_tls(listener, tls, async { let _ = stopped.await; }).await.unwrap();
        Instant::now()
    });
    Server { addr, stop: Some(stop), done, cert_pem: id.cert_pem }
}

fn app_with_hello() -> App {
    let mut app = App::new();
    app.get("/", hello);
    app
}

async fn handshake(server: &Server, alpn: &[&[u8]])
    -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>>
{
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    connector(&server.cert_pem, alpn)
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
}

#[tokio::test]
async fn alpn_negotiates_h2_when_enabled() {
    let server = start(app_with_hello(), true).await;
    let tls = handshake(&server, &[b"h2", b"http/1.1"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
}

#[tokio::test]
async fn alpn_falls_back_to_http11_for_clients_that_only_offer_it() {
    let server = start(app_with_hello(), true).await;
    let tls = handshake(&server, &[b"http/1.1"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
}

#[tokio::test]
async fn an_h2_only_client_is_rejected_when_http2_is_disabled() {
    let server = start(app_with_hello(), false).await;
    assert!(handshake(&server, &[b"h2"]).await.is_err());
    // The server keeps serving others.
    let tls = handshake(&server, &[b"http/1.1"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
}
```

- [ ] **Step 2: Confirm RED**

Add to `Cargo.toml`:

```toml
# [features]
http2 = ["tls", "hyper/http2", "hyper-util/http2"]

# [dev-dependencies]
hyper = { version = "1", features = ["client", "http1", "http2"] }

[[test]]
name = "http2"
required-features = ["http2"]
```

Run: `cargo test --features "uuid test-util swagger multipart tls http2" --test http2`
Expected: FAIL to compile (`no method named enable_http2`).

- [ ] **Step 3: Implement**

In `src/tls.rs` add a field `http2: bool` to `TlsConfig` (always `false` without the feature; default `true` with it) and set the ALPN list from it:

```rust
fn alpn_protocols(http2: bool) -> Vec<Vec<u8>> {
    #[cfg(feature = "http2")]
    if http2 {
        return vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    let _ = http2;
    vec![b"http/1.1".to_vec()]
}
```

`from_pem` builds the config with `alpn_protocols(cfg!(feature = "http2"))` and stores `http2: cfg!(feature = "http2")`. Add (behind `#[cfg(feature = "http2")]`):

```rust
/// Offer HTTP/2 (`h2`) next to `http/1.1` through ALPN. On by default when the
/// `http2` feature is enabled.
pub fn enable_http2(mut self, enabled: bool) -> Self {
    let mut config = (*self.config).clone();
    config.alpn_protocols = alpn_protocols(enabled);
    self.config = Arc::new(config);
    self.http2 = enabled;
    self
}
```

- [ ] **Step 4: Verify GREEN, commit**

Run: `cargo test --features "uuid test-util swagger multipart tls http2" --test http2`
Expected: 3 passed. Also `cargo test --features "uuid test-util swagger multipart tls"` (feature off) still passes.

```bash
git add -A
git commit -m "feat: add the http2 feature and ALPN negotiation for h2"
```

---

### Task 2: Serve HTTP/2 connections

**Files:**
- Modify: `src/tls.rs` (connection driver chooses h2 or http1 after the handshake)
- Test: `tests/http2.rs`

**Interfaces:** Consumes Task 1's `enable_http2` and helpers.

- [ ] **Step 1: Write the failing tests**

Append to `tests/http2.rs`:

```rust
type H2Sender = hyper::client::conn::http2::SendRequest<Full<Bytes>>;

async fn h2_client(server: &Server) -> H2Sender {
    let tls = handshake(server, &[b"h2"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .unwrap();
    tokio::spawn(async move { let _ = connection.await; });
    sender
}

fn get(path: &str) -> http::Request<Full<Bytes>> {
    http::Request::builder()
        .method("GET")
        .uri(format!("https://localhost{path}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}

async fn text(response: http::Response<hyper::body::Incoming>) -> (u16, http::Version, String) {
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (parts.status.as_u16(), parts.version, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn serves_a_request_over_h2() {
    let server = start(app_with_hello(), true).await;
    let mut sender = h2_client(&server).await;
    let (status, version, body) = text(sender.send_request(get("/")).await.unwrap()).await;
    assert_eq!((status, version, body.as_str()), (200, http::Version::HTTP_2, "hello"));
}

#[tokio::test]
async fn http11_clients_still_work_when_http2_is_enabled() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let server = start(app_with_hello(), true).await;
    let mut tls = handshake(&server, &[b"http/1.1"]).await.unwrap();
    tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut out = String::new();
    let _ = tls.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 200") && out.ends_with("hello"), "{out}");
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

#[tokio::test]
async fn many_concurrent_streams_share_one_connection() {
    let mut app = App::new();
    app.get("/slow", slow);
    let server = start(app, true).await;
    let sender = h2_client(&server).await;
    let started = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let mut sender = sender.clone();
        tasks.push(tokio::spawn(async move {
            text(sender.send_request(get("/slow")).await.unwrap()).await
        }));
    }
    for task in tasks {
        let (status, _, body) = task.await.unwrap();
        assert_eq!((status, body.as_str()), (200, "done"));
    }
    // 20 streams of 300 ms: concurrent means well under 20 * 300 ms.
    assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
}

#[tokio::test]
async fn status_and_method_semantics_match_http11_over_h2() {
    let server = start(app_with_hello(), true).await;
    let mut sender = h2_client(&server).await;
    let (status, _, _) = text(sender.send_request(get("/missing")).await.unwrap()).await;
    assert_eq!(status, 404);
    let post = http::Request::builder().method("POST").uri("https://localhost/").body(Full::new(Bytes::new())).unwrap();
    let (status, _, _) = text(sender.send_request(post).await.unwrap()).await;
    assert_eq!(status, 405);
    let head = http::Request::builder().method("HEAD").uri("https://localhost/").body(Full::new(Bytes::new())).unwrap();
    let response = sender.send_request(head).await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.into_body().collect().await.unwrap().to_bytes().is_empty());
}

#[tokio::test]
async fn a_large_json_body_round_trips_over_h2() {
    use oas_rs::Json;
    async fn echo(Json(value): Json<serde_json::Value>) -> Json<serde_json::Value> { Json(value) }
    let mut app = App::new();
    app.post("/echo", echo).body_limit(2 * 1024 * 1024);
    let server = start(app, true).await;
    let mut sender = h2_client(&server).await;
    let big = serde_json::json!({ "data": "x".repeat(512 * 1024) });
    let body = serde_json::to_vec(&big).unwrap();
    let request = http::Request::builder()
        .method("POST").uri("https://localhost/echo")
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.clone()))).unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let echoed = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(echoed.len(), body.len());
}

#[tokio::test]
async fn middleware_runs_over_h2() {
    use oas_rs::{BearerAuth, ApiError};
    let mut app = app_with_hello();
    app.layer(BearerAuth::new(|token: String| async move {
        if token == "good" { Ok(()) } else { Err(ApiError::missing("bad")) }
    }));
    let server = start(app, true).await;
    let mut sender = h2_client(&server).await;
    let (status, _, _) = text(sender.send_request(get("/")).await.unwrap()).await;
    assert_eq!(status, 401);
    let request = http::Request::builder().uri("https://localhost/")
        .header("authorization", "Bearer good").body(Full::new(Bytes::new())).unwrap();
    let (status, _, body) = text(sender.send_request(request).await.unwrap()).await;
    assert_eq!((status, body.as_str()), (200, "hello"));
}
```

- [ ] **Step 2: Confirm RED**

Run: `cargo test --features "uuid test-util swagger multipart tls http2" --test http2`
Expected: the ALPN tests pass; the new h2 tests FAIL (the server speaks HTTP/1.1 on an h2 connection: client handshake or request errors).

- [ ] **Step 3: Implement**

In `src/tls.rs` `serve_tls`, after the handshake succeeds and before building the connection, decide the protocol and drive the matching connection with the existing stop/drain logic:

```rust
#[cfg(feature = "http2")]
let negotiated_h2 = tls_stream.get_ref().1.alpn_protocol() == Some(b"h2");
let io = hyper_util::rt::TokioIo::new(tls_stream);
let service = hyper::service::service_fn(move |request: Request<Incoming>| {
    let prepared = connection.prepare(request);
    async move { Ok::<_, Infallible>(prepared.await) }
});
#[cfg(feature = "http2")]
if negotiated_h2 {
    let conn = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(io, service);
    tokio::pin!(conn);
    tokio::select! {
        _ = conn.as_mut() => return,
        _ = stop.changed() => conn.as_mut().graceful_shutdown(),
    }
    let _ = conn.await;
    return;
}
let conn = hyper::server::conn::http1::Builder::new().serve_connection(io, service);
// ... existing http1 select/drain unchanged ...
```

If the closure `service` is moved into the h2 branch, build it once and move it into whichever branch runs (the `return` in the h2 branch keeps this valid).

- [ ] **Step 4: Verify GREEN and full suite**

Run: `cargo test --features "uuid test-util swagger multipart tls http2"`
Expected: all pass. If a test fails because of a real h2 behavior (for example the 405 `Allow` handling or HEAD body), diagnose with systematic debugging, fix the code (not the test), and ledger it.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: serve HTTP/2 connections negotiated through ALPN"
```

---

### Task 3: Graceful shutdown over HTTP/2

**Files:**
- Test: `tests/http2.rs` (production code from Task 2 already drives `graceful_shutdown`)

- [ ] **Step 1: Write the tests**

```rust
#[tokio::test]
async fn shutdown_lets_an_in_flight_h2_stream_finish() {
    let mut app = App::new();
    app.get("/slow", slow);
    let mut server = start(app, true).await;
    let mut sender = h2_client(&server).await;
    let request = tokio::spawn(async move { text(sender.send_request(get("/slow")).await.unwrap()).await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let (status, _, body) = request.await.unwrap();
    assert_eq!((status, body.as_str()), (200, "done"));
    let returned_at = server.done.await.unwrap();
    assert!(returned_at.duration_since(shutdown_at) >= Duration::from_millis(100));
}

#[tokio::test]
async fn an_idle_h2_connection_does_not_block_shutdown() {
    let mut server = start(app_with_hello(), true).await;
    let mut sender = h2_client(&server).await;
    let (status, _, _) = text(sender.send_request(get("/")).await.unwrap()).await;
    assert_eq!(status, 200);
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let returned_at = tokio::time::timeout(Duration::from_secs(5), server.done)
        .await.expect("an idle h2 connection blocked shutdown").unwrap();
    assert!(returned_at.duration_since(shutdown_at) < Duration::from_secs(2));
    drop(sender);
}
```

- [ ] **Step 2: Run, then mutation-check each guard**

Run: `cargo test --features "uuid test-util swagger multipart tls http2" --test http2 shutdown_lets an_idle_h2`
Expected: both pass. Mutations (each must make its test FAIL, then restore): change the h2 branch's `stop.changed() => conn.as_mut().graceful_shutdown()` arm to `{}` (idle test fails by timeout; record what the in-flight test does). Use the exact test names as filters (a filter that matches nothing proves nothing).

- [ ] **Step 3: Commit**

```bash
git add tests/http2.rs
git commit -m "test: graceful shutdown over HTTP/2"
```

---

### Task 4: Performance gate and supply chain

**Files:** `docs/http2-design.md` (Results)

- [ ] **Step 1: Default-path gate**

Run `cargo bench --bench router --features uuid,test-util,swagger` and with `...,multipart,tls,http2` (3 runs each, Docker): `plaintext` ~250 ns, exactly 3 allocations, identical within noise. Plain TCP loopback (keep-alive and short, 3 alternating runs, `main` vs branch) unchanged.

- [ ] **Step 2: TLS HTTP/1.1 unchanged, h2 versus h1 reported**

Extend the scratch TLS loopback harness: TLS HTTP/1.1 (16 connections, keep-alive) on `main` versus this branch with `http2` on; then an h2 client (16 connections x 8 concurrent streams for 3 s) against the same server. Report req/s and p50/p99 latency.

- [ ] **Step 3: Supply chain**

Run `cargo deny check` and `cargo deny --all-features check` in Docker; add the minimal allowance to `deny.toml` only if a license is rejected. Record the resolved `h2` version.

- [ ] **Step 4: Record and commit**

Append a Results section to `docs/http2-design.md`.

```bash
git add docs/http2-design.md deny.toml
git commit -m "bench: HTTP/2 performance gates"
```

---

### Task 5: Documentation and final verification

**Files:** `README.md`, `CHANGELOG.md`, `CLAUDE.md`, `scripts/verify-docker.sh`, `.github/workflows/ci.yml`

- [ ] **Step 1: Feature lists**

Add `http2` to every `uuid test-util swagger multipart tls` feature list (tests) and to the examples list in `scripts/verify-docker.sh`, `.github/workflows/ci.yml`, README Verification and CLAUDE.md.

- [ ] **Step 2: Docs**

README: a short "HTTP/2" subsection under TLS (feature `http2`, ALPN `h2`, `TlsConfig::enable_http2(false)` to opt out, TLS only (no h2c), no `Host` header on h2, graceful shutdown same as HTTP/1.1, measured numbers qualitatively) and the optional-features line. CHANGELOG: Added. CLAUDE.md: the `http2` feature and how the connection type is chosen after the handshake. Spec status: implemented.

- [ ] **Step 3: Final verification and commit**

Run: `bash scripts/verify-docker.sh`
Expected: all gates pass.

```bash
git add -A
git commit -m "docs: document HTTP/2 and add it to the CI feature lists"
```
