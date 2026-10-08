# Middleware Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a global `Middleware` trait with `Trace` and `BearerAuth` built-ins to oas-rs, with zero cost when unused and a measured per-layer cost.

**Architecture:** Runtime state moves into an inner `Arc<RuntimeInner<S>>` so a chain can own a handle to it (`Next` is `'static`). With no layers, the existing request path runs untouched; with layers, each layer returns a boxed future and the chain ends in the existing dispatch (`prepare_direct` for real connections, `handle` for in-memory requests).

**Tech Stack:** Rust 1.88, Hyper 1, Tokio, http-body, existing `BoxFuture`.

**Spec:** `docs/middleware-design.md`

## Global Constraints

- MSRV 1.88, edition 2024; no new dependencies.
- With zero registered layers the benchmark `plaintext`/`static_route_count` must stay within noise of ~250 ns/op and exactly 3 allocations/op.
- Every `unsafe` block needs a `// SAFETY:` comment (this plan adds none).
- Verification command for every task: `bash scripts/verify-docker.sh`; local quick loop: `cargo test --features "uuid test-util swagger" <filter>`.
- Public API additions: `Middleware`, `Next`, `RequestBody`, `App::layer`, `Trace`, `TraceRecord`, `BearerAuth`. `python` (not `python3`) on the dev machine.

## Review Focus

1. A layer answering `401`/early return on a `POST` with a body, without reading it, must not hang or corrupt the keep-alive connection (test over real TCP).
2. `HEAD`, `404`, `405` and automatic `OPTIONS` requests must pass through layers and keep their semantics.
3. `BearerAuth` with a lowercase scheme (`bearer x`), extra spaces, an empty token, `Bearer` with no token, and a non-UTF-8 header value: all `401`, never a panic.
4. `exempt_paths` must match `/health` and `/health/` (trailing slash) and must not exempt `/healthz`.
5. Zero layers must behave exactly as before (existing 27 acceptance tests pass unchanged).

---

### Task 1: Move runtime state into `RuntimeInner` (pure refactor)

**Files:**
- Modify: `src/runtime.rs` (struct `AppRuntime`, `ConnectionRuntime`, `serve_runtime`, `runtime_ref`)
- Modify: `src/app.rs` (`build()`)

**Interfaces:**
- Produces: `pub(crate) struct RuntimeInner<S> { state, plans, capture_names, static_routes, dynamic_routes }` with `fn runtime_ref(&self) -> RuntimeRef<'_, S>`; `AppRuntime<S> { pub(crate) inner: Arc<RuntimeInner<S>>, pub(crate) shutdown_timeout: Duration }`.

- [ ] **Step 1: Confirm the baseline is green**

Run: `cargo test --features "uuid test-util swagger"`
Expected: all pass.

- [ ] **Step 2: Introduce `RuntimeInner`**

In `src/runtime.rs` replace the `AppRuntime` struct with:

```rust
/// An immutable application runtime produced by [`App::build`].
pub struct AppRuntime<S = ()> {
    pub(crate) inner: Arc<RuntimeInner<S>>,
    pub(crate) shutdown_timeout: Duration,
}

pub(crate) struct RuntimeInner<S> {
    pub(crate) state: Arc<S>,
    pub(crate) plans: Box<[RoutePlan<S>]>,
    pub(crate) capture_names: Box<[Option<Arc<[String]>>]>,
    pub(crate) static_routes: HashMap<String, RouteSet>,
    pub(crate) dynamic_routes: DynamicRouteTrie,
}

impl<S: Send + Sync + 'static> RuntimeInner<S> {
    pub(crate) fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        RuntimeRef {
            state: &self.state,
            plans: &self.plans,
            capture_names: &self.capture_names,
            static_routes: &self.static_routes,
            dynamic_routes: &self.dynamic_routes,
        }
    }
}
```

Change `ConnectionRuntime<S>` to hold `runtime: Arc<RuntimeInner<S>>` (and `new(runtime: Arc<RuntimeInner<S>>)`), change `AppRuntime::runtime_ref(&self)` to `self.inner.runtime_ref()`, and in `serve_runtime` use `let shutdown_timeout = runtime.shutdown_timeout; let runtime = runtime.inner;` instead of `Arc::new(runtime)`. In `prepare_matched`, `Arc::clone(&self.runtime)` already clones the inner `Arc`.

- [ ] **Step 3: Update `build()` in `src/app.rs`**

```rust
Ok(AppRuntime {
    inner: Arc::new(RuntimeInner {
        state: self.state,
        plans: self.plans.into_boxed_slice(),
        capture_names: self
            .metadata
            .iter()
            .map(|metadata| metadata.capture_names.clone())
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        static_routes: self.static_routes,
        dynamic_routes: self.dynamic_routes,
    }),
    shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
})
```

- [ ] **Step 4: Run the whole suite and the benchmark**

Run: `cargo test --features "uuid test-util swagger"` then `bash scripts/verify-docker.sh`
Expected: all pass; `cargo bench --bench router --features uuid,test-util,swagger` still ~250 ns, 3 allocations.

- [ ] **Step 5: Commit**

```bash
git add src/runtime.rs src/app.rs
git commit -m "refactor: hold runtime state behind Arc<RuntimeInner>"
```

---

### Task 2: Core middleware types and the in-memory chain

**Files:**
- Create: `src/middleware.rs`
- Modify: `src/lib.rs` (add `mod middleware;`, re-exports), `src/app.rs` (`layer`, field, `build`), `src/runtime.rs` (`RuntimeInner.middleware`, `Host` impl, `oneshot`), `Cargo.toml` (`[[test]] name = "middleware"` with `required-features = ["test-util"]`)
- Test: `tests/middleware.rs`

**Interfaces:**
- Consumes: Task 1's `RuntimeInner`.
- Produces:
  - `pub struct RequestBody` implementing `http_body::Body<Data = Bytes, Error = hyper::Error>`.
  - `pub trait Middleware: Send + Sync + 'static { fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse>; }` plus a blanket impl for `Fn(Request<RequestBody>, Next) -> impl Future<Output = HttpResponse> + Send + 'static`.
  - `pub struct Next` with `pub fn run(self, request: Request<RequestBody>) -> BoxFuture<HttpResponse>` (`BoxFuture<T>` is the existing `Pin<Box<dyn Future<Output = T> + Send + 'static>>`).
  - `App::layer(&mut self, middleware: impl Middleware) -> &mut Self`.
  - `pub(crate) trait Host` implemented by `RuntimeInner<S>`.

- [ ] **Step 1: Write the failing tests**

Create `tests/middleware.rs`:

```rust
use std::sync::{Arc, Mutex};

use http::{HeaderValue, Request};
use oas_rs::{ApiError, App, Header, HeaderSpec, Method, Next, RequestBody};

async fn hello() -> &'static str {
    "hello"
}

struct User(String);

impl HeaderSpec for User {
    const NAME: &'static str = "x-user";
    fn parse(value: &str) -> Result<Self, ApiError> {
        Ok(User(value.to_owned()))
    }
}

async fn whoami(Header(user): Header<User>) -> String {
    user.0
}

fn recorder(log: &Arc<Mutex<Vec<String>>>, name: &'static str) -> impl oas_rs::Middleware {
    let log = Arc::clone(log);
    move |request: Request<RequestBody>, next: Next| {
        let log = Arc::clone(&log);
        async move {
            log.lock().unwrap().push(format!("{name}:in"));
            let response = next.run(request).await;
            log.lock().unwrap().push(format!("{name}:out"));
            response
        }
    }
}

#[tokio::test]
async fn layers_wrap_in_registration_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new();
    app.get("/", hello);
    app.layer(recorder(&log, "a"));
    app.layer(recorder(&log, "b"));
    let runtime = app.build().unwrap();

    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 200);
    assert_eq!(*log.lock().unwrap(), ["a:in", "b:in", "b:out", "a:out"]);
}

#[tokio::test]
async fn a_layer_can_answer_without_calling_next() {
    let mut app = App::new();
    app.get("/", hello);
    app.layer(|_request: Request<RequestBody>, _next: Next| async {
        ApiError::new(http::StatusCode::FORBIDDEN, "Forbidden", "nope").into_response()
    });
    let runtime = app.build().unwrap();

    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn a_layer_can_modify_the_request_and_the_response() {
    let mut app = App::new();
    app.get("/me", whoami);
    app.layer(|mut request: Request<RequestBody>, next: Next| async move {
        request
            .headers_mut()
            .insert("x-user", HeaderValue::from_static("alice"));
        let mut response = next.run(request).await;
        response
            .headers_mut()
            .insert("x-layer", HeaderValue::from_static("seen"));
        response
    });
    let runtime = app.build().unwrap();

    let response = runtime.oneshot(Method::GET, "/me", &[], None).await;
    assert_eq!(response.header("x-layer"), Some("seen"));
    assert_eq!(response.body_string().await, "alice");
}

#[tokio::test]
async fn layers_see_404_405_options_and_head() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new();
    app.get("/", hello);
    let counter = Arc::clone(&seen);
    app.layer(move |request: Request<RequestBody>, next: Next| {
        let counter = Arc::clone(&counter);
        async move {
            counter.lock().unwrap().push(request.method().to_string());
            next.run(request).await
        }
    });
    let runtime = app.build().unwrap();

    assert_eq!(runtime.oneshot(Method::GET, "/missing", &[], None).await.status(), 404);
    assert_eq!(runtime.oneshot(Method::POST, "/", &[], None).await.status(), 405);
    assert_eq!(runtime.oneshot(Method::OPTIONS, "/", &[], None).await.status(), 204);
    let head = runtime.oneshot(Method::HEAD, "/", &[], None).await;
    assert_eq!(head.status(), 200);
    assert_eq!(head.body_string().await, "");
    assert_eq!(*seen.lock().unwrap(), ["GET", "POST", "OPTIONS", "HEAD"]);
}

#[tokio::test]
async fn no_layers_behaves_as_before() {
    let mut app = App::new();
    app.get("/", hello);
    let runtime = app.build().unwrap();
    let response = runtime.oneshot(Method::GET, "/", &[], None).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body_string().await, "hello");
}
```

- [ ] **Step 2: Add the test target and confirm RED**

Add to `Cargo.toml`:

```toml
[[test]]
name = "middleware"
required-features = ["test-util"]
```

Run: `cargo test --features "uuid test-util swagger" --test middleware`
Expected: FAIL to compile (`no method named layer`, unresolved `Next`/`RequestBody`).

- [ ] **Step 3: Implement `src/middleware.rs`**

```rust
use crate::*;
use http_body::{Body, Frame, SizeHint};

/// The request body seen by middleware: Hyper's streaming body for real
/// connections, or an in-memory buffer for [`AppRuntime::oneshot`]. It can be
/// read, but only the framework constructs it, so the same body always reaches
/// the handler at the end of the chain.
pub struct RequestBody(RequestBodyKind);

enum RequestBodyKind {
    Incoming(Incoming),
    Full(Option<Bytes>),
}

impl RequestBody {
    pub(crate) fn incoming(body: Incoming) -> Self {
        Self(RequestBodyKind::Incoming(body))
    }

    pub(crate) fn full(body: Bytes) -> Self {
        Self(RequestBodyKind::Full(Some(body)))
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        match &mut self.get_mut().0 {
            RequestBodyKind::Incoming(body) => Pin::new(body).poll_frame(context),
            RequestBodyKind::Full(bytes) => {
                Poll::Ready(bytes.take().filter(|b| !b.is_empty()).map(|b| Ok(Frame::data(b))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.0 {
            RequestBodyKind::Incoming(body) => body.is_end_stream(),
            RequestBodyKind::Full(bytes) => bytes.as_ref().is_none_or(|b| b.is_empty()),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.0 {
            RequestBodyKind::Incoming(body) => body.size_hint(),
            RequestBodyKind::Full(bytes) => {
                SizeHint::with_exact(bytes.as_ref().map_or(0, |b| b.len() as u64))
            }
        }
    }
}

/// Logic that wraps every request. Layers run before routing, in the order
/// they were registered with [`App::layer`] (the first one is outermost).
///
/// The returned future is `'static`: a hand-written implementation clones what
/// it needs (typically an `Arc`) into the future instead of borrowing `&self`.
/// Plain `async fn`s and closures with the right signature implement this
/// trait automatically.
pub trait Middleware: Send + Sync + 'static {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse>;
}

impl<F, Fut> Middleware for F
where
    F: Fn(Request<RequestBody>, Next) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = HttpResponse> + Send + 'static,
{
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        Box::pin((self)(request, next))
    }
}

/// The rest of the chain: the remaining layers, then normal dispatch.
pub struct Next {
    host: Arc<dyn Host>,
    index: usize,
}

impl Next {
    pub(crate) fn new(host: Arc<dyn Host>) -> Self {
        Self { host, index: 0 }
    }

    /// Runs the remaining layers and then the handler, returning the response.
    pub fn run(self, request: Request<RequestBody>) -> BoxFuture<HttpResponse> {
        match self.host.middleware().get(self.index).cloned() {
            Some(layer) => layer.handle(
                request,
                Next {
                    host: self.host,
                    index: self.index + 1,
                },
            ),
            None => self.host.dispatch(request),
        }
    }
}

/// What the chain needs from the runtime: the layer list and the terminal
/// dispatch. Implemented by `RuntimeInner`.
pub(crate) trait Host: Send + Sync + 'static {
    fn middleware(&self) -> &[Arc<dyn Middleware>];
    fn dispatch(self: Arc<Self>, request: Request<RequestBody>) -> BoxFuture<HttpResponse>;
}

impl<S: Send + Sync + 'static> Host for RuntimeInner<S> {
    fn middleware(&self) -> &[Arc<dyn Middleware>] {
        &self.middleware
    }

    fn dispatch(self: Arc<Self>, request: Request<RequestBody>) -> BoxFuture<HttpResponse> {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            match body.0 {
                RequestBodyKind::Incoming(incoming) => {
                    let request = Request::from_parts(parts, incoming);
                    ConnectionRuntime::new(self).prepare_direct(request).await
                }
                RequestBodyKind::Full(bytes) => {
                    let request = Request::from_parts(parts, bytes.unwrap_or_default());
                    self.runtime_ref().handle(request).await
                }
            }
        })
    }
}
```

`RuntimeRef::handle` is currently `#[cfg(any(test, feature = "test-util"))]`; remove that cfg (the in-memory path is now also the chain terminal) and mark the `Full` arm and `RequestBody::full` `#[allow(dead_code)]`-free by keeping them unconditional.

- [ ] **Step 4: Wire it into the crate**

`src/lib.rs`: add `mod middleware;`, `pub use middleware::{Middleware, Next, RequestBody};` and `use middleware::Host;`.

`src/runtime.rs`: add `pub(crate) middleware: Box<[Arc<dyn Middleware>]>` to `RuntimeInner`; rename the existing `ConnectionRuntime::prepare` to `prepare_direct` and add:

```rust
fn prepare(&self, request: Request<Incoming>) -> PreparedDispatch {
    if self.runtime.middleware.is_empty() {
        return self.prepare_direct(request);
    }
    let (parts, body) = request.into_parts();
    let request = Request::from_parts(parts, RequestBody::incoming(body));
    let host: Arc<dyn Host> = self.runtime.clone();
    PreparedDispatch::Buffered(Next::new(host).run(request))
}
```

In `AppRuntime::oneshot`, build the request as before, then:

```rust
let response = if self.inner.middleware.is_empty() {
    self.runtime_ref().handle(request).await
} else {
    let (parts, body) = request.into_parts();
    let request = Request::from_parts(parts, RequestBody::full(body));
    let host: Arc<dyn Host> = self.inner.clone();
    Next::new(host).run(request).await
};
TestResponse { response: Some(response) }
```

`src/app.rs`: add `pub(crate) middleware: Vec<Arc<dyn Middleware>>` to `App` (init `Vec::new()` in `App::new`; `with_state` must carry it over), and

```rust
/// Registers a global layer. Layers run before routing; the first one
/// registered is the outermost.
pub fn layer(&mut self, middleware: impl Middleware) -> &mut Self {
    self.middleware.push(Arc::new(middleware));
    self
}
```

and `middleware: self.middleware.into_boxed_slice()` in `build()`.

- [ ] **Step 5: Run the tests to verify GREEN**

Run: `cargo test --features "uuid test-util swagger" --test middleware`
Expected: 5 passed.

- [ ] **Step 6: Full suite, benchmark gate, commit**

Run: `bash scripts/verify-docker.sh`; `cargo bench --bench router --features uuid,test-util,swagger` (zero layers: ~250 ns, 3 allocations).

```bash
git add -A
git commit -m "feat: add global Middleware trait, Next and App::layer"
```

---

### Task 3: Run the chain on real connections

**Files:**
- Modify: nothing in `src/` beyond Task 2 (the server path already calls `prepare`); add tests.
- Test: `tests/middleware.rs`

**Interfaces:** Consumes Task 2's `ConnectionRuntime::prepare` branching.

- [ ] **Step 1: Write the failing/guard tests over real TCP**

Append to `tests/middleware.rs`:

```rust
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

async fn serve(app: App) -> (std::net::SocketAddr, oneshot::Sender<()>) {
    let runtime = app.build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    (addr, tx)
}

async fn raw(addr: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn chain_runs_on_a_real_connection() {
    let mut app = App::new();
    app.get("/", hello);
    app.layer(|request: Request<RequestBody>, next: Next| async move {
        let mut response = next.run(request).await;
        response
            .headers_mut()
            .insert("x-layer", HeaderValue::from_static("tcp"));
        response
    });
    let (addr, _stop) = serve(app).await;
    let out = raw(addr, "GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").await;
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.to_ascii_lowercase().contains("x-layer: tcp"), "{out}");
    assert!(out.ends_with("hello"), "{out}");
}

#[tokio::test]
async fn early_return_on_a_post_with_a_body_does_not_hang_the_connection() {
    let mut app = App::new();
    app.post("/", hello);
    app.layer(|_request: Request<RequestBody>, _next: Next| async {
        ApiError::new(http::StatusCode::UNAUTHORIZED, "Unauthorized", "no").into_response()
    });
    let (addr, _stop) = serve(app).await;
    let body = "x".repeat(10_000);
    let request = format!(
        "POST / HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let out = tokio::time::timeout(Duration::from_secs(3), raw(addr, &request))
        .await
        .expect("connection hung after an early return");
    assert!(out.starts_with("HTTP/1.1 401"), "{out}");
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --features "uuid test-util swagger" --test middleware`
Expected: the new tests PASS (the path was wired in Task 2). If either fails, fix `prepare`/`dispatch` before continuing; a failure here is a real bug, not a test problem.

- [ ] **Step 3: Commit**

```bash
git add tests/middleware.rs
git commit -m "test: middleware chain over real TCP connections"
```

---

### Task 4: `Trace` built-in

**Files:**
- Create: `src/trace.rs`; add `mod trace;` and `pub use trace::{Trace, TraceRecord};` in `src/lib.rs`
- Test: `tests/middleware.rs`

**Interfaces:**
- Consumes: `Middleware`, `Next`, `RequestBody`, `BoxFuture`.
- Produces: `pub struct TraceRecord { pub method: Method, pub path: String, pub status: StatusCode, pub elapsed: Duration }`; `pub struct Trace`; `Trace::new(impl Fn(&TraceRecord) + Send + Sync + 'static) -> Self`; `Trace::stderr() -> Self`; `impl Middleware for Trace`.

- [ ] **Step 1: Write the failing test**

```rust
use oas_rs::{Trace, TraceRecord};

#[tokio::test]
async fn trace_reports_method_path_status_and_latency() {
    let records = Arc::new(Mutex::new(Vec::<TraceRecord>::new()));
    let sink = Arc::clone(&records);
    let mut app = App::new();
    app.get("/hello", hello);
    app.layer(Trace::new(move |record| sink.lock().unwrap().push(record.clone())));
    let runtime = app.build().unwrap();

    runtime.oneshot(Method::GET, "/hello", &[], None).await;
    runtime.oneshot(Method::GET, "/nope", &[], None).await;

    let records = records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!((records[0].method.as_str(), records[0].path.as_str()), ("GET", "/hello"));
    assert_eq!(records[0].status, 200);
    assert_eq!(records[1].status, 404);
    assert!(records[0].elapsed < Duration::from_secs(1));
}
```

- [ ] **Step 2: Confirm RED**

Run: `cargo test --features "uuid test-util swagger" --test middleware trace`
Expected: FAIL to compile (`unresolved import oas_rs::Trace`).

- [ ] **Step 3: Implement `src/trace.rs`**

```rust
use std::time::Instant;

use crate::*;

/// One finished request, handed to the [`Trace`] callback.
#[derive(Clone, Debug)]
pub struct TraceRecord {
    pub method: Method,
    pub path: String,
    pub status: StatusCode,
    pub elapsed: Duration,
}

/// Middleware that reports every request after its response is produced.
#[derive(Clone)]
pub struct Trace {
    sink: Arc<dyn Fn(&TraceRecord) + Send + Sync>,
}

impl Trace {
    pub fn new(sink: impl Fn(&TraceRecord) + Send + Sync + 'static) -> Self {
        Self { sink: Arc::new(sink) }
    }

    /// Prints `METHOD /path STATUS elapsed` lines to standard error.
    pub fn stderr() -> Self {
        Self::new(|record| {
            eprintln!(
                "{} {} {} {:?}",
                record.method,
                record.path,
                record.status.as_u16(),
                record.elapsed
            );
        })
    }
}

impl Middleware for Trace {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let sink = Arc::clone(&self.sink);
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        Box::pin(async move {
            let started = Instant::now();
            let response = next.run(request).await;
            sink(&TraceRecord {
                method,
                path,
                status: response.status(),
                elapsed: started.elapsed(),
            });
            response
        })
    }
}
```

- [ ] **Step 4: Verify GREEN and commit**

Run: `cargo test --features "uuid test-util swagger" --test middleware`
Expected: all pass.

```bash
git add src/trace.rs src/lib.rs tests/middleware.rs
git commit -m "feat: add Trace middleware"
```

---

### Task 5: `BearerAuth` built-in

**Files:**
- Create: `src/bearer.rs`; add `mod bearer;` and `pub use bearer::BearerAuth;` in `src/lib.rs`
- Test: `tests/middleware.rs`

**Interfaces:**
- Consumes: `Middleware`, `Next`, `RequestBody`, `ApiError`, `IntoResponse`, `BoxFuture`, `normalize_request_path`.
- Produces: `BearerAuth::new<F, Fut>(validator: F) -> Self` where `F: Fn(String) -> Fut + Send + Sync + 'static`, `Fut: Future<Output = Result<(), ApiError>> + Send + 'static`; `BearerAuth::exempt_paths<I, T>(self, paths: I) -> Self` where `I: IntoIterator<Item = T>, T: Into<String>`; `impl Middleware for BearerAuth`.

- [ ] **Step 1: Write the failing tests**

```rust
use oas_rs::BearerAuth;

fn secured() -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.get("/secret", hello);
    app.get("/health", hello);
    app.get("/healthz", hello);
    app.layer(
        BearerAuth::new(|token: String| async move {
            if token == "good" {
                Ok(())
            } else {
                Err(ApiError::new(http::StatusCode::FORBIDDEN, "Forbidden", "bad token"))
            }
        })
        .exempt_paths(["/health"]),
    );
    app.build().unwrap()
}

#[tokio::test]
async fn bearer_accepts_a_valid_token() {
    let runtime = secured();
    let response = runtime
        .oneshot(Method::GET, "/secret", &[("authorization", "Bearer good")], None)
        .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn bearer_rejects_missing_or_malformed_headers_with_401() {
    let runtime = secured();
    for header in [None, Some("Bearer"), Some("Bearer "), Some("Bearer   "), Some("Basic abc"), Some("good")] {
        let headers: Vec<(&str, &str)> = header.map(|v| ("authorization", v)).into_iter().collect();
        let response = runtime.oneshot(Method::GET, "/secret", &headers, None).await;
        assert_eq!(response.status(), 401, "{header:?}");
        assert_eq!(response.header("www-authenticate"), Some("Bearer"), "{header:?}");
    }
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive_and_tolerates_extra_spaces() {
    let runtime = secured();
    for value in ["bearer good", "BEARER good", "Bearer  good"] {
        let response = runtime
            .oneshot(Method::GET, "/secret", &[("authorization", value)], None)
            .await;
        assert_eq!(response.status(), 200, "{value}");
    }
}

#[tokio::test]
async fn validator_errors_are_returned_as_given() {
    let runtime = secured();
    let response = runtime
        .oneshot(Method::GET, "/secret", &[("authorization", "Bearer evil")], None)
        .await;
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn exempt_paths_skip_authentication_exactly() {
    let runtime = secured();
    assert_eq!(runtime.oneshot(Method::GET, "/health", &[], None).await.status(), 200);
    assert_eq!(runtime.oneshot(Method::GET, "/health/", &[], None).await.status(), 200);
    assert_eq!(runtime.oneshot(Method::GET, "/healthz", &[], None).await.status(), 401);
}
```

- [ ] **Step 2: Confirm RED**

Run: `cargo test --features "uuid test-util swagger" --test middleware bearer`
Expected: FAIL to compile (`unresolved import oas_rs::BearerAuth`).

- [ ] **Step 3: Implement `src/bearer.rs`**

```rust
use crate::*;

type Validator = dyn Fn(String) -> BoxFuture<Result<(), ApiError>> + Send + Sync;

/// Middleware that requires `Authorization: Bearer <token>` and asks a
/// user-supplied async validator whether the token is acceptable.
///
/// It enforces authentication; declaring the scheme in the OpenAPI document is
/// separate (see `OpenApiOptions::bearer_auth`). Paths listed in
/// [`exempt_paths`](Self::exempt_paths) skip the check.
#[derive(Clone)]
pub struct BearerAuth {
    validator: Arc<Validator>,
    exempt: Arc<[String]>,
}

impl BearerAuth {
    pub fn new<F, Fut>(validator: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        Self {
            validator: Arc::new(move |token| Box::pin(validator(token))),
            exempt: Arc::from(Vec::new()),
        }
    }

    /// Exact paths (a trailing slash is ignored) that do not require a token.
    pub fn exempt_paths<I, T>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.exempt = paths
            .into_iter()
            .map(|path| normalize_path(&path.into()))
            .collect();
        self
    }
}

fn unauthorized() -> HttpResponse {
    let mut response = ApiError::new(
        StatusCode::UNAUTHORIZED,
        "Unauthorized",
        "a valid bearer token is required",
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn bearer_token(value: &HeaderValue) -> Option<String> {
    let value = value.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_owned())
}

impl Middleware for BearerAuth {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let path = normalize_request_path(request.uri().path());
        if self.exempt.iter().any(|exempt| exempt == path) {
            return next.run(request);
        }
        let Some(token) = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(bearer_token)
        else {
            return Box::pin(async { unauthorized() });
        };
        let validator = Arc::clone(&self.validator);
        Box::pin(async move {
            match validator(token).await {
                Ok(()) => next.run(request).await,
                Err(error) => error.into_response(),
            }
        })
    }
}
```

`normalize_path` (stored form: trailing slashes trimmed, `"/health"` stays `"/health"`) and `normalize_request_path` (`"/health/"` becomes `"/health"`) are the existing helpers in `src/path.rs`, so comparing them gives the exact-match-ignoring-trailing-slash behavior pinned by `exempt_paths_skip_authentication_exactly`.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cargo test --features "uuid test-util swagger" --test middleware`
Expected: all pass (non-UTF-8 header values yield 401 through `to_str().ok()?`).

```bash
git add src/bearer.rs src/lib.rs tests/middleware.rs
git commit -m "feat: add BearerAuth middleware"
```

---

### Task 6: Benchmarks and the performance gate

**Files:**
- Modify: `benches/router.rs` (new cases), `docs/middleware-design.md` (results table at the end)

**Interfaces:** Consumes `App::layer`, `Next`, `RequestBody`, `AppRuntime::oneshot`, existing `measure_app`.

- [ ] **Step 1: Add the cases**

In `benches/router.rs`, inside `main` after the existing plaintext case, add (reusing `measure_app`'s output shape `(elapsed_ns, allocations, bytes)`):

```rust
async fn noop(request: http::Request<oas_rs::RequestBody>, next: oas_rs::Next) -> http::Response<oas_rs::ResponseBody> {
    next.run(request).await
}

for layers in [0usize, 1, 3, 5] {
    let mut app = App::new();
    app.get("/plaintext", plaintext);
    for _ in 0..layers {
        app.layer(noop);
    }
    let runtime = app.build().unwrap();
    let iterations = 100_000;
    let (elapsed, allocations, bytes) =
        measure_app(&runtime, Method::GET, "/plaintext", &[], iterations).await;
    println!(
        "case=middleware layers={layers} iterations={iterations} ns_per_op={:.2} allocations_per_op={:.4} bytes_per_op={:.2}",
        elapsed as f64 / iterations as f64,
        allocations as f64 / iterations as f64,
        bytes as f64 / iterations as f64,
    );
}
```

- [ ] **Step 2: Run the gate and record results**

Run: `cargo bench --bench router --features uuid,test-util,swagger` in the pinned Docker image (`bash scripts/verify-docker.sh` runs the tests; run the bench via `docker run` as in earlier runs).
Gate: `layers=0` within noise of the pre-change baseline (about 250 ns) and exactly 3.0000 allocations/op. Report ns/op and allocations/op per layer count, and the per-layer delta.

- [ ] **Step 3: TCP loopback comparison**

Repeat the keep-alive and short-connection loopback benchmark (16 connections, 3 s, 3 alternating runs) on `main` versus this branch, with and without one no-op layer. Report mean req/s.

- [ ] **Step 4: Record results and commit**

Append a "Results" table to `docs/middleware-design.md` with the measured numbers.

```bash
git add benches/router.rs docs/middleware-design.md
git commit -m "bench: measure middleware overhead per layer"
```

If `layers=0` regresses beyond noise or allocations change, stop and fix before continuing; the work is not finished.

---

### Task 7: Documentation and final verification

**Files:**
- Modify: `README.md` (new "Middleware" section), `CHANGELOG.md` (Unreleased: Added), `CLAUDE.md` (architecture note), `docs/middleware-design.md` (status: implemented)

- [ ] **Step 1: README section**

Document `App::layer`, an `async fn` example, `Trace::stderr()`, `BearerAuth` with `exempt_paths(["/health", "/openapi.json"])`, the "runs before routing" and "cannot replace the body" limits, and that `BearerAuth` enforces while `bearer_auth` only documents.

- [ ] **Step 2: CHANGELOG and CLAUDE.md**

CHANGELOG: list `Middleware`, `Next`, `RequestBody`, `App::layer`, `Trace`, `BearerAuth`, and note the 0.2.0 target. CLAUDE.md: one paragraph on `RuntimeInner`, the `Host` trait, and the `prepare` / `prepare_direct` split.

- [ ] **Step 3: Final verification**

Run: `bash scripts/verify-docker.sh`
Expected: every gate passes. Re-run the benchmark gate from Task 6 on the final tree.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs: document middleware"
```
