# Multipart Upload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `Multipart` extractor (feature `multipart`, backed by `multer`) and a per-route `App::body_limit`, with an OpenAPI `multipart/form-data` request body.

**Architecture:** The buffered request body (`Bytes`, already capped by the route's limit) is wrapped in a one-item `Stream` and handed to `multer::Multipart`; thin wrapper types (`Multipart`, `Field`) keep `multer` out of the public API. `body_limit` edits the last registered buffered route's plan.

**Tech Stack:** Rust 1.88, `multer` 3.1 (optional), existing `futures-core`, Hyper/Tokio.

**Spec:** `docs/multipart-design.md`

## Global Constraints

- No new dependency in the default feature set; `multer = { version = "3", optional = true }` behind feature `multipart`.
- MSRV 1.88, edition 2024. `cargo deny check` must pass with default features and `--all-features`.
- Default build: `plaintext` stays ~250 ns/op with exactly 3 allocations/op; re-measure with and without the feature.
- Verification: `bash scripts/verify-docker.sh` (this plan updates it to include the `multipart` feature); quick loop: `cargo test --features "uuid test-util swagger multipart" <filter>`. Use `python` (not `python3`).
- Multipart errors are `ApiError` problem-details: wrong media type `415`, missing/invalid boundary or malformed body `400`, oversize `413`.

## Review Focus

1. A part whose content contains the boundary text or CRLF sequences must not be split or truncated (test with such content).
2. A truncated body (no closing boundary) returns `400` from the handler's reads, never hangs or panics.
3. File names with quotes, unicode and path separators come back verbatim; the framework never touches the filesystem.
4. A body exactly at the route limit is accepted and limit+1 returns `413`, per route, without changing other routes' limits.
5. Missing `Content-Type`, `multipart/mixed`, and a quoted boundary (`boundary="abc"`) behave correctly (415, 415, accepted).

---

### Task 1: Per-route `App::body_limit`

**Files:**
- Modify: `src/app.rs` (new method; fix `max_body_size` docs), `README.md` (body-size sentence)
- Test: `tests/body_limit.rs` (new; no feature needed beyond `test-util`)
- Modify: `Cargo.toml` (`[[test]] name = "body_limit"`, `required-features = ["test-util"]`)

**Interfaces:**
- Produces: `App::body_limit(&mut self, bytes: usize) -> &mut Self` — sets the limit of the last registered route if it has a buffered body; no effect otherwise.

- [ ] **Step 1: Write the failing test**

Create `tests/body_limit.rs`:

```rust
use bytes::Bytes;
use oas_rs::{App, Json, Method};
use serde_json::Value;

async fn echo(Json(_body): Json<Value>) -> &'static str {
    "ok"
}

fn json_of_len(len: usize) -> Bytes {
    // {"a":"xxx..."} with `len` bytes in total.
    let filler = "x".repeat(len - 8);
    Bytes::from(format!("{{\"a\":\"{filler}\"}}"))
}

async fn post(runtime: &oas_rs::AppRuntime, uri: &str, body: Bytes) -> u16 {
    runtime
        .oneshot(
            Method::POST,
            uri,
            &[("content-type", "application/json")],
            Some(body),
        )
        .await
        .status()
        .as_u16()
}

#[tokio::test]
async fn body_limit_applies_to_one_route_only() {
    let mut app = App::new();
    app.post("/small", echo);
    app.post("/big", echo).body_limit(2 * 1024 * 1024);
    let runtime = app.build().unwrap();

    let default_limit = 1024 * 1024;
    assert_eq!(post(&runtime, "/small", json_of_len(default_limit)).await, 200);
    assert_eq!(post(&runtime, "/small", json_of_len(default_limit + 1)).await, 413);
    assert_eq!(post(&runtime, "/big", json_of_len(default_limit + 1)).await, 200);
    assert_eq!(post(&runtime, "/big", json_of_len(2 * 1024 * 1024)).await, 200);
    assert_eq!(post(&runtime, "/big", json_of_len(2 * 1024 * 1024 + 1)).await, 413);
}

#[tokio::test]
async fn body_limit_can_also_lower_a_routes_limit() {
    let mut app = App::new();
    app.post("/tiny", echo).body_limit(64);
    app.post("/normal", echo);
    let runtime = app.build().unwrap();

    assert_eq!(post(&runtime, "/tiny", json_of_len(64)).await, 200);
    assert_eq!(post(&runtime, "/tiny", json_of_len(65)).await, 413);
    assert_eq!(post(&runtime, "/normal", json_of_len(65)).await, 200);
}

#[tokio::test]
async fn body_limit_on_a_route_without_a_buffered_body_is_a_no_op() {
    async fn hello() -> &'static str {
        "hello"
    }
    let mut app = App::new();
    app.get("/", hello).body_limit(10);
    let runtime = app.build().unwrap();
    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 200);
}
```

- [ ] **Step 2: Confirm RED**

Add to `Cargo.toml`:

```toml
[[test]]
name = "body_limit"
required-features = ["test-util"]
```

Run: `cargo test --features "uuid test-util swagger" --test body_limit`
Expected: FAIL to compile (`no method named body_limit`).

- [ ] **Step 3: Implement**

In `src/app.rs`, after `max_body_size`:

```rust
/// Sets the body-size limit of the last registered route, overriding the
/// application-wide limit for that route only. It has no effect on routes
/// without a buffered body (raw handlers, handlers that read no body).
///
/// Call it right after registering the route. A later call to
/// [`max_body_size`](Self::max_body_size) resets every buffered route,
/// including this one.
pub fn body_limit(&mut self, limit: usize) -> &mut Self {
    let limit = encode_body_limit(limit);
    if let Some(index) = self.last_route
        && let Some(plan) = self.plans.get_mut(index)
        && matches!(plan.body_mode, BodyMode::Buffered)
    {
        plan.body_limit = limit;
    }
    self
}
```

Correct the doc comment of `max_body_size` to state that it applies to every buffered route (already registered and later), and that `body_limit` is the per-route override. If `self.plans.get_mut(index)` does not correspond to the last route (the first test fails on `/big`), inspect where `last_route` is assigned in `App` registration and align the index with `plans`.

- [ ] **Step 4: Verify GREEN, full suite, commit**

Run: `cargo test --features "uuid test-util swagger"` then `bash scripts/verify-docker.sh`
Expected: all pass.

In `README.md`, change the body-size sentence to: "Buffered JSON/body extractors have a default 1 MiB limit. `app.max_body_size(bytes)` changes it for every buffered route; `app.post(...).body_limit(bytes)` overrides it for one route."

```bash
git add -A
git commit -m "feat: add per-route App::body_limit"
```

---

### Task 2: `Multipart` extractor behind the `multipart` feature

**Files:**
- Create: `src/multipart.rs`
- Modify: `Cargo.toml` (optional dep, feature, `[[test]]`), `src/lib.rs` (`#[cfg(feature = "multipart")] mod multipart;` and re-exports), `scripts/verify-docker.sh` and `.github/workflows/ci.yml` (add `multipart` to feature lists), `README.md` Verification commands, `CLAUDE.md` command list
- Test: `tests/multipart.rs`

**Interfaces:**
- Consumes: Task 1's `body_limit`; `FromRequest`, `OpenApiRequest`, `ApiError`.
- Produces: `Multipart` with `async fn next_field(&mut self) -> Result<Option<Field>, ApiError>` and `Field` with `name() -> Option<&str>`, `file_name() -> Option<&str>`, `content_type() -> Option<&str>`, `async fn bytes(self) -> Result<Bytes, ApiError>`, `async fn text(self) -> Result<String, ApiError>`.

- [ ] **Step 1: Write the failing tests**

Create `tests/multipart.rs`:

```rust
use bytes::Bytes;
use oas_rs::{ApiError, App, Method, Multipart};

struct Part<'a> {
    name: &'a str,
    file_name: Option<&'a str>,
    content_type: Option<&'a str>,
    data: &'a [u8],
}

fn multipart_body(boundary: &str, parts: &[Part<'_>]) -> Bytes {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(file_name) = part.file_name {
            disposition.push_str(&format!("; filename=\"{file_name}\""));
        }
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = part.content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Bytes::from(body)
}

/// Echoes `name|file_name|content_type|length` per part, one line each.
async fn summarize(mut form: Multipart) -> Result<String, ApiError> {
    let mut lines = Vec::new();
    while let Some(field) = form.next_field().await? {
        let name = field.name().unwrap_or("-").to_owned();
        let file_name = field.file_name().unwrap_or("-").to_owned();
        let content_type = field.content_type().unwrap_or("-").to_owned();
        let data = field.bytes().await?;
        lines.push(format!("{name}|{file_name}|{content_type}|{}", data.len()));
    }
    Ok(lines.join("\n"))
}

fn runtime() -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.post("/upload", summarize);
    app.build().unwrap()
}

async fn upload(
    runtime: &oas_rs::AppRuntime,
    content_type: Option<&str>,
    body: Bytes,
) -> (u16, String) {
    let headers: Vec<(&str, &str)> = content_type.map(|v| ("content-type", v)).into_iter().collect();
    let response = runtime
        .oneshot(Method::POST, "/upload", &headers, Some(body))
        .await;
    let status = response.status().as_u16();
    (status, response.body_string().await)
}

#[tokio::test]
async fn reads_text_and_binary_parts() {
    let body = multipart_body(
        "XBOUNDARYX",
        &[
            Part { name: "note", file_name: None, content_type: None, data: b"hello" },
            Part {
                name: "firmware",
                file_name: Some("fw.bin"),
                content_type: Some("application/octet-stream"),
                data: &[0u8, 255, 10, 13, 0, 1],
            },
        ],
    );
    let (status, text) = upload(&runtime(), Some("multipart/form-data; boundary=XBOUNDARYX"), body).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(
        text,
        "note|-|-|5\nfirmware|fw.bin|application/octet-stream|6"
    );
}

#[tokio::test]
async fn wrong_media_type_is_415_and_missing_boundary_is_400() {
    let runtime = runtime();
    let body = multipart_body("b", &[Part { name: "a", file_name: None, content_type: None, data: b"1" }]);
    assert_eq!(upload(&runtime, Some("application/json"), body.clone()).await.0, 415);
    assert_eq!(upload(&runtime, Some("multipart/mixed; boundary=b"), body.clone()).await.0, 415);
    assert_eq!(upload(&runtime, None, body.clone()).await.0, 415);
    assert_eq!(upload(&runtime, Some("multipart/form-data"), body).await.0, 400);
}

#[tokio::test]
async fn quoted_boundary_is_accepted() {
    let body = multipart_body("abc", &[Part { name: "a", file_name: None, content_type: None, data: b"1" }]);
    let (status, text) = upload(&runtime(), Some("multipart/form-data; boundary=\"abc\""), body).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(text, "a|-|-|1");
}
```

- [ ] **Step 2: Confirm RED**

Add to `Cargo.toml`:

```toml
[[test]]
name = "multipart"
required-features = ["multipart", "test-util"]
```

Run: `cargo test --features "uuid test-util swagger multipart" --test multipart`
Expected: FAIL (`unknown feature multipart` until Step 3 adds it, then unresolved `oas_rs::Multipart`).

- [ ] **Step 3: Implement**

`Cargo.toml`: add `multer = { version = "3", optional = true }` under `[dependencies]` and `multipart = ["dep:multer"]` under `[features]`.

`src/lib.rs`: add

```rust
#[cfg(feature = "multipart")]
mod multipart;
#[cfg(feature = "multipart")]
pub use multipart::{Field, Multipart};
```

Create `src/multipart.rs`:

```rust
use futures_core::Stream;

use crate::*;

/// A one-item stream over an already buffered body.
struct OnceBody(Option<Bytes>);

impl Stream for OnceBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take().filter(|bytes| !bytes.is_empty()).map(Ok))
    }
}

fn map_error(error: multer::Error) -> ApiError {
    match error {
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large",
                error.to_string(),
            )
        }
        other => ApiError::bad_request(format!("invalid multipart body: {other}")),
    }
}

/// A `multipart/form-data` request body. Extract it in a handler and read the
/// parts with [`next_field`](Self::next_field). The body is buffered up to the
/// route's size limit (see `App::body_limit`).
pub struct Multipart {
    inner: multer::Multipart<'static>,
}

impl Multipart {
    /// The next part, or `None` after the last one.
    pub async fn next_field(&mut self) -> Result<Option<Field>, ApiError> {
        self.inner
            .next_field()
            .await
            .map(|field| field.map(|inner| Field { inner }))
            .map_err(map_error)
    }
}

/// One part of a multipart body.
pub struct Field {
    inner: multer::Field<'static>,
}

impl Field {
    pub fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    /// The client-supplied file name, returned verbatim (never used by the framework).
    pub fn file_name(&self) -> Option<&str> {
        self.inner.file_name()
    }

    pub fn content_type(&self) -> Option<&str> {
        self.inner.content_type().map(|mime| mime.as_ref())
    }

    pub async fn bytes(self) -> Result<Bytes, ApiError> {
        self.inner.bytes().await.map_err(map_error)
    }

    pub async fn text(self) -> Result<String, ApiError> {
        self.inner.text().await.map_err(map_error)
    }
}

impl<S: Send + Sync + 'static> FromRequest<S> for Multipart {
    const NEEDS_BODY: bool = true;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest {
            request_body: Some(json!({
                "required": true,
                "content": {
                    "multipart/form-data": {
                        "schema": {
                            "type": "object",
                            "additionalProperties": { "type": "string", "format": "binary" }
                        }
                    }
                }
            })),
            ..OpenApiRequest::default()
        }
    }

    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let boundary = multer::parse_boundary(content_type).map_err(|error| match error {
            multer::Error::NoBoundary => {
                ApiError::bad_request("multipart Content-Type has no boundary")
            }
            _ => ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Media Type",
                "expected multipart/form-data",
            ),
        })?;
        let body = std::mem::take(request.body_mut());
        Ok(Multipart {
            inner: multer::Multipart::new(OnceBody(Some(body)), boundary),
        })
    }
}
```

If the compiler reports that the handler future is not `Send`, ledger a ruling and wrap the parser so the extractor stays `Send` (the public API does not change).

Update feature lists: in `scripts/verify-docker.sh` and `.github/workflows/ci.yml` every `--features "uuid test-util swagger"` becomes `--features "uuid test-util swagger multipart"` (and the examples build keeps `"uuid swagger"`); in `README.md` Verification and `CLAUDE.md` Commands do the same.

- [ ] **Step 4: Verify GREEN, full suite, commit**

Run: `cargo test --features "uuid test-util swagger multipart" --test multipart` then `bash scripts/verify-docker.sh`
Expected: 3 passed, then every gate passes.

```bash
git add -A
git commit -m "feat: add Multipart extractor (feature multipart)"
```

---

### Task 3: Edge cases and the real-TCP upload

**Files:**
- Test: `tests/multipart.rs`

**Interfaces:** Consumes Task 1's `body_limit` and Task 2's types.

- [ ] **Step 1: Write the tests**

Append to `tests/multipart.rs`:

```rust
#[tokio::test]
async fn part_content_may_contain_the_boundary_text_and_crlf() {
    let tricky = b"line1\r\n--XB-not-a-boundary\r\n--XBOUNDARY\r\nmore\r\n";
    let body = multipart_body(
        "XB",
        &[Part { name: "f", file_name: Some("a.txt"), content_type: None, data: tricky }],
    );
    let (status, text) = upload(&runtime(), Some("multipart/form-data; boundary=XB"), body).await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(text, format!("f|a.txt|-|{}", tricky.len()));
}

#[tokio::test]
async fn truncated_body_is_400_not_a_hang() {
    let full = multipart_body(
        "XB",
        &[Part { name: "f", file_name: None, content_type: None, data: b"0123456789" }],
    );
    let truncated = full.slice(..full.len() - 12);
    let (status, text) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        upload(&runtime(), Some("multipart/form-data; boundary=XB"), truncated),
    )
    .await
    .expect("truncated multipart body hung");
    assert_eq!(status, 400, "{text}");
}

#[tokio::test]
async fn file_names_come_back_verbatim() {
    for name in ["quo\\\"te.bin", "ünï-コード.bin", "../../etc/passwd", "a b;c.bin"] {
        let body = multipart_body(
            "XB",
            &[Part { name: "f", file_name: Some(name), content_type: None, data: b"x" }],
        );
        let (status, text) = upload(&runtime(), Some("multipart/form-data; boundary=XB"), body).await;
        assert_eq!(status, 200, "{name}: {text}");
        assert!(text.starts_with("f|"), "{name}: {text}");
    }
}

#[tokio::test]
async fn empty_field_and_no_parts() {
    let body = multipart_body(
        "XB",
        &[Part { name: "empty", file_name: None, content_type: None, data: b"" }],
    );
    let (status, text) = upload(&runtime(), Some("multipart/form-data; boundary=XB"), body).await;
    assert_eq!((status, text.as_str()), (200, "empty|-|-|0"));

    let (status, text) = upload(
        &runtime(),
        Some("multipart/form-data; boundary=XB"),
        Bytes::from_static(b"--XB--\r\n"),
    )
    .await;
    assert_eq!((status, text.as_str()), (200, ""));
}

#[tokio::test]
async fn route_limit_accepts_exactly_the_limit_and_rejects_one_more() {
    let mut app = App::new();
    app.post("/upload", summarize).body_limit(4 * 1024 * 1024);
    app.post("/other", summarize);
    let runtime = app.build().unwrap();

    let make = |len: usize| {
        let overhead = multipart_body(
            "XB",
            &[Part { name: "f", file_name: Some("fw.bin"), content_type: None, data: b"" }],
        )
        .len();
        let data = vec![7u8; len - overhead];
        multipart_body(
            "XB",
            &[Part { name: "f", file_name: Some("fw.bin"), content_type: None, data: &data }],
        )
    };
    let ct = [("content-type", "multipart/form-data; boundary=XB")];
    let limit = 4 * 1024 * 1024;

    let at = make(limit);
    assert_eq!(at.len(), limit);
    let ok = runtime.oneshot(Method::POST, "/upload", &ct, Some(at.clone())).await;
    assert_eq!(ok.status(), 200);
    let over = make(limit + 1);
    let rejected = runtime.oneshot(Method::POST, "/upload", &ct, Some(over)).await;
    assert_eq!(rejected.status(), 413);
    // The default 1 MiB limit still applies to the other route.
    let other = runtime.oneshot(Method::POST, "/other", &ct, Some(at)).await;
    assert_eq!(other.status(), 413);
}

#[tokio::test]
async fn upload_over_a_real_tcp_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    let mut app = App::new();
    app.post("/upload", summarize).body_limit(4 * 1024 * 1024);
    let runtime = app.build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });

    let data = vec![42u8; 3 * 1024 * 1024];
    let body = multipart_body(
        "XB",
        &[Part { name: "firmware", file_name: Some("fw.bin"), content_type: Some("application/octet-stream"), data: &data }],
    );
    let head = format!(
        "POST /upload HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Type: multipart/form-data; boundary=XB\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{}", &out[..out.len().min(300)]);
    assert!(out.ends_with(&format!("firmware|fw.bin|application/octet-stream|{}", data.len())), "{}", &out[out.len().saturating_sub(200)..]);
    let _ = stop.send(());
}
```

- [ ] **Step 2: Run, and mutation-check the guards**

Run: `cargo test --features "uuid test-util swagger multipart" --test multipart`
Expected: all pass. Several of these pass immediately on the Task 2 implementation (they pin behavior); for each of `part_content_may_contain_the_boundary_text_and_crlf` and `truncated_body_is_400_not_a_hang`, temporarily break the code that makes it pass (for the first, feed a modified boundary to `multer::Multipart::new`; for the second, make `map_error` return `ApiError::missing`) and watch the test FAIL, then restore.

- [ ] **Step 3: Commit**

```bash
git add tests/multipart.rs
git commit -m "test: multipart edge cases and a real TCP upload"
```

---

### Task 4: OpenAPI request body

**Files:**
- Test: `tests/multipart.rs`; no production change expected (Task 2 added `openapi_request`).

- [ ] **Step 1: Write the test**

```rust
#[test]
fn multipart_routes_document_a_multipart_request_body() {
    let mut app = App::new();
    app.openapi().title("t").version("1");
    app.post("/firmwares", summarize);
    let doc = app.openapi_document();
    let body = &doc["paths"]["/firmwares"]["post"]["requestBody"];
    assert_eq!(body["required"], true);
    assert_eq!(
        body["content"]["multipart/form-data"]["schema"]["additionalProperties"]["format"],
        "binary"
    );
}
```

- [ ] **Step 2: Run, mutation-check, commit**

Run: `cargo test --features "uuid test-util swagger multipart" --test multipart documents`
Expected: PASS. Temporarily change `"multipart/form-data"` in `openapi_request` to `"application/json"`, watch it FAIL, restore.

```bash
git add tests/multipart.rs
git commit -m "test: multipart OpenAPI request body"
```

---

### Task 5: Benchmarks and the performance gate

**Files:**
- Modify: `benches/router.rs` (a case compiled only with the feature), `docs/multipart-design.md` (results)

- [ ] **Step 1: Add the upload case**

At the end of `main` in `benches/router.rs`:

```rust
#[cfg(feature = "multipart")]
{
    use oas_rs::Multipart;

    async fn upload(mut form: Multipart) -> Result<&'static str, oas_rs::ApiError> {
        while let Some(field) = form.next_field().await? {
            let _ = field.bytes().await?;
        }
        Ok("OK")
    }

    let mut app = App::new();
    app.post("/upload", upload).body_limit(5 * 1024 * 1024);
    let runtime = app.build().unwrap();

    let boundary = "BENCHBOUNDARY";
    let file = vec![0xA5u8; 4 * 1024 * 1024 - 1024];
    let mut body = Vec::new();
    for (name, data) in [("note", &b"firmware 1.2.3"[..]), ("firmware", &file[..])] {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{name}.bin\"\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let body = Bytes::from(body);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let headers = [("content-type", content_type.as_str())];

    let iterations = 200u64;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..iterations {
        let response = runtime
            .oneshot(Method::POST, "/upload", &headers, Some(body.clone()))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    let elapsed = start.elapsed();
    println!(
        "case=multipart_upload body_bytes={} iterations={iterations} ms_per_op={:.3} mib_per_s={:.0} allocations_per_op={:.1} bytes_per_op={:.0}",
        body.len(),
        elapsed.as_secs_f64() * 1000.0 / iterations as f64,
        body.len() as f64 * iterations as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64(),
        ALLOCATIONS.load(Ordering::Relaxed) as f64 / iterations as f64,
        ALLOCATED_BYTES.load(Ordering::Relaxed) as f64 / iterations as f64,
    );
}
```

- [ ] **Step 2: Run both gates in the pinned Docker image**

Run `cargo bench --bench router --features uuid,test-util,swagger` (feature off) three times, then `--features uuid,test-util,swagger,multipart` three times.
Gate: feature off: `plaintext` and `static_route_count` within noise of ~250 ns, exactly 3.0000 allocations/op; feature on: the same numbers (the feature adds no per-request work to other routes). Report the `multipart_upload` line (ms/op, MiB/s, allocations/op, bytes/op).

- [ ] **Step 3: Record and commit**

Append a "Results" section with the numbers to `docs/multipart-design.md`.

```bash
git add benches/router.rs docs/multipart-design.md
git commit -m "bench: multipart upload and default-path gate"
```

If the default path regresses beyond noise or allocations change, stop and fix before continuing.

---

### Task 6: Documentation and final verification

**Files:**
- Modify: `README.md` (new "Multipart uploads" section), `CHANGELOG.md` (Unreleased: Added), `CLAUDE.md` (feature note), `docs/multipart-design.md` (status: implemented)

- [ ] **Step 1: README section**

Document the `multipart` feature, `Multipart` / `Field`, the `body_limit` per-route limit (and that `max_body_size` is global), the status codes, the "buffered, not streaming" limitation (use a raw handler for very large uploads), and an example `POST /firmwares` handler with `.body_limit(4 * 1024 * 1024)` and `.security(["BearerAuth"])`.

- [ ] **Step 2: CHANGELOG and CLAUDE.md**

CHANGELOG: `App::body_limit`, `Multipart`/`Field` behind feature `multipart`. CLAUDE.md: add `multipart` to the feature list and the verification commands.

- [ ] **Step 3: Final verification**

Run: `bash scripts/verify-docker.sh` and `cargo deny check` (inside Docker, as the CI job does).
Expected: every gate passes with default features and `--all-features`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs: document multipart uploads and body_limit"
```
