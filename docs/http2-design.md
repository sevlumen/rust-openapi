# HTTP/2 design for oas-rs

Status: approved design, being implemented. Target release: 0.5.0.

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
