# Middleware design for oas-rs

Status: draft for review. Target release: 0.2.0.

## Goal

Let applications run shared logic around every request (authentication,
tracing, CORS, timeouts) without repeating it in each handler, using a small
oas-rs-specific `Middleware` trait. Applications must pay nothing when they
register no middleware, and the cost of each registered layer must be measured
and published.

Non-goals for 0.2: per-route or per-group middleware, replacing the request
body, Tower interoperability, and a large catalogue of built-ins (CORS,
compression, rate limiting are future work).

## Public API

```rust
pub trait Middleware: Send + Sync + 'static {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse>;
}
```

- Plain `async fn`s and closures are middleware through a blanket impl for
  `Fn(Request<RequestBody>, Next) -> impl Future<Output = HttpResponse> + Send + 'static`:

  ```rust
  async fn timing(req: Request<RequestBody>, next: Next) -> HttpResponse {
      let started = Instant::now();
      let response = next.run(req).await;
      eprintln!("{} in {:?}", response.status(), started.elapsed());
      response
  }
  app.layer(timing);
  ```
- The returned future is `'static`, so a struct that implements `Middleware`
  by hand clones the state it needs (typically an `Arc`) into the future rather
  than borrowing `&self`; functions and closures that capture only owned data
  have no such concern.
- `Next` is an owned, `'static` handle (an `Arc` plus an index), so ordinary
  `async fn`s work without lifetime gymnastics. `Next::run(self, request) ->
  impl Future<Output = HttpResponse>` runs the remaining layers and then the
  normal dispatch.
- `App::layer(&mut self, middleware: impl Middleware) -> &mut Self` registers
  a global layer. The layer registered first runs outermost.
- `RequestBody` is an opaque `http_body::Body`. Middleware may read or
  modify the request head (method, URI, headers, extensions) and may read the
  body, but cannot substitute a different body, so raw handlers still receive
  Hyper's `Incoming` at the end of the chain.
- A layer may return a response without calling `next` (for example `401` or a
  CORS preflight answer). Because layers run before routing, they also see
  `404`, `405`, automatic `OPTIONS` and `HEAD` handling, and they observe the
  final response of those paths.
- Middleware returns `HttpResponse`, not `Result`. Errors are produced with
  `ApiError::...into_response()`. Panics are not caught.

## Built-in middleware

- `Trace`: calls a user callback with method, path, status and elapsed time
  after each request (`Trace::new(|record| ...)`, plus `Trace::stderr()`).
  No `tracing` dependency in 0.2.
- `BearerAuth`: extracts `Authorization: Bearer <token>` and calls an async
  user validator `Fn(String) -> Future<Output = Result<(), ApiError>>`. A
  missing or malformed header yields `401` with `WWW-Authenticate: Bearer`;
  a validator error is returned as given. `exempt_paths([...])` lists exact
  paths that skip authentication (for example `/health`, `/openapi.json`,
  `/docs`). This is the enforcement counterpart of the documentation-only
  `security` schemes added earlier; the two are configured independently.

## Architecture

- `AppRuntime` stores `Option<Arc<[Arc<dyn Middleware>]>>`. `None` is the
  existing request path, untouched: one `Option` check per request.
- With layers present, the connection wraps `Request<Incoming>` as
  `Request<RequestBody::Incoming>` and starts the chain. Each layer returns a
  boxed future, so each registered layer costs one allocation per request.
- The end of the chain (the terminal) is an object-safe host trait implemented
  by `AppRuntime<S>`: `RequestBody::Incoming` goes to the existing `prepare`
  path; `RequestBody::Full(Bytes)` (used by `AppRuntime::oneshot` in tests and
  benchmarks, behind `test-util`) goes to the existing in-memory `handle` path.
- `App::oneshot` (the builder-level test helper) does not run middleware; the
  chain is exercised through `AppRuntime::oneshot` and real TCP in tests.

## Testing

- Order of layers, early return without calling `next`, request-header
  mutation, response mutation, interaction with `404`/`405`/`OPTIONS`/`HEAD`,
  both dispatch paths (`oneshot` and a real TCP server), `BearerAuth`
  (missing, malformed, rejected, accepted, exempt path) and `Trace` callback
  fields. Tests are written before the implementation.

## Performance gate

Added to `benches/router.rs` and run through `AppRuntime::oneshot`:
`middleware_none`, `middleware_noop_x1`, `x3`, `x5`.

- `middleware_none` must match the pre-change baseline within noise and keep
  exactly 3 allocations per request (baseline ~250 ns).
- For the layered cases the work reports ns/op and allocations/op, and the
  per-layer cost (expected: one allocation per layer plus a small constant).
- A TCP loopback comparison (keep-alive and short connections) before and
  after, as for graceful shutdown. If `middleware_none` regresses, the work is
  not finished until it is fixed.

## Risks and open questions

- Boxed futures per layer are a deliberate trade for a simple, object-safe
  trait; the benchmark above quantifies the cost.
- Layers cannot know which route matched. Route-aware behavior is done by
  inspecting the path and method inside the layer; per-route layers are a
  possible later extension.
- Adding `RequestBody` and `Next` to the public API commits to them for the
  0.2 line.

## Results (measured 2026-10-08, Docker Linux, Rust 1.88, release profile)

In-memory microbenchmark (`benches/router.rs`, `AppRuntime::oneshot`, 100,000
iterations, median of 3 runs; run-to-run noise is roughly ±5%, with an
occasional outlier):

| No-op layers | ns/op | allocations/op | bytes/op |
|---|---|---|---|
| 0 | 283.8 | 3 | 666 |
| 1 | 403.3 | 5 | 2,866 |
| 3 | 489.5 | 7 | 3,490 |
| 5 | 569.8 | 9 | 4,114 |

- Zero layers keeps exactly 3 allocations and 666 bytes per request, identical
  to the pre-change path (the pre-change `plaintext` case measured 256–278 ns
  in the same session).
- Each additional layer costs about 40 ns, one allocation and about 310 bytes.
  The first layer also pays a fixed cost (about 120 ns, one extra allocation
  and about 1.9 KB) for the boxed terminal dispatch; in-memory dispatch is the
  larger state machine, so the real-connection terminal is cheaper.

TCP loopback (16 connections, 3 s, mean of 3 alternating runs):

| Scenario | `main` before | this branch, 0 layers | this branch, 1 no-op layer |
|---|---|---|---|
| Keep-alive | 223,800 req/s | 224,000 req/s (+0.1%) | 220,100 req/s (-1.7%) |
| Short connections | 60,900 req/s | 61,000 req/s (+0.2%) | 61,200 req/s (+0.5%) |

Conclusion: the no-middleware path is unchanged within measurement noise, and a
no-op layer costs about 1.7% of keep-alive throughput.
