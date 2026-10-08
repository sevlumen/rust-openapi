# Scoped Middleware Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Layers scoped to a path prefix (`layer_for`, `Group`) or to a single route (`route_layer`), with identical behavior for global layers.

**Architecture:** Layers are stored as `ScopedLayer { scope, layer }`; `Next::run` skips layers whose scope does not match the request's method and path (segments split with the router's `PathParts`). `Group` prefixes paths and forwards registration and metadata calls to `App`.

**Tech Stack:** Rust 1.88; no new dependencies.

**Spec:** `docs/scoped-middleware-design.md`

## Global Constraints

- No new dependencies. `Middleware`, `Next`, `RequestBody` and existing `App::layer` behavior are unchanged.
- No layers: unchanged request path (`plaintext` ~250 ns, 3 allocations). One global layer: no regression versus the 0.4.0 numbers (about 403 ns with one no-op layer in the in-memory harness).
- A scope must never match fewer requests than the router routes (no auth bypass): matching uses `PathParts`.
- Verification: `bash scripts/verify-docker.sh`; quick loop: `cargo test --features "uuid test-util swagger multipart tls http2" --test scoped_middleware`. Use `python` (not `python3`). Run `bash scripts/clean.sh` when finished.

## Review Focus

1. `//admin/x`, `/admin//x` and `/admin/x/` must run an `/admin` scoped layer whenever the router would serve them (parity test against an app without the layer).
2. `/admin` scope must not match `/administrator`; it must match `/admin`, `/admin/` and a 404 under it.
3. A chained `g.get("/a", h).get("/b", h2)` must register both routes under the prefix.
4. `route_layer` on `GET /export` must not affect `POST /export`; a `HEAD /export` request must run it.
5. Global-only apps must behave exactly as before (all existing middleware tests pass unchanged).

---

### Task 1: Scopes, `ScopedLayer` and `App::layer_for`

**Files:**
- Modify: `src/middleware.rs` (`Scope`, `ScopedLayer`, `Next::run`, `Host::middleware`), `src/app.rs` (`layer_for`, layer storage), `src/runtime.rs` (`RuntimeInner.middleware` type), `src/lib.rs` (imports)
- Test: `tests/scoped_middleware.rs` (new; `[[test]] required-features = ["test-util"]`)

**Interfaces:**
- Produces: `App::layer_for(&mut self, prefix: &str, middleware: impl Middleware) -> &mut Self`; `pub(crate) enum Scope { All, Prefix(Arc<[ScopeSegment]>), Route { method: Method, segments: Arc<[ScopeSegment]> } }` with `Scope::parse(prefix: &str) -> Arc<[ScopeSegment]>` and `Scope::matches(&self, method: &Method, path: &str) -> bool`; `pub(crate) struct ScopedLayer { pub(crate) scope: Scope, pub(crate) layer: Arc<dyn Middleware> }`; `Host::middleware(&self) -> &[ScopedLayer]`.

- [ ] **Step 1: Write the failing tests**

Create `tests/scoped_middleware.rs`:

```rust
use std::sync::{Arc, Mutex};

use http::{HeaderValue, Request, StatusCode};
use oas_rs::{ApiError, App, IntoResponse, Method, Next, RequestBody};

async fn ok() -> &'static str {
    "ok"
}

/// A layer that rejects with 401 unless `x-token: yes` is present.
fn auth(request: Request<RequestBody>, next: Next) -> impl std::future::Future<Output = oas_rs::HttpResponse> + Send + 'static {
    async move {
        if request.headers().get("x-token").and_then(|v| v.to_str().ok()) == Some("yes") {
            next.run(request).await
        } else {
            ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized", "token").into_response()
        }
    }
}

async fn status(runtime: &oas_rs::AppRuntime, method: Method, uri: &str) -> u16 {
    runtime.oneshot(method, uri, &[], None).await.status().as_u16()
}

fn app_with_scoped_auth(prefix: &str) -> oas_rs::AppRuntime {
    let mut app = App::new();
    app.get("/admin", ok);
    app.get("/admin/users", ok);
    app.get("/administrator", ok);
    app.get("/public", ok);
    app.get("/tenants/{id}/data", ok);
    app.layer_for(prefix, auth);
    app.build().unwrap()
}

#[tokio::test]
async fn a_prefix_layer_covers_the_prefix_and_what_is_below_it() {
    let runtime = app_with_scoped_auth("/admin");
    assert_eq!(status(&runtime, Method::GET, "/admin").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/admin/").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/admin/users").await, 401);
    // not found under the prefix is still protected (no route-existence leak)
    assert_eq!(status(&runtime, Method::GET, "/admin/nope").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/admin/users").await, 401);
}

#[tokio::test]
async fn a_prefix_layer_does_not_cover_other_paths_or_longer_names() {
    let runtime = app_with_scoped_auth("/admin");
    assert_eq!(status(&runtime, Method::GET, "/administrator").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/public").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/missing").await, 404);
}

#[tokio::test]
async fn captures_in_a_prefix_match_any_single_segment() {
    let runtime = app_with_scoped_auth("/tenants/{id}");
    assert_eq!(status(&runtime, Method::GET, "/tenants/42/data").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/tenants/abc").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/tenants").await, 404); // outside the scope
    assert_eq!(status(&runtime, Method::GET, "/public").await, 200);
}

#[tokio::test]
async fn the_root_prefix_is_global() {
    let runtime = app_with_scoped_auth("/");
    assert_eq!(status(&runtime, Method::GET, "/public").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/missing").await, 401);
}

#[tokio::test]
async fn a_prefix_layer_runs_for_405_and_options_under_the_prefix() {
    let runtime = app_with_scoped_auth("/admin");
    assert_eq!(status(&runtime, Method::POST, "/admin/users").await, 401);
    assert_eq!(status(&runtime, Method::OPTIONS, "/admin/users").await, 401);
    assert_eq!(status(&runtime, Method::POST, "/public").await, 405);
}

/// Security parity: wherever the router serves a request, the scope must apply.
#[tokio::test]
async fn a_scope_is_never_narrower_than_routing() {
    let mut plain = App::new();
    plain.get("/admin/x", ok);
    plain.get("/admin/{id}", ok);
    let plain = plain.build().unwrap();

    let mut guarded = App::new();
    guarded.get("/admin/x", ok);
    guarded.get("/admin/{id}", ok);
    guarded.layer_for("/admin", auth);
    let guarded = guarded.build().unwrap();

    for uri in ["/admin/x", "/admin/x/", "//admin/x", "/admin//x", "//admin//x//", "/admin/y", "//admin/y", "/%61dmin/x", "/admin/%78"] {
        let served = status(&plain, Method::GET, uri).await;
        let protected = status(&guarded, Method::GET, uri).await;
        if served == 200 {
            assert_eq!(protected, 401, "{uri} is served by the router but not guarded");
        }
    }
}

#[tokio::test]
async fn global_and_scoped_layers_run_in_registration_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let make = |name: &'static str, log: &Arc<Mutex<Vec<String>>>| {
        let log = Arc::clone(log);
        move |request: Request<RequestBody>, next: Next| {
            let log = Arc::clone(&log);
            async move {
                log.lock().unwrap().push(name.to_owned());
                next.run(request).await
            }
        }
    };
    let mut app = App::new();
    app.get("/admin/users", ok);
    app.get("/other", ok);
    app.layer(make("global-1", &log));
    app.layer_for("/admin", make("admin", &log));
    app.layer(make("global-2", &log));
    let runtime = app.build().unwrap();

    status(&runtime, Method::GET, "/admin/users").await;
    assert_eq!(*log.lock().unwrap(), ["global-1", "admin", "global-2"]);
    log.lock().unwrap().clear();
    status(&runtime, Method::GET, "/other").await;
    assert_eq!(*log.lock().unwrap(), ["global-1", "global-2"]);
}

#[allow(dead_code)]
fn _unused(_: HeaderValue) {}
```

- [ ] **Step 2: Confirm RED**

Add to `Cargo.toml`:

```toml
[[test]]
name = "scoped_middleware"
required-features = ["test-util"]
```

Run: `cargo test --features "uuid test-util swagger multipart tls http2" --test scoped_middleware`
Expected: FAIL to compile (`no method named layer_for`).

- [ ] **Step 3: Implement**

In `src/middleware.rs` add (and use `PathParts` from `crate::*`):

```rust
/// One segment of a scope pattern: a literal, or `{name}` matching any one segment.
#[derive(Clone, Debug)]
pub(crate) enum ScopeSegment {
    Literal(Box<str>),
    Any,
}

/// The part of the API a layer applies to.
#[derive(Clone)]
pub(crate) enum Scope {
    /// Every request (a global layer).
    All,
    /// Requests whose path starts with the pattern.
    Prefix(Arc<[ScopeSegment]>),
    /// Requests for exactly this path pattern and method.
    Route {
        method: Method,
        segments: Arc<[ScopeSegment]>,
    },
}

impl Scope {
    pub(crate) fn parse(pattern: &str) -> Arc<[ScopeSegment]> {
        PathParts::new(pattern)
            .map(|part| {
                if part.value.starts_with('{') && part.value.ends_with('}') && part.value.len() > 2 {
                    ScopeSegment::Any
                } else {
                    ScopeSegment::Literal(part.value.into())
                }
            })
            .collect::<Vec<_>>()
            .into()
    }

    pub(crate) fn matches(&self, method: &Method, path: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Prefix(segments) => segments_match(segments, path, false),
            Scope::Route { method: route_method, segments } => {
                (route_method == method || (*route_method == Method::GET && *method == Method::HEAD))
                    && segments_match(segments, path, true)
            }
        }
    }
}

/// Compares path segments with the router's own splitter (repeated slashes are
/// ignored), so a scope matches at least everything the router can route to.
fn segments_match(pattern: &[ScopeSegment], path: &str, exact: bool) -> bool {
    let mut parts = PathParts::new(path);
    for segment in pattern {
        let Some(part) = parts.next() else {
            return false;
        };
        if let ScopeSegment::Literal(literal) = segment
            && part.value != &**literal
        {
            return false;
        }
    }
    !exact || parts.next().is_none()
}

#[derive(Clone)]
pub(crate) struct ScopedLayer {
    pub(crate) scope: Scope,
    pub(crate) layer: Arc<dyn Middleware>,
}
```

Change `Host::middleware` to return `&[ScopedLayer]`, `RuntimeInner.middleware` and `App.middleware` to `Vec<ScopedLayer>` / `Box<[ScopedLayer]>`, and `Next::run` to:

```rust
pub fn run(self, request: Request<RequestBody>) -> BoxFuture<HttpResponse> {
    let mut index = self.index;
    let layers = self.host.middleware();
    while let Some(entry) = layers.get(index) {
        if entry.scope.matches(request.method(), request.uri().path()) {
            let layer = Arc::clone(&entry.layer);
            return layer.handle(request, Next { host: self.host, index: index + 1 });
        }
        index += 1;
    }
    self.host.dispatch(request)
}
```

(`Next`'s `Debug` uses `middleware().len()`; keep it counting remaining entries.) In `src/app.rs`: `layer` pushes `ScopedLayer { scope: Scope::All, layer }`; add

```rust
/// Registers a layer that applies only to requests whose path starts with
/// `prefix` (which may contain `{capture}` segments). See the scoped-middleware
/// notes in the README for the matching rules.
pub fn layer_for(&mut self, prefix: &str, middleware: impl Middleware) -> &mut Self {
    self.middleware.push(ScopedLayer {
        scope: Scope::Prefix(Scope::parse(prefix)),
        layer: Arc::new(middleware),
    });
    self
}
```

- [ ] **Step 4: Verify GREEN, existing tests, commit**

Run: `cargo test --features "uuid test-util swagger multipart tls http2"` (the 16 existing middleware tests must still pass unchanged).
Expected: all pass. Mutation checks (each must make its named test FAIL, then restore; use exact test names as filters): make `segments_match` use `path.split('/')` instead of `PathParts` (`a_scope_is_never_narrower_than_routing`); drop the boundary by comparing `starts_with` on the raw string (`a_prefix_layer_does_not_cover_other_paths_or_longer_names`).

```bash
git add -A
git commit -m "feat: scoped layers with App::layer_for"
```

---

### Task 2: `App::route_layer`

**Files:** Modify `src/app.rs`; Test `tests/scoped_middleware.rs`.

**Interfaces:** Consumes Task 1's `Scope::Route`. Produces `App::route_layer(&mut self, middleware: impl Middleware) -> &mut Self` (applies to the last registered route's path pattern and method; no-op if no route yet).

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn route_layer_affects_only_that_route_and_method() {
    let mut app = App::new();
    app.get("/export", ok).route_layer(auth);
    app.post("/export", ok);
    app.get("/export/{id}", ok);
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/export").await, 401);
    assert_eq!(status(&runtime, Method::HEAD, "/export").await, 401); // HEAD falls back to GET
    assert_eq!(status(&runtime, Method::POST, "/export").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/export/7").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/export/").await, 401); // trailing slash is the same route
}

#[tokio::test]
async fn route_layer_uses_the_routes_capture_pattern() {
    let mut app = App::new();
    app.get("/items/{id}", ok).route_layer(auth);
    app.get("/items", ok);
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/items/9").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/items").await, 200);
}
```

- [ ] **Step 2: Confirm RED** (`no method named route_layer`), **Step 3: Implement**

`App` needs the last route's template and method: `self.metadata[index]` already holds `template` (the path pattern string) and `method`. Then:

```rust
/// Scopes a layer to the last registered route: its path pattern and method
/// (a `HEAD` request also matches a `GET` route).
pub fn route_layer(&mut self, middleware: impl Middleware) -> &mut Self {
    if let Some(index) = self.last_route {
        let metadata = &self.metadata[index];
        self.middleware.push(ScopedLayer {
            scope: Scope::Route {
                method: metadata.method.clone(),
                segments: Scope::parse(&metadata.template),
            },
            layer: Arc::new(middleware),
        });
    }
    self
}
```

- [ ] **Step 4: Verify GREEN, mutation-check** (make `Scope::Route` ignore the method: the POST assertion must fail), **commit**: `git commit -am "feat: App::route_layer"`.

---

### Task 3: `Group`

**Files:**
- Create: `src/group.rs`; Modify `src/lib.rs` (`mod group; pub use group::Group;`), `src/app.rs` (`App::group`)
- Test: `tests/scoped_middleware.rs`

**Interfaces:** Consumes `App` registration methods and `layer_for`. Produces `App::group<R>(&mut self, prefix: &str, f: impl FnOnce(&mut Group<'_, S>) -> R) -> R` and `Group<'a, S>` with the methods listed in the spec, each returning `&mut Group<'a, S>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn group_prefixes_routes_and_scopes_its_layer() {
    let mut app = App::new();
    app.get("/public", ok);
    app.group("/admin", |g| {
        g.layer(auth);
        g.get("/users", ok);
        g.group("/v1", |g| {
            g.post("/reset", ok);
        });
    });
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/admin/users").await, 401);
    assert_eq!(status(&runtime, Method::POST, "/admin/v1/reset").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/public").await, 200);
    let with_token = runtime
        .oneshot(Method::GET, "/admin/users", &[("x-token", "yes")], None)
        .await;
    assert_eq!(with_token.status(), 200);
}

#[tokio::test]
async fn chained_group_registration_keeps_the_prefix() {
    let mut app = App::new();
    app.group("/admin", |g| {
        g.get("/a", ok).get("/b", ok).post("/c", ok);
    });
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/admin/a").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/admin/b").await, 200);
    assert_eq!(status(&runtime, Method::POST, "/admin/c").await, 200);
    assert_eq!(status(&runtime, Method::GET, "/a").await, 404);
    assert_eq!(status(&runtime, Method::GET, "/b").await, 404);
}

#[test]
fn group_metadata_lands_on_the_prefixed_route() {
    let mut app = App::new();
    app.openapi().title("t").version("1").bearer_auth("Bearer");
    app.group("/admin", |g| {
        g.get("/users", ok).tag("admin").summary("List").security(["Bearer"]);
        g.get("/open", ok).public();
    });
    let doc = app.openapi_document();
    assert_eq!(doc["paths"]["/admin/users"]["get"]["tags"][0], "admin");
    assert_eq!(doc["paths"]["/admin/users"]["get"]["summary"], "List");
    assert_eq!(doc["paths"]["/admin/users"]["get"]["security"][0]["Bearer"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(doc["paths"]["/admin/open"]["get"]["security"], serde_json::json!([]));
}

#[tokio::test]
async fn group_route_layer_and_body_limit_apply_to_the_prefixed_route() {
    let mut app = App::new();
    app.group("/api", |g| {
        g.get("/export", ok).route_layer(auth);
        g.get("/other", ok);
    });
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/api/export").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/api/other").await, 200);
}

#[tokio::test]
async fn group_prefixes_may_contain_captures_and_join_paths_cleanly() {
    let mut app = App::new();
    app.group("/tenants/{id}/", |g| {
        g.layer(auth);
        g.get("/", ok); // the group root
        g.get("/items", ok);
    });
    let runtime = app.build().unwrap();
    assert_eq!(status(&runtime, Method::GET, "/tenants/7").await, 401);
    assert_eq!(status(&runtime, Method::GET, "/tenants/7/items").await, 401);
}
```

- [ ] **Step 2: Confirm RED** (`no method named group`).

- [ ] **Step 3: Implement `src/group.rs`**

```rust
use crate::*;

/// Joins a group prefix and a route path without doubled or missing slashes.
fn join(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{path}"),
        (false, true) => prefix.to_owned(),
        (false, false) => format!("{prefix}/{path}"),
    }
}

/// Routes registered through a group share a path prefix, and layers added to
/// the group apply only to requests under that prefix. Every method returns
/// the group itself so a chain never falls back to an unprefixed `App`.
pub struct Group<'a, S = ()> {
    app: &'a mut App<S>,
    prefix: String,
}

impl<S: Send + Sync + 'static> Group<'_, S> {
    // get/post/put/patch/delete/head/options with the same bounds as App
    // (H: Handler<S, A>), raw/raw_get with H: RawHandler<S>, static_text and
    // static_json: each calls the App method with `join(&self.prefix, path)`
    // and returns `self`.
    //
    // layer(mw): app.layer_for(&self.prefix, mw)
    // layer_for(prefix, mw): app.layer_for(&join(&self.prefix, prefix), mw)
    // group(prefix, f): nested Group with the joined prefix
    // tag/summary/operation_id/security/public/body_limit/route_layer:
    //   forward to the App method for the last registered route
}
```

Write every forwarding method out in full (the comment above lists them; there is no macro): `get`, `post`, `put`, `patch`, `delete`, `head`, `options`, `raw`, `raw_get`, `static_text`, `static_json`, `layer`, `layer_for`, `group`, `tag`, `summary`, `operation_id`, `security`, `public`, `body_limit`, `route_layer`. In `src/app.rs`:

```rust
pub fn group<R>(&mut self, prefix: &str, f: impl FnOnce(&mut Group<'_, S>) -> R) -> R {
    let mut group = Group { app: self, prefix: prefix.to_owned() };
    f(&mut group)
}
```

(`Group`'s fields need `pub(crate)` visibility for `App::group` to build it.)

- [ ] **Step 4: Verify GREEN, the forwarding-parity check, commit**

Add a test that fails if `App` gains a registration method that `Group` lacks: grep-free is impossible in Rust, so add a unit test that calls each `Group` method name once through a compile-time list (the test file already exercises get/post/chain/tag/summary/security/public/route_layer/layer/group/body_limit; add `put`, `patch`, `delete`, `head`, `options`, `raw_get`, `static_text`, `static_json`, `operation_id` calls in one `group_supports_every_registration_method` test and assert the resulting paths exist in `openapi_document()`).

Run: `cargo test --features "uuid test-util swagger multipart tls http2" --test scoped_middleware` then the whole suite.
Mutation: make `join` drop the prefix for the second chained call (return `path` unprefixed) and confirm `chained_group_registration_keeps_the_prefix` FAILS.

```bash
git add -A
git commit -m "feat: add Group for prefixed routes with scoped layers"
```

---

### Task 4: Benchmarks and the performance gate

**Files:** `benches/router.rs`, `docs/scoped-middleware-design.md` (Results)

- [ ] **Step 1: Add cases**

Extend the middleware benchmark loop: for each of `[0, 1, 3, 5]` no-op layers also run (a) all layers as `layer_for("/plaintext", noop_layer)` (matching), (b) all layers as `layer_for("/other", noop_layer)` (skipped), printing `case=middleware_scoped layers=N kind=match|skip`.

- [ ] **Step 2: Run the gate** (3 runs, Docker, `...,multipart,tls,http2`)

Gate: `layers=0` unchanged (~250 ns, 3 allocations); the global-layer rows within noise of the 0.4.0 numbers (about 404 / 490 / 570 ns for 1 / 3 / 5); report the matching-scoped and skipped-scoped rows (a skipped layer must add no allocation and little time).

- [ ] **Step 3: Record and commit** `bench: scoped middleware gates`. If a global-layer row regresses beyond noise, stop and fix.

---

### Task 5: Documentation and final verification

**Files:** `README.md`, `CHANGELOG.md`, `CLAUDE.md`, `docs/scoped-middleware-design.md` (status)

- [ ] **Step 1: README**: a "Scoped middleware" subsection under Middleware: `group`, `layer_for`, `route_layer`, the matching rules (prefix by segment, captures, trailing/repeated slashes, 404 under prefix), the raw-path caveat with the recommendation, ordering, and a short example mixing `Trace` globally with `BearerAuth` on `/admin`.
- [ ] **Step 2: CHANGELOG** (Added), **CLAUDE.md** (`ScopedLayer`/`Scope`, `PathParts` parity rule), spec status implemented.
- [ ] **Step 3: Verify** `bash scripts/verify-docker.sh`; then `bash scripts/clean.sh`.
- [ ] **Step 4: Commit** `docs: document scoped middleware`.
