# oas-rs

Typed HTTP routing on Hyper + Tokio with startup-generated OpenAPI 3.1 metadata.

```rust
use oas_rs::{App, Json, OpenApi, Path};
use uuid::Uuid;

#[derive(serde::Serialize, OpenApi)]
struct User { id: Uuid }

async fn get_user(Path(id): Path<Uuid>) -> Json<User> {
    Json(User { id })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut app = App::new().title("Users API").version("1.0.0");
    app.get("/users/{id}", get_user).tag("Users").summary("Get user");
    app.openapi("/openapi.json").swagger("/swagger");
    app.listen("0.0.0.0:8080").await
}
```

Verification:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo test --doc --workspace
cargo build --workspace --examples
```

Docker benchmark smoke:

```powershell
./benchmarks/run-benchmark.ps1 -Iterations 10000 -Runs 1 -Vus 32 -Version smoke
```

Use the default arguments for the release matrix. Reports remain `INCONCLUSIVE`
until the acceptance plan's seven paired runs, baseline CV and confidence bounds
are available.

The release-profile router microbench also checks static route counts at
1/10/100/1,000/10,000 routes and asserts zero extra allocations/bytes for
typed path, query, and header extraction against request-shape-matched raw
comparators.

Streaming is opt-in with `StreamResponse<S>`; ordinary text and JSON responses
continue to use fixed `Bytes` bodies.

Handlers that need an upload or streaming request body can use
`app.raw(Method::POST, "/upload", handler)` (or the `raw_get` shorthand),
where `handler` receives `Request<hyper::body::Incoming>` directly. The
framework does not collect that body; the raw handler owns its limits and
cancellation policy.
