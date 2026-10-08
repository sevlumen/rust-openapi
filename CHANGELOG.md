# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `#[derive(ApiSchema)]` for enums with data (`oneOf`; externally, internally
  and adjacently tagged and untagged), serde `skip`, `skip_serializing`,
  `skip_deserializing`, `default`, `skip_serializing_if` and `flatten`
  (`allOf`), doc comments as descriptions, and field-level
  `#[api_schema(description, example, minimum, maximum, min_length,
  max_length, pattern, min_items, max_items, format, deprecated)]`.

- `#[serde(transparent)]`, container `#[serde(default)]`,
  `deny_unknown_fields` (`additionalProperties: false`), `rename_all_fields`
  and variant-level `#[serde(untagged)]` are understood by the derive;
  `ApiSchema` does not support generic types (clear compile error).

- `Multipart::from_stream` and `Field::chunk`: streaming multipart uploads
  from a raw handler with a whole-body limit (declared lengths are refused
  early, chunked bodies are cut off at the limit, and a body that never yields
  a boundary or part headers is cut off after 256 KiB).
- `App::multipart_fields` / `MultipartField`: document the form fields of a
  `multipart/form-data` route (buffered or raw) in the OpenAPI document.

- `AppRuntime::serve_unix` (Unix): HTTP/1.1 on a Unix domain socket with the
  shared accept loop.
- `AppRuntime::h2c` (feature `http2`): HTTP/2 with prior knowledge on
  `serve_listener` / `serve_unix`, next to HTTP/1.1.

- Feature `compression-brotli`: `Compress` also negotiates `br` (quality
  values, brotli wins a tie; `Compress::brotli_quality`).

- Response helpers `ResponseExt` / `Headered` (headers, status, cookies on any
  response), `Redirect`, `Html`, `SetCookie` / `SameSite`; the `Cookies`
  extractor; `Form<T>` for urlencoded bodies (documented in OpenAPI).
- `AppRuntime::connect_info`, the `ConnectInfo` extractor, `peer_addr` and
  `RateLimit::key_by_peer_ip` (opt-in peer address, TCP only).
- `Sse` / `Event`: server-sent events with optional keep-alive comments.
- Feature `static-files`: `ServeDir` middleware (content types, validators and
  `304`, streaming, index files with a slash redirect, traversal, symlink and
  hidden-file protection including Windows 8.3 short names).
- `serde_urlencoded` is now a dependency (it backs `Form`).
- Feature `websocket`: `WebSocketUpgrade` / `WebSocket` / `Message` (handshake
  validation, origin allow-list, subprotocol negotiation, message size limit,
  automatic pongs, idle timeout) over `tokio-tungstenite`; an open session keeps
  its `max_connections` slot.
- `ErrorFormat` also carries `Sec-WebSocket-Version` over to its replacement.
- `pub use bytes::Bytes` (it appears in public types).

### Changed (internal)
- The accept loop tracks connections with a shutdown signal and a completion
  channel (as the TLS loop already did) instead of `GracefulShutdown::watch`,
  so upgraded connections can be served; shutdown behaviour is unchanged.

### Changed
- **Schema changes for existing derives** (they now match what serde writes):
  `#[serde(default)]` and `skip_serializing_if` fields are no longer
  `required` (and a query struct with a `default` field is parsed by serde, so a
  missing parameter no longer gives `400`); `#[serde(skip)]` fields are
  hidden; `#[serde(flatten)]` fields are merged (`allOf`, or
  `additionalProperties` for a map) instead of appearing as a property;
  `Option<T>` is nullable (`oneOf: [T, null]`) in request/response schemas
  (query parameters keep the plain type).
- The derive's generated code uses `__oas_*` local names, so neither a query
  field called `key`/`value`/`pair` nor a user constant called `registry`
  or `schema` breaks it.

## [0.7.0] - 2026-10-08

### Added
- `Cors` middleware: preflight handling, `Access-Control-*` headers,
  credentials, exposed headers, `Vary`.
- `TestResponse::header_all` (test-util) for headers that repeat.
- `Headers` extractor exposing the request's `HeaderMap`.
- `Compress` middleware (feature `compression`, gzip via `flate2`'s pure-Rust
  backend): content negotiation with quality values, size threshold, `Vary`,
  weak `ETag`, blocking-pool compression for large bodies.
- `TestResponse::body_bytes` (test-util).
- `RateLimit` middleware: token bucket per key (header or custom), `429` with
  `Retry-After`, bounded memory (keys are hashed; a full table is swept at most
  once per token interval).
- `ErrorFormat` middleware and `ErrorInfo`: rewrite framework-generated error
  responses to keep an existing error contract.

## [0.6.0] - 2026-10-08

### Added
- `AppRuntime::header_read_timeout` (default 30 s, `None` disables),
  `AppRuntime::max_connections` (default unlimited) and
  `AppRuntime::on_connection_error`, for plain HTTP and for TLS (including
  handshake failures and HTTP/2 connection errors).
- `CatchPanic` middleware: a handler panic becomes a `500` problem-details
  response instead of a dropped connection; `CatchPanic::with_hook` receives
  the panic message.

- `RequestId` middleware: keeps a usable `X-Request-Id` or generates one, exposes
  it to handlers and copies it onto every response (including 404/405).
- `constant_time_eq` and `BearerAuth::static_token` for comparing a shared
  bearer token without a timing side channel.
- `AppRuntime::http2_max_concurrent_streams` (feature `http2`): lowers the
  per-connection stream cap (Hyper's default is 200); streams over it are
  refused with `REFUSED_STREAM`.

- Named schemas go under `components.schemas` and are referenced with `$ref`:
  `ApiSchema::schema_with(&mut SchemaRegistry)` (default: `schema()`),
  `SchemaRegistry`, `#[api_schema(name = "...")]` and
  `BuildError::SchemaNameConflict`. Hand-written `ApiSchema` impls keep
  working and stay inline.
- Routes document the errors the framework returns (`400`, `413`, `415`, `401`) with
  a shared `Problem` schema; `OpenApiOptions::document_errors(false)` turns it
  off.

### Changed
- **OpenAPI document shape:** schemas of derived types are `$ref`s into
  `components.schemas` instead of inline objects, and error responses are
  added. Clients that compare the document byte for byte will see a change.
- **Behavior:** a client now has 30 seconds to send a complete request head,
  and an idle keep-alive connection is closed after the same time. Call
  `header_read_timeout(None)` to keep the old behavior.
- `tokio`'s `sync` feature is now enabled for the connection limit.

## [0.5.0] - 2026-10-08

### Added
- Scoped middleware: `App::group` / `Group` (prefixed routes with layers scoped
  to the prefix), `App::layer_for(prefix, layer)` and `App::route_layer(layer)`.
  Scopes match whole path segments (captures allowed), never match fewer
  requests than the router serves, and a request that no layer covers skips the
  middleware chain.
- HTTP/2 over TLS behind the new `http2` feature: `serve_tls` negotiates `h2`
  through ALPN and serves it with the same routing, middleware and graceful
  shutdown as HTTP/1.1. `TlsConfig::enable_http2(bool)` opts out. No h2c.
- `examples/tls.rs`, built in CI.

### Changed
- `TlsError` keeps its underlying cause as `std::error::Error::source` (for
  example an `io::Error` for a missing certificate file).

## [0.4.0] - 2026-10-08

### Added
- `AppRuntime::tcp_nodelay(bool)` to control `TCP_NODELAY` on accepted
  connections.

### Changed
- Behavior change: accepted connections now get `TCP_NODELAY` by default
  (`listen`, `serve_listener` and `serve_tls`), removing a roughly 40 ms
  Nagle/delayed-ACK stall for responses written in several small pieces
  (measured: 3-chunk streamed response 41 ms -> 6.5 ms; small-response
  throughput -3%..+1.5% across harnesses, within noise). Opt out with
  `AppRuntime::tcp_nodelay(false)`.

## [0.3.0] - 2026-10-08

### Added
- `serve_tls`, `TlsConfig` (`from_pem`, `from_pem_files`), `TlsError` and
  `AppRuntime::handshake_timeout` behind the new `tls` feature (`tokio-rustls`
  with the `ring` provider; HTTP/1.1, TLS 1.2/1.3).
- `App::body_limit` overrides the body-size limit of one route
  (`max_body_size` remains global and now says so).
- `Multipart` / `Field` extractor for `multipart/form-data` behind the new
  `multipart` feature (uses `multer`), with an OpenAPI request body.

## [0.2.0] - 2026-10-08

### Breaking
- `BuildError` is now `#[non_exhaustive]` and has a new
  `UnknownSecurityScheme { name }` variant.

### Added
- Middleware: `Middleware`, `Next`, `RequestBody` and `App::layer` for global
  layers that run before routing, plus the built-in `Trace` and `BearerAuth`
  layers. Zero cost without layers; the first layer adds about 120 ns and two
  allocations, each further layer about 40 ns and one allocation.
- OpenAPI security schemes: `OpenApiOptions::{security_scheme, bearer_auth,
  api_key, default_security}`, per-route `App::security` / `App::public`, and
  the `SecurityScheme` / `ApiKeyLocation` types. Generates
  `components.securitySchemes` and `security`; `build()` rejects undeclared
  schemes. Documentation only: no credentials are checked.
- Graceful shutdown: `serve_listener` waits for in-flight requests after the
  shutdown signal, bounded by `AppRuntime::shutdown_timeout` (default 30 s).

### Fixed
- A transient `accept` error (for example `EMFILE`) no longer terminates the
  whole server.

## [0.1.0] - 2026-10-08

First public release.

### Features
- Typed routing on Hyper and Tokio with startup-generated OpenAPI 3.1 metadata
  and an opt-in Swagger UI (pinned `swagger-ui-dist@5.17.14` with Subresource
  Integrity hashes).
- Extractors (`Path`, `Query`, `Header`, `Json`, `State`, `Params`) and response
  types with RFC 9457-style `ApiError`.
- `#[derive(ApiSchema)]` for structs and unit-variant enums, with
  `#[serde(rename)]` / `#[serde(rename_all)]` support. Built-in schemas cover
  `String`, `bool`, integers, `f32`/`f64`, `Uuid`, `Option`, `Vec`, `Box`,
  string-keyed maps and `serde_json::Value`.

### Quality
- Inline-future safety invariants are enforced with `assert!`, documented with
  `SAFETY` comments, and checked by Miri in CI.
- CI runs fmt, clippy (default and all features), tests, doctests, examples,
  rustdoc with warnings denied, and `cargo-deny`.
- `scripts/verify-docker.sh` runs the same gates locally in Docker.
- MIT license, `CONTRIBUTING.md`, crate metadata and a minimum supported Rust
  version of 1.88.
