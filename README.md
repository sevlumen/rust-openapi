# oas-rs

Typed HTTP routing on Hyper + Tokio with startup-generated OpenAPI 3.1
metadata. The V1 release line is Cargo `0.1.0`; the public API and HTTP
semantics follow semver within the `0.3` line. Licensed under the MIT License.

## Quick start

```rust
use oas_rs::App;

async fn hello() -> &'static str {
    "hello"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut app = App::new();
    app.get("/", hello);
    app.openapi().title("Hello API").version("1.0.0");

    let runtime = app.build()?;
    runtime.listen("0.0.0.0:8080").await
}
```

The canonical examples are kept in [`examples/hello.rs`](examples/hello.rs)
and [`examples/users-api.rs`](examples/users-api.rs). Run them with:

```bash
cargo run --example hello --features swagger
cargo run --example users-api --features 'uuid swagger'
```

## Builder and runtime lifecycle

`App` is the mutable registration builder. `App::build()` compiles the
registered routes and returns an immutable `AppRuntime`. Only the runtime
serves requests:

```rust
let mut app = App::new();
app.get("/users/{id}", get_user);
app.post("/users", create_user);

app.openapi()
    .title("Users API")
    .version("1.0.0");

let runtime = app.build()?;
runtime.listen("0.0.0.0:8080").await?;
# Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
```

`AppRuntime::serve_listener` accepts an already-bound Tokio listener and a
shutdown future. The default OpenAPI endpoint is `/openapi.json`.

### TCP_NODELAY

Accepted connections get `TCP_NODELAY` by default. Without it, a response that
is written in several small pieces (streamed or chunked bodies) can stall for
about 40 ms on Linux while the kernel waits for a delayed ACK; with it that
stall disappears and small-response throughput showed no measurable change.
Use `app.build()?.tcp_nodelay(false)` to leave sockets as accepted (they then
inherit the listener's setting). This applies to every serving entry point:
`listen`, `serve_listener` and `serve_tls`.

### Graceful shutdown

When the shutdown future completes, the server stops accepting connections,
closes idle keep-alive connections, and waits for in-flight requests to finish
before `serve_listener` returns. The wait is bounded by
`AppRuntime::shutdown_timeout` (default 30 seconds); connections still busy
after that are abandoned. Transient `accept` errors (aborted connections,
out of file descriptors) no longer stop the server.

```rust
let runtime = app.build()?.shutdown_timeout(std::time::Duration::from_secs(10));
runtime.serve_listener(listener, async { tokio::signal::ctrl_c().await.ok(); }).await?;
```

## Routing

Use `get`, `post`, `put`, `patch`, `delete`, `head`, and `options`. Static
segments take precedence over dynamic segments, and a trailing slash is
normalized. `HEAD` falls back to `GET` when no explicit `HEAD` route exists.
Automatic `OPTIONS` returns `204 No Content` with an `Allow` header; an
explicit `OPTIONS` route takes precedence. Unknown paths return `404`, while
known paths with an unsupported method return `405` and `Allow`.

Duplicate route/method pairs are rejected during registration. A dynamic route
supports at most eight path captures; a route exceeding that limit is rejected
by `build()` with `BuildError::TooManyCaptures`.

## Extractors and state

Built-in extractors include `Path<T>`, `Query<T>`, `Header<T>`, `Json<T>`, and
`State<T>`. Multi-capture routes can use `Params`; typed path/query/header
values are decoded and validated before the handler runs. Query `+` is treated
as a literal plus, not as a space.

Install application state before registering routes:

```rust
let mut app = App::new().with_state(Database::new());
app.get("/users/{id}", get_user);
```

Buffered JSON/body extractors have a default 1 MiB limit. `app.max_body_size(bytes)`
changes it for every buffered route; `app.post(...).body_limit(bytes)` overrides
it for the route just registered. Raw handlers receive
Hyper's streaming `Incoming` body directly and own their upload limit and
cancellation policy.

## Responses and errors

Handlers can return text, `Bytes`, `Json<T>`, `JsonBytes`, `Created<T>`,
`NoContent`, `NotModified`, `StreamResponse<S>`, or `Result<T, ApiError>`.
`ApiError` renders a problem-details-compatible JSON response. `Json<T>` uses
`application/json`; JSON requests must send exactly that media type, including
optional parameters such as `charset`.

## OpenAPI and Swagger

Calling `app.openapi()` enables OpenAPI generation and configures its title,
version, description, and path. Swagger UI is independent and opt-in:

```rust
app.openapi().title("Users API").version("1.0.0");
app.swagger().path("/swagger");
```

Enable `swagger` for the UI and `uuid` for UUID extraction/schema support.
`ApiSchema` derives are provided by the companion `oas-rs-macros` crate.

`#[derive(ApiSchema)]` supports structs with named fields. Field types may be
scalars (`String`, `bool`, integers, `f32`/`f64`, `Uuid`), `Option`, `Vec`,
`Box`, string-keyed maps, `serde_json::Value`, or another `ApiSchema` type.
Unit-variant enums derive a string `enum` schema. `#[serde(rename = "...")]`
and `#[serde(rename_all = "...")]` (all serde casing rules) are honored for
schema names, query parameter names and the direct query parser. Other serde
attributes (`flatten`, `skip`, tagged enums) are not yet reflected; see the
roadmap below.

Swagger UI is loaded from a pinned `swagger-ui-dist` release on unpkg with
Subresource Integrity hashes, so it needs network access from the browser.

### Security schemes

Declare the authentication your API expects so the generated document lists
`components.securitySchemes` and Swagger UI shows its **Authorize** button.
This only *describes* authentication; `oas-rs` does not check credentials.

```rust
use oas_rs::ApiKeyLocation;

app.openapi()
    .bearer_auth("BearerAuth")
    // `TenantId` / `X-Tenant-Id` are only an *example* of a custom API-key header.
    .api_key("TenantId", ApiKeyLocation::Header, "X-Tenant-Id")
    .default_security(["BearerAuth"]); // applies to every route

app.post("/firmwares", upload).security(["BearerAuth", "TenantId"]); // both required
app.get("/health", health).public(); // no authentication
```

`.security([...])` requires every listed scheme together; calling it again adds
an alternative (logical OR). Referencing a scheme that was never declared makes
`build()` return `BuildError::UnknownSecurityScheme`. For other kinds, use
`security_scheme(name, SecurityScheme::...)` (`bearer_with_format("JWT")`,
`basic()`, `api_key(...)`).

## TLS

Enable the `tls` feature to serve HTTPS directly with `rustls` and the `ring`
crypto provider (no OpenSSL dependency; `ring` compiles a small amount of C, so
a C compiler must be available at build time):

```rust
use oas_rs::TlsConfig;
use std::time::Duration;

let tls = TlsConfig::from_pem_files("cert.pem", "key.pem")?; // or TlsConfig::from_pem(cert_bytes, key_bytes)
let runtime = app
    .build()?
    .handshake_timeout(Duration::from_secs(10)) // default 10 s
    .shutdown_timeout(Duration::from_secs(30)); // default 30 s
let listener = tokio::net::TcpListener::bind("0.0.0.0:8443").await?;
runtime
    .serve_tls(listener, tls, async { tokio::signal::ctrl_c().await.ok(); })
    .await?;
```

- The certificate file may contain a chain; the private key may be PKCS#8,
  PKCS#1 or SEC1 PEM (parsed by `rustls-pki-types`). A missing file, malformed
  PEM or a key that does not match the certificate returns a `TlsError`.
- TLS 1.2 and 1.3 with rustls' safe defaults; HTTP/1.1 only: ALPN advertises
  only `http/1.1`, so a client that offers only `h2` is rejected during the
  handshake. No client certificates.
- The handshake runs per connection, so a slow client never blocks accepting
  others, and a failed or timed-out handshake closes only that connection.
- Shutdown behaves like `serve_listener`: stop accepting, let in-flight
  requests finish, bounded by `shutdown_timeout`; idle keep-alive connections
  are closed and half-finished handshakes are dropped.
- On a loopback benchmark, steady-state TLS throughput was about 9% below plain
  TCP; with short connections the handshake dominates (see
  `docs/tls-design.md`). Reuse connections where you can.

## Multipart uploads

Enable the `multipart` feature to accept `multipart/form-data` (for example
firmware uploads). The body is buffered up to the route's size limit, then read
part by part:

```rust
use oas_rs::{ApiError, Json, Multipart};

async fn upload(mut form: Multipart) -> Result<Json<serde_json::Value>, ApiError> {
    let mut stored = Vec::new();
    while let Some(field) = form.next_field().await? {
        let file_name = field.file_name().map(str::to_owned);
        let data = field.bytes().await?; // inside the route's body limit
        stored.push(serde_json::json!({ "file": file_name, "bytes": data.len() }));
    }
    Ok(Json(serde_json::json!(stored)))
}

app.openapi().bearer_auth("BearerAuth"); // declare the scheme used below
app.post("/firmwares", upload)
    .body_limit(4 * 1024 * 1024) // this route only
    .security(["BearerAuth"]);
```

- `app.max_body_size(bytes)` changes the limit of **every** buffered route
  (including earlier `.body_limit(...)` overrides, so call it before the
  routes); `.body_limit(bytes)` overrides it for the route just registered.
- A body over the limit returns `413`, a non-multipart `Content-Type` returns
  `415`, a missing boundary or malformed body returns `400`.
- `Field::file_name` returns the client-supplied name as sent (only a
  backslash-escaped quote `\"` is unescaped); never use it as a filesystem
  path without sanitizing it.
- Drop (or consume with `bytes()`/`text()`) the previous `Field` before calling
  `next_field()` again; holding it is a handler bug and returns `500`.
- Uploads are buffered, not streamed: budget about 3x the route's `body_limit`
  of memory per concurrent upload. For very large files use a raw handler.
- The route is documented in OpenAPI as a `multipart/form-data` request body.

## Middleware

Register global layers with `App::layer`. A layer is an `async fn` (or any type
implementing `Middleware`) that receives the request and a `Next` handle:

```rust
use oas_rs::{BearerAuth, Next, RequestBody, Trace};
use http::Request;

async fn timing(request: Request<RequestBody>, next: Next) -> oas_rs::HttpResponse {
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    eprintln!("{} in {:?}", response.status(), started.elapsed());
    response
}

app.layer(Trace::stderr()); // outermost: first registered runs first
app.layer(
    BearerAuth::new(|token: String| async move {
        // Validate against your own store, comparing secrets in constant time
        // (or compare hashes). Return an ApiError to reject.
        if token == "secret" {
            Ok(())
        } else {
            Err(oas_rs::ApiError::new(http::StatusCode::UNAUTHORIZED, "Unauthorized", "bad token"))
        }
    })
    .exempt_paths(["/health", "/openapi.json"]),
);
app.layer(timing);
```

Layers run before routing, so they also see `404`, `405`, automatic `OPTIONS`
and `HEAD` requests. A layer may answer without calling `next` (for example
`401`). It can read or change the request head and read the body (which
consumes it for the handler and extractors downstream), but cannot substitute a
different body. There is no per-route layer yet, and panics are
not caught. `BearerAuth` *enforces* a bearer token; `OpenApiOptions::bearer_auth`
only *documents* the scheme, so use both for a protected, documented API.

With no layers registered, the request path is unchanged. The first layer adds a
fixed cost of about 120 ns and two allocations (the boxed end of the chain);
each further layer adds about 40 ns and one allocation (see
`docs/middleware-design.md` for measurements).

`BearerAuth` answers `401` to any non-exempt request without a token, including
CORS preflight `OPTIONS` requests, which browsers send without credentials.
Register a layer that answers preflights before `BearerAuth`, or list the
paths in `exempt_paths`. `Trace::new(|record| ...)` receives the method, path,
status and elapsed time of every request.

## Installation

```toml
[dependencies]
oas-rs = "0.3"
```

Enable optional features as needed: `swagger` (Swagger UI), `uuid` (UUID
extraction and schema support), `multipart` (`multipart/form-data` uploads),
`tls` (HTTPS via `serve_tls`), `test-util` (in-process `oneshot` testing).
The minimum supported Rust version is 1.88.

## Performance

The core repository contains the release-profile router microbenchmark used as
the developer regression detector (design notes in
[`docs/benchmark-design.md`](docs/benchmark-design.md)):

```bash
cargo bench --bench router --features uuid,test-util,swagger
```

It covers static/dynamic routing, route scaling, extraction, response
construction, allocations, and 404/405/OPTIONS dispatch. HTTP acceptance is a
separate Linux lab at `../oas-rs-perf`, comparing raw Hyper with `oas-rs` over
real TCP/Hyper/Tokio connections. The current diagnostic reference is about a
`-0.90%` throughput delta and `+0.54%` p95 overhead; the full 7-run, 1M-request
matrix is a deferred performance milestone rather than a V1 blocker.

Reference microbenchmark numbers (Linux container, release profile): a static
route costs about 256 ns per request against about 239 ns for a raw handler,
with 3 allocations per request (raw: 5). Static and dynamic routing time stays
flat from 1 to 10,000 routes. Treat these as relative signals; run the
benchmark on your own hardware for absolute values.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --features 'uuid test-util swagger multipart tls'
cargo test --doc --workspace
cargo build --workspace --examples --features 'uuid swagger'
```

The Miri inline-future safety job is a permanent CI gate.

Documentation is built with `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
--no-deps --all-features`. Run `bash scripts/verify-docker.sh` to execute all of
the above in Docker with the CI toolchain (Rust 1.88, the minimum supported
version) before committing.

## Roadmap

Planned after `0.1`: `ApiSchema` support for data-carrying enums and more
serde attributes,
splitting `src/lib.rs` into modules, middleware, TLS, and the full HTTP
acceptance benchmark matrix.

## Contributing and license

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CHANGELOG.md](CHANGELOG.md). This
project is licensed under the [MIT License](LICENSE-MIT).
