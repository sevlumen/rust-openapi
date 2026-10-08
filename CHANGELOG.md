# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
