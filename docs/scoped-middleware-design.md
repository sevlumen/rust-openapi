# Scoped middleware design for oas-rs

Status: implemented (see Results at the end). Target release: 0.5.0.

## Goal

Let a layer apply only to part of the API: a group of routes under a path
prefix, or a single route, without changing how requests are dispatched and
without cost for layers that stay global.

Non-goals: layers that depend on the matched route's handler or metadata,
changing a layer's behavior per method inside a group, replacing the request
body, and any change to `Middleware`, `Next` or `RequestBody`.

## Key decision: scope by request path, not by matched route

Layers run before routing, so a layer cannot know which route will match.
"Group" is therefore defined by the request **path prefix** (which may contain
captures such as `/tenants/{id}`). This needs no change to the dispatch hot
path, and it means a layer on `/admin` also covers `/admin/does-not-exist`
(a `404`): an authentication layer must not reveal which routes exist.

## Public API

```rust
// Groups: routes registered through `g` get the prefix; layers are scoped to it.
app.group("/admin", |g| {
    g.layer(auth);                       // applies to /admin and everything below
    g.get("/users", list).tag("admin");  // registered as /admin/users
    g.group("/v1", |g| { g.post("/reset", reset); });
});

app.layer_for("/tenants/{id}", mw);      // scope by path pattern directly
app.get("/export", export).route_layer(mw);  // this route only: its path and method
```

- `App::group(prefix, f)`; `Group` offers `get`, `post`, `put`, `patch`,
  `delete`, `head`, `options`, `raw`, `raw_get`, `static_text`, `static_json`,
  `layer`, `layer_for`, `group`, and the per-route forwarders `tag`, `summary`,
  `operation_id`, `security`, `public`, `body_limit`, `route_layer`.
- Every `Group` method returns `&mut Group` (never `&mut App`), so a chained
  `g.get("/a", h).get("/b", h2)` registers both under the prefix. Returning
  `&mut App` would silently drop the prefix on the second route.
- `App::layer_for(prefix, mw)` registers a layer scoped to a path pattern;
  `App::layer` stays the global form (scope: everything).
- `App::route_layer(mw)` scopes a layer to the last registered route: the same
  path pattern and the same method (a `HEAD` request also matches a `GET`
  route, as routing does). Other methods on the same path are not affected.

## Matching rules

- A prefix scope matches when the first N path segments of the request match
  the N segments of the pattern: literals compare exactly (case-sensitive),
  `{name}` matches any one non-empty segment. `/admin` matches `/admin`,
  `/admin/` and `/admin/x`, and does not match `/administrator`.
- Segments are split with the router's own splitter, which ignores repeated
  slashes, so a scope matches at least everything the router can route to
  (`//admin/x` and `/admin//x` included): a scope can never be narrower than
  routing, which would turn an auth layer into a bypass.
- Matching compares raw segments first. A segment that contains `%` is also
  compared after percent-decoding, because a `{capture}` route decodes its
  segment: `/api/%61dmin/x` reaches `/api/{section}/x` as `admin` and must run
  an `/api/admin` scope (found in review: at any depth where the router has a
  capture and the scope has a literal, not only at the top level). Matching is
  case-sensitive like routing; undecodable segments never match; a decoded `/`
  makes the value differ from the literal. The scope can only be widened by the
  decoding, never narrowed.
- A malformed scope pattern (`{id`, `{}`, `x{id}`) panics at registration, like
  a route template, so a typo cannot silently disable an authentication layer.
- A route scope matches the pattern exactly (same number of segments) and the
  method. Because layers run before routing it cannot know which route will be
  chosen: it also covers requests that a more specific static sibling route
  serves (`GET /items/new` under a layer on `GET /items/{id}`), and `HEAD`
  matches a `GET` route scope even when a `HEAD` route exists. For auth this
  fails closed; for audit or rate limiting it can fire on the sibling.
- `OPTIONS` (automatic preflight) does not match a method-scoped route layer;
  a prefix scope does match it.

## Ordering

Layers run in registration order, global and scoped interleaved; a scoped layer
is skipped when the request is outside its scope. The first registered matching
layer is outermost.

## Architecture

- A layer is stored as `ScopedLayer { scope, layer }`. `Scope` is `All`,
  `Prefix(segments)` or `Route { method, segments }`.
- `Next::run` walks the layer list from its index and skips entries whose scope
  does not match the request's method and path, then dispatches as before. A
  global layer costs one enum check; a prefix scope compares path segments
  without allocating.
- With no layers registered the request path is untouched.

## Testing

Written before the implementation: prefix boundary (`/admin` vs
`/administrator`, trailing slash, root), captures, nested groups and
interleaved ordering, `404`/`405`/`OPTIONS` under a prefix, `route_layer` per
method and `HEAD` for `GET`, group chaining keeps the prefix, forwarded
metadata lands on the prefixed route (OpenAPI document), and a security parity
test: for every path variant the router serves (`//admin/x`, `/admin//x`,
`/admin/x/`), the scoped layer must also run.

## Performance gate

- No layers: unchanged (`plaintext` ~250 ns, 3 allocations).
- One global layer: unchanged versus 0.4.0 numbers.
- Report the cost of a scoped layer that matches and of one that is skipped.

## Risks

- Scope by path can differ from the route a request finally reaches (a
  captured route serving a literal-looking segment, a static sibling under a
  route scope); the matching rules above are designed so a scope is never
  narrower than routing, and the over-application cases are documented.
- A new public type (`Group`) with many forwarding methods must stay in step
  with `App`'s registration methods; a test lists them.

## Results (measured 2026-10-08, Docker Linux, Rust 1.88, release profile)

In-memory microbenchmark (`benches/router.rs`, `AppRuntime::oneshot`, 100,000
iterations, median of 3 runs; run-to-run noise about +/-5%):

| Layers (all no-op) | none | global | scoped, request inside scope | scoped, request outside scope |
|---|---|---|---|---|
| 0 | 260 ns, 3 allocs | - | - | - |
| 1 | - | 390 ns, 5 allocs | 395 ns, 5 allocs | 259 ns, 3 allocs |
| 3 | - | 480 ns, 7 allocs | 502 ns, 7 allocs | 283 ns, 3 allocs |
| 5 | - | 585 ns, 9 allocs | 610 ns, 9 allocs | 285 ns, 3 allocs |

- With no layer covering the request the chain is skipped entirely: a request
  outside every scope costs the same 3 allocations and 666 bytes as an app with
  no middleware, plus about 7 ns per scoped layer to check its scope. (Before
  this check was added, such a request paid a boxed terminal dispatch: about
  +90 ns, 4 allocations and 2.5 KB.)
- A global layer is unchanged versus 0.4.0 (about 390 / 480 / 585 ns for 1 / 3 /
  5 layers, against 403 / 490 / 570 ns measured before).
- A scoped layer that matches costs about 2-4% more than a global one (segment
  comparison), still one allocation per layer.
