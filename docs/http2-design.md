# HTTP/2 design for oas-rs

Status: implemented (see Results at the end). Target release: 0.5.0.

## Goal

Let `serve_tls` serve HTTP/2 to clients that negotiate it through ALPN, with
the same graceful shutdown, handshake timeout and `TCP_NODELAY` behavior as the
HTTP/1.1 path, at no cost for applications that do not enable the feature.

Non-goals: cleartext HTTP/2 (h2c / prior knowledge) on `serve_listener`,
server push, HTTP/2 tuning knobs (stream limits, window sizes, PING keep-alive;
Hyper's defaults apply), HTTP/3.

## Why TLS only

HTTP/2 in practice is negotiated over TLS (ALPN `h2`). Supporting h2c on the
plain listener would require hyper-util's `auto` builder, which sniffs the
connection preface and would add work to every plain HTTP/1.1 connection. The
plain path therefore stays exactly as it is.

## Public API

Feature `http2` (off by default; implies `tls`; enables `hyper/http2` and
`hyper-util/http2`, which pulls in `h2`):

```rust
impl TlsConfig {
    /// Offer HTTP/2 (`h2`) next to `http/1.1` through ALPN. On by default when
    /// the `http2` feature is enabled.
    pub fn enable_http2(self, enabled: bool) -> Self;
}
```

`serve_tls` itself is unchanged. With the feature on and h2 enabled, ALPN
advertises `["h2", "http/1.1"]`; otherwise only `["http/1.1"]` (the current
behavior).

## Behavior

- After the handshake, the negotiated ALPN protocol selects the connection type:
  `h2` uses `hyper::server::conn::http2` with a Tokio executor; anything else
  uses the existing HTTP/1.1 connection.
- Routing, extractors, responses and middleware are unchanged: Hyper hands both
  protocols the same `Request<Incoming>`. HTTP/2 has no `Host` header (the
  authority is in the request URI); this is documented.
- Graceful shutdown: on the shutdown signal an established h2 connection gets
  `graceful_shutdown()` (GOAWAY, in-flight streams finish), bounded by
  `shutdown_timeout`; idle h2 connections are closed; connections still in the
  handshake are dropped. Same drain mechanism as the HTTP/1.1 TLS path.
- A client that offers only `h2` while HTTP/2 is disabled is rejected during the
  handshake (no application protocol in common), as today for `h2`-only clients.

## Testing

Written before the implementation, with a `hyper` HTTP/2 client over
`tokio-rustls` (dev-dependency `hyper` with the `client` feature):

- ALPN negotiates `h2` when enabled and `http/1.1` when the client offers only
  that; an `h2`-only client is rejected when HTTP/2 is disabled.
- Request/response over h2 (status, headers, body, response version).
- Many concurrent streams on one connection are served concurrently.
- A large request body over h2 (JSON echo) and HEAD/404/405 semantics.
- Middleware (a layer adding a header, `BearerAuth`) works over h2.
- Graceful shutdown: an in-flight h2 stream finishes; an idle h2 connection
  does not block shutdown past the timeout.
- With the feature off, the default build is unchanged.

## Performance gate

- Default build and plain TCP loopback: unchanged within noise
  (`plaintext` ~250 ns, 3 allocations).
- TLS HTTP/1.1 path with the feature on: unchanged within noise versus 0.4.0.
- Report TLS HTTP/1.1 versus HTTP/2 throughput and latency on loopback
  (multiplexed streams per connection versus connection per request).

## Risks

- `h2` adds dependencies behind the feature; `cargo deny` must pass with default
  features and `--all-features`.
- HTTP/2 rapid-reset style abuse is mitigated by `h2`'s defaults (bounded
  pending-accept reset streams); the version in use is recorded in the results.

## Results (measured 2026-10-08, Docker Linux, Rust 1.88, release profile)

Default-path gate, microbenchmark (`benches/router.rs`), 3 runs each:

| Case | without `http2` | with `multipart,tls,http2` |
|---|---|---|
| `plaintext` (median) | 260.0 ns, 3 allocations | 260.0 ns, 3 allocations |
| `static_route_count` (1 route, median) | 266.5 ns, 3 allocations | 260.6 ns, 3 allocations |

One run of the second configuration showed a 296-302 ns outlier in both cases
(the other two runs: 260 ns); run-to-run noise on this machine is about +/-5%
with occasional outliers, so the medians are the comparison.

TLS HTTP/1.1 over loopback (16 connections, keep-alive, 3 s, mean of 3
alternating runs): `main` 205,300 req/s versus this branch with the `http2`
feature on 205,400 req/s (p50 73-74 us, p99 152-175 us on both): unchanged.

HTTP/2 versus HTTP/1.1 over TLS (same harness, tiny response, mean of 3 runs):

| Client | Throughput | p50 | p99 |
|---|---|---|---|
| HTTP/1.1, 16 connections x 1 request in flight | 205,400 req/s | 73 us | 152-175 us |
| HTTP/2, 16 connections x 1 stream | 112,100 req/s | 137 us | 257-291 us |
| HTTP/2, 16 connections x 8 streams (128 in flight) | 227,300 req/s | 550 us | 990-1,197 us |

At equal concurrency a single h2 stream costs about 1.8x the latency of an
HTTP/1.1 request on a keep-alive connection (frame encoding, HPACK, per-stream
tasks), so HTTP/2 is not a speed-up for tiny responses on loopback. Its value is
multiplexing many requests over few connections: at 128 requests in flight it
delivered about 11% more throughput than 16 HTTP/1.1 connections (latency is
higher there because more requests are queued).

Supply chain: `cargo deny check` passes with default features and with
`--all-features`; the resolved `h2` version is 0.4.19.
