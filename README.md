# oas-rs

Typed HTTP routing on Hyper + Tokio with startup-generated OpenAPI 3.1
metadata. The V1 release line is Cargo `0.1.0`; the public API and HTTP
semantics are frozen for the `0.1` line. Licensed under the MIT License.

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

`#[derive(ApiSchema)]` currently supports structs with named fields whose types
are scalars (`String`, `bool`, integers, floats, `Uuid`) or `Option` of those.
Collections, enums and serde attributes such as `#[serde(rename)]` are not yet
reflected in the generated schema; see the roadmap below.

Swagger UI is loaded from a pinned `swagger-ui-dist` release on unpkg with
Subresource Integrity hashes, so it needs network access from the browser.

## Distribution through the Quangt registry

Released crates are distributed through the static sparse registry at
`storage.quangt.com`. A consuming project configures the registry once:

```toml
# .cargo/config.toml
[registries.quangt]
index = "sparse+https://storage.quangt.com/cargo/index/"
```

Then it can depend on the release without a path checkout:

```toml
[dependencies]
oas-rs = { version = "=0.1.0", registry = "quangt" }
```

The proc-macro crate is published to the same registry and is resolved
automatically as an internal dependency. Public dependencies such as Hyper,
Tokio, Serde, and Bytes continue to come from crates.io. The release artifacts
and sparse index files can be prepared with:

```powershell
cargo package -p oas-rs-macros --registry quangt --locked
pwsh -File .\scripts\generate-sparse-index.ps1 -CrateNames oas-rs-macros
# Upload the macro crate and its index entry to R2 before packaging oas-rs.
cargo package -p oas-rs --registry quangt --locked
pwsh -File .\scripts\generate-sparse-index.ps1 -CrateNames oas-rs
```

Upload the resulting `dist/cargo/index` and `dist/cargo/crates` trees to R2.
When adding a version, invalidate the CDN cache for the changed index object.
The `.crate` files contain source code and are compiled by the consuming
project.

For the current `0.1.0` release, upload these objects under the `cargo/`
prefix:

```text
cargo/index/config.json
cargo/index/oa/s-/oas-rs
cargo/index/oa/s-/oas-rs-macros
cargo/crates/oas-rs/0.1.0/oas-rs-0.1.0.crate
cargo/crates/oas-rs-macros/0.1.0/oas-rs-macros-0.1.0.crate
```

The release download endpoint is
`https://storage.quangt.com/cargo/crates/{crate}/{version}/{crate}-{version}.crate`.

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
cargo test --workspace --all-targets --features 'uuid test-util swagger'
cargo test --doc --workspace
cargo build --workspace --examples --features 'uuid swagger'
```

The Miri inline-future safety job is a permanent CI gate.

Documentation is built with `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
--no-deps --all-features`. Run `bash scripts/verify-docker.sh` to execute all of
the above in Docker with the CI toolchain (Rust 1.88, the minimum supported
version) before committing.

## Roadmap

Planned after `0.1`: `ApiSchema` support for `Vec`, enums and serde renames,
splitting `src/lib.rs` into modules, middleware, TLS, and the full HTTP
acceptance benchmark matrix.

## Contributing and license

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CHANGELOG.md](CHANGELOG.md). This
project is licensed under the [MIT License](LICENSE-MIT).
