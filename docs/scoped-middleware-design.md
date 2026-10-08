# Scoped middleware design for oas-rs

Status: approved design, being implemented. Target release: 0.5.0.

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
- Matching uses the raw (not percent-decoded) path, like static routing. A
  request such as `/%61dmin/x` neither matches the `/admin` scope nor a static
  `/admin/x` route; it can only reach a top-level dynamic route such as
  `/{section}/x`, which is not in the group. The documentation says so, and
  recommends a global layer with explicit checks for sensitive prefixes when
  top-level dynamic routes exist.
- A route scope matches the pattern exactly (same number of segments) and the
  method.
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

- Scope by raw path (above) can differ from the route a request finally reaches
  when top-level dynamic routes or unusual encodings are involved; documented
  with the mitigation.
- A new public type (`Group`) with many forwarding methods must stay in step
  with `App`'s registration methods; a test lists them.
