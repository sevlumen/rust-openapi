# Multipart upload design for oas-rs

Status: draft for review. Target release: 0.3.0.

## Goal

Let a handler accept `multipart/form-data` uploads (for example firmware files
sent to `POST /firmwares`) with a per-route size limit, an OpenAPI
`requestBody` for the route, and no cost for applications that do not enable
the feature.

Non-goals: streaming multi-gigabyte uploads (use a raw handler), per-field size
limits, resumable uploads, writing files to disk.

## Findings that shape the design

- `App::max_body_size` is *global*: it rewrites the limit of every buffered
  route already registered and applies to later ones. A 4 MiB upload route would
  therefore raise the limit of every JSON route. A per-route limit is needed.
- Extractors are synchronous (`FromRequest::from_request`) over a body that has
  already been buffered into `Bytes` up to the route's limit. A multipart
  extractor therefore works on an in-memory body; the *handler* reads the parts
  asynchronously.

## Public API

Feature `multipart` (off by default; adds the `multer` dependency):

```rust
pub struct Multipart { /* wraps multer::Multipart<'static> */ }
impl Multipart {
    pub async fn next_field(&mut self) -> Result<Option<Field>, ApiError>;
}
pub struct Field { /* wraps multer::Field<'static> */ }
impl Field {
    pub fn name(&self) -> Option<&str>;
    pub fn file_name(&self) -> Option<&str>;
    pub fn content_type(&self) -> Option<&str>;
    pub async fn bytes(self) -> Result<Bytes, ApiError>;
    pub async fn text(self) -> Result<String, ApiError>;
}
```

`multer` types are wrapped, not re-exported, so a `multer` upgrade is not a
breaking change for oas-rs users.

A per-route limit, available regardless of the feature:

```rust
app.post("/firmwares", upload).body_limit(4 * 1024 * 1024);
```

`App::body_limit(&mut self, bytes: usize) -> &mut Self` sets the limit of the
last registered buffered route (like `tag`/`summary`); it has no effect on a
route without a buffered body (raw handlers, no body) and is documented as
such. `max_body_size` keeps its current global behavior and its documentation
is corrected to say so.

Usage:

```rust
async fn upload(mut form: Multipart) -> Result<Json<Uploaded>, ApiError> {
    while let Some(field) = form.next_field().await? {
        if field.name() == Some("firmware") {
            let file_name = field.file_name().map(str::to_owned);
            let data = field.bytes().await?;
            // store `data`...
        }
    }
    Ok(Json(Uploaded { .. }))
}
```

## Behavior

- Extraction requires `Content-Type: multipart/form-data; boundary=...`.
  Another media type returns `415`; a missing or invalid boundary returns `400`;
  a malformed body surfaces as `400` from `next_field`/`bytes`/`text`.
  All errors use the existing problem-details `ApiError`.
- A body larger than the route limit returns `413` through the existing
  buffered-body path, before the handler runs.
- The body is wrapped in a one-item `Stream` (a tiny local type over
  `futures_core::Stream`, no `futures-util` dependency) and handed to `multer`.
- OpenAPI: the route gets a `requestBody` with content `multipart/form-data`
  and schema `{ "type": "object", "additionalProperties": { "type": "string",
  "format": "binary" } }`. Documenting named fields is a future extension.

## Testing

Written before the implementation: text and binary parts, several files, empty
field, missing boundary (`400`), wrong content type (`415`), body over the route
limit (`413`), a route limit that differs from the global limit (a JSON route
keeps the default while `/firmwares` accepts 4 MiB), tricky file names (quotes,
unicode, path separators are returned as-is, never used by the framework), a
truncated body, a body that contains the boundary text inside a part, and an
end-to-end upload over real TCP. A test confirms that with the feature
disabled nothing in the default build changes.

## Performance gate

- Default build (feature off): `plaintext`/`static_route_count` unchanged
  (about 250 ns, 3 allocations); `body_limit` does no work on the request path.
- A benchmark parses a 4 MiB two-part upload through `oneshot` and reports ns,
  MiB/s and allocations per request.

## Risks and open questions

- `multer` adds dependencies only behind the feature; `cargo deny` must still
  pass with the default features and with `--all-features`.
- If `multer::Multipart<'static>` turns out not to be `Send`, the wrapper owns
  the parser behind a `Mutex`-free design chosen at implementation time; the
  public API above does not change.
