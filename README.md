# oas-rs

Typed HTTP routing on Hyper + Tokio with startup-generated OpenAPI 3.1
metadata. The V1 release line is Cargo `0.1.0`; the current release target is
internal use while the public API and HTTP semantics are being frozen.

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

Buffered JSON/body extractors have a default 1 MiB limit. Configure it with
`app.max_body_size(bytes)` before route registration. Raw handlers receive
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

## Performance workflow

The core repository contains the release-profile router microbenchmark used as
the developer regression detector:

```bash
cargo bench --bench router --features uuid,test-util,swagger
```

It covers static/dynamic routing, route scaling, extraction, response
construction, allocations, and 404/405/OPTIONS dispatch. HTTP acceptance is a
separate Linux lab at `../oas-rs-perf`, comparing raw Hyper with `oas-rs` over
real TCP/Hyper/Tokio connections. The current diagnostic reference is about a
`-0.90%` throughput delta and `+0.54%` p95 overhead; the full 7-run, 1M-request
matrix is a deferred performance milestone rather than a V1 blocker.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --features 'uuid test-util swagger'
cargo test --doc --workspace
cargo build --workspace --examples --features 'uuid swagger'
```

The Miri inline-future safety job is a permanent CI gate.
