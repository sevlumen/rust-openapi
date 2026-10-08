# TLS design for oas-rs

Status: draft for review. Target release: 0.3.0.

## Goal

Let an application serve HTTPS directly (`serve_tls`) with the same graceful
shutdown and accept-error handling as plain `serve_listener`, using a pure-Rust
TLS stack, with no cost for applications that do not enable the feature.

Non-goals: HTTP/2 and ALPN negotiation (the server is HTTP/1.1 only), client
certificates (mTLS), certificate hot-reload, ACME, TLS 1.0/1.1.

## Public API

Feature `tls` (off by default): `tokio-rustls` with the `ring` provider (no C
toolchain required) and `rustls-pki-types` (PEM parsing).

```rust
pub struct TlsConfig { /* Arc<rustls::ServerConfig> */ }
impl TlsConfig {
    /// Reads a PEM certificate chain and a PEM private key (PKCS#8, PKCS#1 or SEC1).
    pub fn from_pem_files(cert: impl AsRef<Path>, key: impl AsRef<Path>) -> Result<Self, TlsError>;
    /// Reads the same from in-memory PEM bytes.
    pub fn from_pem(cert: &[u8], key: &[u8]) -> Result<Self, TlsError>;
}
pub struct TlsError { /* Display + std::error::Error; wraps io/PEM/rustls errors */ }

impl<S> AppRuntime<S> {
    pub async fn serve_tls<F>(
        self,
        listener: tokio::net::TcpListener,
        tls: TlsConfig,
        shutdown: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where F: Future<Output = ()> + Send + 'static;

    pub fn handshake_timeout(self, timeout: Duration) -> Self; // default 10 s
}
```

rustls types are not part of the public API, so a rustls upgrade is not a
breaking change for oas-rs users. TLS 1.2 and 1.3 are enabled with rustls'
safe defaults.

## Architecture

- `serve_runtime` is generalized over a connection wrapper: a function turning
  an accepted `TcpStream` into the IO type given to Hyper. Plain TCP wraps with
  `TokioIo`; TLS performs the handshake and wraps the TLS stream. The accept
  loop, graceful shutdown (`GracefulShutdown::watch`), accept-error
  classification and `shutdown_timeout` are shared, not duplicated.
- The handshake runs inside the per-connection task, never in the accept loop,
  so a slow or stalled client cannot block other connections. It is bounded by
  `handshake_timeout` (slowloris protection). A failed or timed-out handshake
  closes only that connection.
- The zero-TLS path keeps its current monomorphized code; the generalization
  must not add a branch or allocation per request.

## Testing

Written before the implementation, using a certificate generated in the test
with `rcgen` (dev-dependency) and a `tokio-rustls` client that trusts it:
request/response over TLS (status, headers, body), keep-alive over one TLS
connection, a client that connects and sends garbage (server keeps serving), a
client that connects and stalls (handshake timeout closes it, others unaffected),
graceful shutdown with an in-flight request over TLS, wrong key/cert pair and
unparsable PEM (`TlsError`, no panic), and PEM key formats (PKCS#8, PKCS#1,
SEC1). With the feature disabled the default build is unchanged.

## Performance gate

- Default build: `plaintext`/`static_route_count` unchanged (about 250 ns, 3
  allocations) and TCP loopback keep-alive/short-connection throughput unchanged
  within noise.
- A TLS loopback measurement (keep-alive and short connections, 16 clients)
  against plain TCP, reported as req/s and handshakes/s.

## Risks and open questions

- `rustls` with `ring` limits supported cipher suites to ring's set; this is
  acceptable and documented.
- `cargo deny` license/advisory checks must pass for the new dependency tree
  (rustls, ring, webpki). The `ring` license (ISC/OpenSSL-style terms) may need
  an explicit allow entry in `deny.toml`.
- MSRV: the chosen rustls version must support Rust 1.88.

## Results (measured 2026-10-08, Docker Linux, Rust 1.88, release profile)

Default-path gate, microbenchmark (`benches/router.rs`), mean of 3 runs each:

| Case | features without `tls` | with `multipart,tls` |
|---|---|---|
| `plaintext` | 258.3 ns, 3 allocations | 260.7 ns, 3 allocations |
| `static_route_count` (1 route) | 258.5 ns, 3 allocations | 261.0 ns, 3 allocations |

Both are within run-to-run noise (about +/-5%): the feature adds nothing to the
request path of plain connections.

Plain TCP loopback before/after extracting the shared accept loop (16
connections, 3 s, mean of 3 alternating runs): keep-alive 220,800 vs 219,900
req/s (-0.4%), short connections 59,400 vs 60,400 req/s (+1.8%), i.e. unchanged.

TLS loopback (same harness, TLS 1.3, `ring`, self-signed certificate, mean of
3 alternating runs against plain TCP from the same session):

| Scenario | plain TCP | TLS | TLS vs plain |
|---|---|---|---|
| Keep-alive (handshake once per connection) | 219,600 req/s | 200,700 req/s | -8.6% |
| Short connections (one handshake per request) | 60,200 conn/s | 12,900 handshakes+requests/s | about 21% |

Steady-state TLS costs about 9% throughput on this loopback setup; short
connections are dominated by the handshake (key exchange and certificate
signing), which is why keep-alive and session reuse matter for TLS clients.
