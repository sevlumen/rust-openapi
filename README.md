# oas-rs

Typed HTTP routing on Hyper + Tokio with startup-generated OpenAPI 3.1
metadata. The crate is `0.x`: breaking changes are minor releases listed in the
changelog (see "Stability and support policy" below). Licensed under the MIT
License.

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

### Connection limits and timeouts

```rust
let runtime = app
    .build()?
    .header_read_timeout(Some(Duration::from_secs(10))) // default 30 s; None disables
    .max_connections(Some(10_000))                      // default: unlimited
    .on_connection_error(|error| eprintln!("connection: {error}"));
```

- `header_read_timeout` disconnects a client that takes too long to send its
  request head (slowloris). Hyper also applies it while a keep-alive
  connection waits for its next request, so an idle connection that sends
  nothing for that long is closed, so behind a proxy set it above the proxy's
  idle timeout toward this server (nginx `keepalive_timeout` 75 s, AWS ALB
  60 s) or the proxy may reuse a connection just closed and answer 502. It
  applies to HTTP/1.1, plain or over TLS; reading a request body is bounded by
  `body_read_timeout` below. For HTTP/2 it is the idle timeout instead: a
  connection with no request in flight and no response data written for that
  long is closed. Progress is judged by HTTP/2 frame type, not by write size:
  only `HEADERS` and `DATA` frames count, so a peer that only sends `PING`s
  (singly or in bursts) looks idle, while a client that reads slowly but
  steadily, even with 16-byte windows, never does. A streaming (or large
  buffered) response counts as in flight until it ends. `send_timeout`
  (default 60 s, `None` disables) frees the connection of a client that stops
  reading a streaming or large response, on HTTP/1.1 and HTTP/2 alike: the
  clock runs while the socket refuses a write (its buffers are full because
  the client is not reading) and, under HTTP/2, while a response body waits for
  a window the client never opens even though it keeps answering `PING`s. A
  quiet SSE stream and a slow but steady reader are left alone.
- `body_read_timeout` (default 60 s, `None` disables) bounds how long a
  *buffered* request body (`Json`, `Form`, `Multipart`) may take to arrive in
  full; a body still incomplete then gets `408` and the connection is closed,
  so a header followed by a trickle (or nothing) cannot hold a handler task and
  a `max_connections` slot. It is a deadline for the whole body: raise it for
  routes that take large uploads over slow links. Raw handlers and streaming
  uploads read their own body: use `tokio::time::timeout` around their reads.
- `http2_max_concurrent_streams(Some(n))` (feature `http2`) lowers the 200
  streams one h2 connection may have open; excess streams are refused with
  `REFUSED_STREAM`. Each open stream can buffer up to its route's body limit,
  so this bounds the memory one client can pin.
- `max_connections` stops accepting while the cap is reached. Waiting clients
  sit in the operating system's listen backlog; shutdown is not delayed. Over
  TLS the handshake counts toward the cap. Idle keep-alive connections keep
  their slot until `header_read_timeout` closes them: do not combine a limit
  with `header_read_timeout(None)`.
- `on_connection_error` sees every error that ends a connection (malformed
  requests, timeouts, I/O failures, failed or timed-out TLS handshakes,
  HTTP/2 connection errors); a client that disconnects mid-request appears
  there too. It is not called for handler errors.

`CatchPanic` turns a panic in a handler into a `500` problem-details response
and keeps the connection serving (`app.layer(CatchPanic::new())`, or
`CatchPanic::with_hook(|message| ...)` to log the message, which is never sent
to the client). It needs unwinding, so it does nothing under
`panic = "abort"`. A panic can poison a `std::sync::Mutex` that later requests
use, and a handler that panicked before reading the request body may make
Hyper close the connection after the `500`.

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

`#[derive(ApiSchema)]` supports structs with named fields and enums. Field
types may be scalars (`String`, `bool`, integers, `f32`/`f64`, `Uuid`),
`Option`, `Vec`, `Box`, string-keyed maps, `serde_json::Value`, or another
`ApiSchema` type. Unit-variant enums derive a string `enum` schema; enums with
data derive `oneOf` for serde's tagged representations: externally tagged (the
default), internally tagged (`tag = "..."`) and adjacently tagged (`tag` and
`content`), and `anyOf` for `untagged` (whose payloads can overlap).

Serde attributes reflected in the schema: `rename`, `rename_all`, `skip`
(field omitted), `skip_serializing` (`writeOnly`), `skip_deserializing`
(`readOnly`, not required), `default` and `skip_serializing_if` (not
required), `flatten` (merged with `allOf`), and the enum `tag`/`content`/
`untagged` forms. Doc comments become `description`s. Field-level
`#[api_schema(...)]` adds `description`, `example`, `minimum`, `maximum`,
`min_length`, `max_length`, `pattern`, `min_items`, `max_items`, `format` and
`deprecated`:

```rust
/// A user account.
#[derive(Serialize, Deserialize, ApiSchema)]
struct Account {
    /// Login name.
    #[api_schema(example = "ada", min_length = 2, pattern = "^[a-z]+$")]
    login: String,
    #[serde(flatten)]
    audit: Audit,
}
```

Also understood: `transparent`, container `default`, `deny_unknown_fields`,
`rename_all_fields` and a variant-level `untagged`. `Option<T>` is nullable
(`oneOf: [T, null]`). Query structs with `skip`, `flatten` or `default` fields
fall back to serde for parsing; a `flatten`ed struct's fields are listed as
parameters too. `rename(serialize = .., deserialize = ..)` with different names
is a compile error (one schema serves both directions).
Not interpreted: `alias`, `with`, `serialize_with`/`deserialize_with`,
`bound`, and generic types (the derive reports an error: implement
`ApiSchema` by hand). A `flatten`ed externally tagged enum with unit variants
is not representable. `skip_serializing_if` and `skip_deserializing` make a
field not `required`, which also loosens what request bodies must contain.

Named schemas (structs and enums that derive `ApiSchema`) are written once under
`components.schemas` and referenced with `$ref`, so a type used in several
routes, or nested in another, appears a single time (recursive types work).
A hand-written `ApiSchema` impl stays inline unless it implements `schema_with`
and calls `SchemaRegistry::define`. Rename a component with
`#[api_schema(name = "ItemDto")]`; two different types with the same name make
`build()` fail with `BuildError::SchemaNameConflict`.

Every route also documents the errors the framework itself returns, all with
the shared `Problem` schema (`type`, `title`, `status`, `detail`, media type
`application/json`, which is what the server sends): `400` for routes with typed
path/query/header parameters or a JSON body, `413` and `415` for routes with a
body, and `401` for routes whose declared security (`.security(..)` or the
document default) is non-empty. It follows the declarations, not middleware:
a layer that enforces auth without `.security(..)` is not reflected, and raw
routes and custom extractors declare nothing. Turn it off with
`app.openapi().document_errors(false)`. Do not name your own type `Problem`
while it is on and some route documents errors. The component name of a
derived type is its Rust name (a container `#[serde(rename)]` does not change
it); use `#[api_schema(name = "...")]` to change it.

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
- TLS 1.2 and 1.3 with rustls' safe defaults. Without the `http2` feature the
  server speaks HTTP/1.1 only: ALPN advertises only `http/1.1`, so a client that
  offers only `h2` is rejected during the handshake (see "HTTP/2" below for the
  feature). No client certificates.
- The handshake runs per connection, so a slow client never blocks accepting
  others, and a failed or timed-out handshake closes only that connection.
- Shutdown behaves like `serve_listener`: stop accepting, let in-flight
  requests finish, bounded by `shutdown_timeout`; idle keep-alive connections
  are closed and half-finished handshakes are dropped.
- A runnable example lives in [`examples/tls.rs`](examples/tls.rs)
  (`cargo run --example tls --features tls`).
- On a loopback benchmark, steady-state TLS throughput was about 9% below plain
  TCP; with short connections the handshake dominates (see
  `docs/tls-design.md`). Reuse connections where you can.

### HTTP/2

Enable the `http2` feature (it implies `tls`) and `serve_tls` also serves HTTP/2
to clients that negotiate `h2` through ALPN; everything else keeps working as
HTTP/1.1. Routing, extractors, middleware and graceful shutdown behave the same
over both protocols.

- By default it is HTTP/2 **over TLS** only and the plain HTTP/1.1 path is
  unchanged; cleartext HTTP/2 for a trusted proxy is opt-in (see "Listener
  options" below).
- `TlsConfig::from_pem_files(..)?.enable_http2(false)` offers HTTP/1.1 only; a
  client that offers only `h2` is then rejected during the handshake.
- HTTP/2 requests carry the authority in the request URI and have no `Host`
  header.
- Capacity: Hyper allows up to 200 concurrent streams per HTTP/2 connection
  (lower it with `http2_max_concurrent_streams`), so by default one h2
  connection can hold up to 200 times a route's `body_limit` of buffered
  request bodies (about 3x that for multipart) and 200 handler tasks, versus
  one of each on an HTTP/1.1 connection. `header_read_timeout` covers HTTP/1.1
  only: an h2 stream that sends headers and never finishes holds its slot, so
  bound this with a reverse proxy or OS limits when exposed to untrusted
  clients.
- Hyper limits an h2 request's header list to 16 KiB (larger gets a `431` and a
  reset stream), stricter than the HTTP/1.1 path; clients with very large
  cookies or tokens will notice.
- Cargo feature unification: enabling `http2` anywhere in a dependency graph
  turns ALPN `h2` on for every `TlsConfig` in that binary; `enable_http2(false)`
  is the opt-out and only exists when the feature is on.
- HTTP/2 multiplexes many requests over one connection, which pays off with many
  concurrent requests; for tiny responses at low concurrency a single h2 stream
  was slower than HTTP/1.1 on a loopback benchmark (see `docs/http2-design.md`).

### Listener options

- **Unix domain socket** (Unix only): `runtime.serve_unix(UnixListener::bind(path)?,
  shutdown).await?` serves HTTP/1.1 (plus h2c when enabled) with the same
  shutdown, timeouts, connection limit and observer as `serve_listener`. The
  socket file gets the process umask (restrict it with `chmod`), a stale file
  must be removed before binding, and it is not removed on shutdown.
- **h2c** (feature `http2`): `runtime.h2c(true)` makes `serve_listener` and
  `serve_unix` also accept HTTP/2 with prior knowledge on the same port, picked
  from the first bytes of the connection. For a proxy that speaks h2c to its
  upstream; browsers do not. A connection must send its first bytes within
  `header_read_timeout` (a partial preface counts), and after that the timeout
  applies to HTTP/1.1 requests only.
- **`SO_REUSEPORT`, backlog, buffers**: you build the listener, so set them
  with Tokio's `TcpSocket` and pass the result to `serve_listener`:

```rust
let socket = tokio::net::TcpSocket::new_v4()?;
socket.set_reuseaddr(true)?;
#[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
socket.set_reuseport(true)?;
socket.bind("0.0.0.0:8080".parse()?)?;
let listener = socket.listen(1024)?;
runtime.serve_listener(listener, shutdown).await?;
```

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
- Uploads through the `Multipart` extractor are buffered: budget about 3x the
  route's `body_limit` of memory per concurrent upload. For large files stream
  instead (below).
- The route is documented in OpenAPI as a `multipart/form-data` request body.
  Add the fields with `.multipart_fields([MultipartField::file("firmware")
  .required().content_type("application/octet-stream"),
  MultipartField::text("version").required()])` (it documents only; it does
  not validate).

### Streaming uploads

A raw handler gets the connection's body unbuffered; `Multipart::from_stream`
turns it into a `Multipart` whose parts are read as they arrive, so memory is
about one chunk however large the file (for a well-formed body; a body that
never reaches a boundary, or whose part headers never end, is refused with
`400` after 256 KiB rather than buffered):

```rust
use hyper::body::Incoming;

app.raw(Method::POST, "/videos", |request: Request<Incoming>| async move {
    let mut form = Multipart::from_stream(request, 2 * 1024 * 1024 * 1024)?; // 2 GiB
    let mut total = 0;
    while let Some(mut field) = form.next_field().await? {
        while let Some(chunk) = field.chunk().await? {
            total += chunk.len(); // write `chunk` to a file instead
        }
    }
    Ok::<_, ApiError>(total.to_string())
})
.multipart_fields([MultipartField::file("video").required()]);
```

The limit is yours to choose (raw routes have no `body_limit`): a larger
declared `Content-Length` is refused with `413` before reading, and a chunked
body is cut off with `413` as soon as it passes the limit. Authentication
layers run before the handler, so register them first.

- An early answer (any `?` above) ends the connection. A client that is still
  sending usually sees a reset instead of the `413`/`400`; clients that send
  `Expect: 100-continue` (curl does for large bodies) read it reliably.
- There is no body read timeout: wrap `next_field()` and `chunk()` in
  `tokio::time::timeout`, otherwise a stalled upload holds its connection (and a
  `max_connections` slot) indefinitely.
- Streamed `Field::text` decodes UTF-8; the buffered extractor honours the
  part's `charset`.
- `oneshot` cannot call raw routes: test them over `serve_listener`.

## Web building blocks

**Response helpers.** `ResponseExt` adds headers, a status or cookies to any
response, and `Redirect`, `Html` and `SetCookie` cover the common cases:

```rust
use oas_rs::{Html, Redirect, ResponseExt, SameSite, SetCookie};

async fn login() -> impl oas_rs::IntoResponse {
    Redirect::see_other("/home")
        .with_cookie(SetCookie::new("sid", "abc").path("/").http_only().secure().same_site(SameSite::Lax))
}
async fn page() -> Html<&'static str> { Html("<h1>hi</h1>") }
```

`SetCookie::new` panics on a name or value a cookie may not contain
(percent-encode values first). Several `with_cookie` calls send several
`Set-Cookie` headers. `Redirect` appears in the OpenAPI document as `302`
whatever constructor is used.

**Cookies and forms.** The `Cookies` extractor reads every `Cookie` header
(`cookies.get("sid")`; malformed pairs are skipped, values are not
percent-decoded). `Form<T>` decodes an `application/x-www-form-urlencoded`
body (`+` is a space; each value is parsed into its field's own type, so a
`String` keeps `123456` as text; a repeated key keeps the last value; `a[]=1`
and nested keys are not supported) into a type that derives `ApiSchema` and
`Deserialize`, and documents the form in OpenAPI; other media types get `415`.

**Peer address.** `runtime.connect_info(true)` (off by default: it adds a
request extension) enables the `ConnectInfo(addr)` extractor, `peer_addr(&request)`
in middleware and `RateLimit::key_by_peer_ip()`. Behind a proxy the peer is the
proxy; key on a header it sets instead. Unix sockets and in-process requests
have no address.

**Server-sent events.** Return `Sse::new(stream)` where the stream yields
`Event::data("...")` (optionally `.event("name")`, `.id("1")`, `.retry(..)`);
`.keep_alive(Duration)` sends `: keep-alive` comments in the silences. The
connection stays open until the stream ends or the client leaves.

**Static files** (feature `static-files`). `app.layer(ServeDir::new("/assets",
"./public"))` serves `GET`/`HEAD` for files under the prefix, falling through to
your routes for everything else. It sends `Content-Type`, `Content-Length`, a
weak `ETag` and `Last-Modified` (`304` on conditional requests), streams large
files, serves `index.html` for directories (a directory without a trailing
slash is redirected with `308` so relative links work) and refuses `..`, hidden
files (also through symlinks and Windows 8.3 short names) and symlinks that
leave the directory. No `Range` requests (it answers `Accept-Ranges: none`),
directory listings or precompressed variants. `.cache_control("public,
max-age=3600")`, `.index_file("start.html")` and `.allow_dotfiles(true)`
configure it. The layer runs before routing: a file under the prefix wins over
a route with the same path, and layers registered after it (authentication,
rate limits, `Cors`) do not run for files, so mount it under a prefix of its
own (ideally `app.layer_for("/assets", ServeDir::new(..))`) and register `Cors`
first if fonts are fetched cross-origin.

**WebSocket** (feature `websocket`, over `tokio-tungstenite`). Use
`WebSocketUpgrade` in a raw route, which receives the `Request<Incoming>` an
upgrade needs:

```rust
use hyper::body::Incoming;
use oas_rs::{Message, WebSocketUpgrade};

app.raw(Method::GET, "/ws", |request: Request<Incoming>| async move {
    WebSocketUpgrade::new(request)
        .allow_origin("https://app.example")
        .on_upgrade(|mut socket| async move {
            while let Some(Ok(message)) = socket.recv().await {
                if let Message::Text(text) = message {
                    let _ = socket.send(Message::Text(text)).await;
                }
            }
        })
});
```

Layers (authentication, rate limits) run before the handshake like for any
route; a request that is not a valid handshake gets `400` (`426` for a wrong
version) and a disallowed origin `403`. Browsers do not apply CORS to
WebSockets, so list the allowed origins with `allow_origin` for
cookie-authenticated endpoints (a request with no `Origin` header, which is not
a browser, is not blocked by that check). `protocols([...])` negotiates a
subprotocol (the server's first preference the client also offered);
messages above `max_message_size` (1 MiB by default) end the session; pings are
answered automatically; `idle_timeout(..)` ends a silent session. Limits:
HTTP/1.1 only (no RFC 8441 over HTTP/2), no `permessage-deflate`, and after the
upgrade the connection belongs to your handler task: graceful shutdown does not
wait for it (watch your own shutdown signal if sessions must be closed
politely), `header_read_timeout` no longer applies, and a panic in the handler
is not seen by `CatchPanic`. An open session keeps its `max_connections` slot.
The route appears in the OpenAPI document as a `101` response; its `400`,
`403` and `426` rejections are not listed.

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
different body. Panics are not caught. `BearerAuth` *enforces* a bearer token; `OpenApiOptions::bearer_auth`
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

`RequestId::new()` keeps a usable `X-Request-Id` (1 to 128 characters of
`A-Z a-z 0-9 - _ . :`) or generates a unique, non-secret one, shows it to
handlers (read it with a `HeaderSpec` for `x-request-id`) and sets it on every
response. Register it first so the id exists for the layers after it.
For a single shared token use `BearerAuth::static_token("...")`; for your own
validator compare secrets with `constant_time_eq(a, b)`.

### CORS

```rust
app.layer(
    Cors::new()
        .allow_origin("https://app.example")
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(["content-type", "authorization"])
        .max_age(Duration::from_secs(600)),
);
app.layer(BearerAuth::static_token("..."));   // after Cors
```

`Cors` answers preflights itself (an explicit `OPTIONS` route is never reached
for a preflight) and adds `Access-Control-*` headers to responses for allowed
origins, error responses included. With listed origins every response, with or
without an `Origin` header, carries `Vary: Origin` so shared caches stay
correct; `allow_any_origin()` sends `Access-Control-Allow-Origin: *` on every
response. `content-type` is allowed by default (JSON bodies need it): add
`authorization` with `allow_headers` for bearer tokens, which replaces the
list. `allow_methods` replaces the default `GET`, `HEAD`, `POST`.
Register it **before** `BearerAuth`: browsers send preflights without
credentials. Use `layer`, `layer_for` or a group layer, not `route_layer`
(a route scope matches only its own method, never the `OPTIONS` preflight).
A request from an origin that is not allowed is still served but gets no CORS
headers. `allow_any_origin()` cannot be combined with `allow_credentials(true)`
(it panics; list the origins instead), and `allow_origin` panics on anything
but `scheme://host[:port]`.

### Rate limiting

```rust
app.layer(Cors::new().allow_origin("https://app.example"));
app.layer(
    RateLimit::new(100, Duration::from_secs(60))   // 100 requests a minute
        .burst(20)
        .key_by_header("x-api-key"),
);
```

`RateLimit` is a token bucket held in this process: a request that finds the
bucket empty gets `429` with `Retry-After` and goes no further. By default one
bucket serves everyone; `key_by_header` / `key_by` give each client its own
(requests without a key share an anonymous bucket). At most `max_keys`
(10,000) buckets are kept: idle ones are dropped first and, when the table is
full of busy keys, new keys share one overflow bucket, so varying the key
cannot bypass the limit. The middleware does not see the socket, so there is
no per-address key: behind a proxy key on the header it sets (and only if it
overwrites what clients send). With several instances each enforces its own
limit.

### Compression

Enable the `compression` feature and register `Compress::new()`: buffered
text, JSON, XML, JavaScript and SVG responses of at least 1 KiB are gzipped
for clients whose `Accept-Encoding` allows it (`min_size` and `level` are
configurable), with `Content-Encoding`, a corrected `Content-Length` and a weak
`ETag`. The choice depends on the length alone (never below 32 bytes), so a
`HEAD` gets the same `Content-Encoding` and weak `ETag` as its `GET` (without a
length); a body that does not shrink is still encoded, a few bytes larger.
Streaming responses, bodiless statuses, already-encoded responses and other
media types pass through, and any response that could have been compressed
carries `Vary: Accept-Encoding`. Gzip is built in; the
`compression-brotli` feature (pure-Rust `brotli`) adds `br`, chosen when the
client's quality for it is at least gzip's (`Compress::brotli_quality`, default
4). There is no zstd. Do not use compression on endpoints that mix secrets with
attacker-chosen input over TLS (BREACH).

### Custom error format

```rust
app.layer(ErrorFormat::new(|info: &ErrorInfo| {
    info.respond(serde_json::json!({ "error": info.detail }))
}));          // register it first
```

`ErrorFormat` rewrites every error the framework produces (bad parameters or
bodies, `404`, `405`, `413`, `BearerAuth` failures and each `ApiError` a
handler returns) so you can keep an existing error contract. Responses your
own handlers build are untouched, `Allow` and `WWW-Authenticate` are carried
over, and `HEAD` errors stay bodiless. The OpenAPI document keeps describing
the default `Problem` schema, so use `app.openapi().document_errors(false)`
when you change the format.

### All headers

`Header<T>` reads one declared header. For many or dynamic headers extract
`Headers(map)`: it hands you the request's `HeaderMap` (a clone per request)
and adds no OpenAPI parameters. Its `Debug` output includes every header
value, `Authorization` and `Cookie` included, so do not log it.

### Scoped middleware

A layer can apply to part of the API only. Layers still run before routing, so
"part of the API" is defined by the request **path**:

```rust
app.layer(Trace::stderr());                     // every request
app.group("/admin", |g| {
    g.layer(BearerAuth::new(validate));         // only /admin and below
    g.get("/users", list).tag("admin");         // registered as /admin/users
    g.group("/v1", |g| { g.post("/reset", reset); });
});
app.layer_for("/tenants/{id}", tenant_check);   // scope by path pattern
app.get("/export", export).route_layer(audit);  // this route's path and method
```

- Every `Group` method returns the group itself, so `g.get("/a", h).get("/b", h2)`
  registers both routes under the prefix. Per-route calls such as `.tag(..)`,
  `.security(..)`, `.public()`, `.body_limit(..)` and `.route_layer(..)` work on the
  route just registered.
- A prefix matches whole segments: `/admin` covers `/admin`, `/admin/` and
  `/admin/x`, not `/administrator`; `{name}` matches any one segment. A prefix
  also covers paths with no route under it (a `404`), so an authentication
  layer does not reveal which routes exist.
- Segments are split like the router does (repeated slashes are ignored), so a
  scope is never narrower than routing.
- `route_layer` is scoped by the route's path **pattern** and method, because
  layers run before routing: it also covers requests that a more specific sibling
  route would serve (a layer on `GET /items/{id}` runs for `GET /items/new` even
  though `/items/new` is its own route), and a `HEAD` request matches a `GET`
  route's layer even when an explicit `HEAD` route exists. Other methods on the
  same path, and automatic `OPTIONS` answers, are not affected. Call it right
  after registering the route (with no route registered it does nothing).
- A malformed scope pattern (`/admin/{id`, `{}`, `x{id}`) panics when the layer
  is registered, like a malformed route template, instead of silently never
  matching.
- Layers run in registration order, global and scoped interleaved; a scoped
  layer is skipped for requests outside its scope. A request that no layer
  covers skips the middleware chain completely (nearly the same cost as an app
  without middleware: about 7 ns per scoped layer to check its scope); a
  matching scoped layer costs about as much as a global one.
- Percent-encoding: a segment that contains `%` is also compared after
  decoding, because a `{capture}` route decodes its segment (`/api/%61dmin/x`
  reaches `/api/{section}/x` as `admin`). So an `/api/admin` scope covers that
  request. Matching stays case-sensitive (`Admin` is a different section, like
  in routing), a segment that does not decode never matches, and a decoded `/`
  (`%2F`) makes the value differ from the literal. Your handler still receives
  the decoded value, so validate it when it is security relevant.

## Installation

```toml
[dependencies]
oas-rs = "0.9"
```

Enable optional features as needed: `swagger` (Swagger UI), `uuid` (UUID
extraction and schema support), `multipart` (`multipart/form-data` uploads),
`tls` (HTTPS via `serve_tls`), `http2` (HTTP/2 over TLS), `test-util` (in-process `oneshot` testing).
The minimum supported Rust version is 1.88.

## Stability and support policy

- **Versioning.** Semantic versioning. While the crate is `0.x`, a breaking
  change to the public API or to documented HTTP/OpenAPI behaviour is a minor
  bump and is listed under `### Changed` in the changelog; patch releases only
  fix bugs. From `1.0` breaking changes need a major version. Releases are
  checked against the previous one with `cargo semver-checks`.
- **What is public API.** Everything re-exported from the crate root, plus the
  `Cargo` features documented here. `#[doc(hidden)]` items (for example
  `__private`, used by the derive) are not. The shape of the generated
  `openapi.json` is documented behaviour; its key order is not.
- **MSRV.** Rust 1.88. Raising it is a minor release (never a patch) and is
  noted in the changelog; the previous release keeps working.
- **Security fixes.** The latest minor release gets them. Report problems
  privately through the repository's security advisory page.
- **Testing your app.** The `test-util` feature (`AppRuntime::oneshot`,
  `TestResponse`) is supported API for in-process tests; it cannot call raw
  routes, which need a real listener (`serve_listener` on port 0).
- **Not provided:** HTTP/3, zstd, and an in-repository HTTP load laboratory
  (see `docs/benchmark-design.md`: it lives in a separate project).

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
cargo test --workspace --all-targets --features 'uuid test-util swagger multipart tls http2 compression compression-brotli static-files websocket'
cargo test --doc --workspace
cargo build --workspace --examples --features 'uuid swagger tls http2'
```

The Miri inline-future safety run (`cargo +nightly miri test --lib
inline_future`) is a permanent gate before every release.

Documentation is built with `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
--no-deps --all-features`. Run `bash scripts/verify-docker.sh` to execute all of
the above in Docker with the pinned toolchain (Rust 1.88, the minimum supported
version) before committing.

## Roadmap

Not provided, and not planned for now: HTTP/3 (a QUIC stack) and zstd
compression (a C dependency). The paired HTTP acceptance benchmark against a raw
Hyper server is designed (see `docs/benchmark-design.md`) to live in a separate
`oas-rs-perf` project and has not been run for this release line.

## Contributing and license

See [CONTRIBUTING.md](CONTRIBUTING.md) and [CHANGELOG.md](CHANGELOG.md). This
project is licensed under the [MIT License](LICENSE-MIT).
