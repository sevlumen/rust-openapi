use std::sync::atomic::{AtomicU64, Ordering};

use futures_core::Stream;

use crate::*;

/// How many body bytes `Multipart::from_stream` may pull without yielding a
/// part header or a piece of data. `multer` buffers everything it has not been
/// able to parse yet (a preamble with no boundary, part headers with no
/// blank line), so without this a malformed body could pin the whole `limit`.
const MAX_UNPRODUCTIVE: u64 = 256 * 1024;

/// Bytes pulled from the body since the parser last produced something.
#[derive(Default)]
struct Progress(AtomicU64);

impl Progress {
    fn made(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

/// The request body as a stream that fails when the parser is not
/// consuming it (see [`MAX_UNPRODUCTIVE`]).
struct GuardedBody {
    inner: http_body_util::BodyDataStream<Incoming>,
    progress: Arc<Progress>,
}

impl Stream for GuardedBody {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(context) {
            Poll::Ready(Some(Ok(bytes))) => {
                // What was pulled before this chunk and produced nothing. A
                // single large chunk from the connection is not a violation by
                // itself, so the bound is `MAX_UNPRODUCTIVE` plus one chunk.
                let before = self
                    .progress
                    .0
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                if before > MAX_UNPRODUCTIVE {
                    return Poll::Ready(Some(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "multipart preamble or part headers too large",
                    ))));
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(std::io::Error::other(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

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
        // The handler asked for the next part while still holding the previous
        // `Field`: a programming error, not a client error.
        multer::Error::LockFailure => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "drop the previous multipart Field before requesting the next one",
        ),
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

fn boundary_of(headers: &http::HeaderMap) -> Result<String, ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    multer::parse_boundary(content_type).map_err(|error| match error {
        multer::Error::NoBoundary => {
            ApiError::bad_request("multipart Content-Type has no boundary")
        }
        _ => ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type",
            "expected multipart/form-data",
        ),
    })
}

/// A `multipart/form-data` request body. Extract it in a handler and read the
/// parts with [`next_field`](Self::next_field). The body is buffered up to the
/// route's size limit (see `App::body_limit`); for large uploads use
/// [`from_stream`](Self::from_stream) in a raw handler, which reads the body
/// as it arrives.
pub struct Multipart {
    inner: multer::Multipart<'static>,
    /// Set for `from_stream`: the guard against unparsable data piling up.
    progress: Option<Arc<Progress>>,
}

impl Multipart {
    /// Reads a multipart body straight from the connection, without
    /// buffering it: memory use is about one chunk plus whatever the handler
    /// keeps. Use it in a raw handler (`App::raw`), which receives the
    /// streaming `Request<Incoming>`.
    ///
    /// Memory use: about one chunk for a well-formed body. A body that never
    /// produces a boundary or ends a part's headers is refused with `400` after
    /// 256 KiB instead of being buffered up to `limit`.
    ///
    /// There is no read timeout on the body: a client that stops sending keeps
    /// the handler (and its connection) waiting, so wrap `next_field()` and
    /// `chunk()` in `tokio::time::timeout`. Answering early (any `?` from here)
    /// ends the connection, and a client that is still uploading usually
    /// never sees the error status unless it sent `Expect: 100-continue`.
    ///
    /// `limit` is the most bytes the whole body may have. A larger declared
    /// `Content-Length` is refused with `413` before anything is read, and a
    /// body without a length (chunked) is cut off with `413` once it passes
    /// `limit`, so a client cannot exceed it by not declaring a size. A missing
    /// boundary is a `400`, another content type a `415`. Read parts with
    /// [`Field::chunk`] to stream to disk, or [`Field::bytes`] to buffer one
    /// part.
    pub fn from_stream(request: Request<Incoming>, limit: u64) -> Result<Self, ApiError> {
        let declared = request
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if declared.is_some_and(|declared| declared > limit) {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large",
                "request body exceeds the configured limit",
            ));
        }
        let boundary = boundary_of(request.headers())?;
        let constraints =
            multer::Constraints::new().size_limit(multer::SizeLimit::new().whole_stream(limit));
        let progress = Arc::new(Progress::default());
        let stream = GuardedBody {
            inner: request.into_body().into_data_stream(),
            progress: Arc::clone(&progress),
        };
        Ok(Multipart {
            inner: multer::Multipart::with_constraints(stream, boundary, constraints),
            progress: Some(progress),
        })
    }

    /// The next part, or `None` after the last one.
    ///
    /// The previous [`Field`] must be dropped (or consumed with
    /// [`Field::bytes`] / [`Field::text`]) before calling this again; holding
    /// it makes this return a `500` error, because that is a bug in the
    /// handler rather than a bad request.
    pub async fn next_field(&mut self) -> Result<Option<Field>, ApiError> {
        let field = self.inner.next_field().await.map_err(map_error)?;
        if let Some(progress) = &self.progress {
            progress.made();
        }
        Ok(field.map(|inner| Field {
            inner,
            progress: self.progress.clone(),
        }))
    }
}

/// One part of a multipart body.
pub struct Field {
    inner: multer::Field<'static>,
    progress: Option<Arc<Progress>>,
}

impl Field {
    pub fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    /// The client-supplied file name as sent, with only the quoted-string
    /// escape `\"` removed (never used by the framework). Treat it as
    /// untrusted input: do not use it as a filesystem path unsanitized.
    pub fn file_name(&self) -> Option<&str> {
        self.inner.file_name()
    }

    pub fn content_type(&self) -> Option<&str> {
        self.inner.content_type().map(|mime| mime.as_ref())
    }

    /// The next piece of this part as it arrives (`None` at its end). With
    /// [`Multipart::from_stream`] this is how a large file is written out
    /// without holding all of it.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, ApiError> {
        let chunk = self.inner.chunk().await.map_err(map_error)?;
        if let Some(progress) = &self.progress {
            progress.made();
        }
        Ok(chunk)
    }

    pub async fn bytes(mut self) -> Result<Bytes, ApiError> {
        if self.progress.is_none() {
            return self.inner.bytes().await.map_err(map_error);
        }
        // Streaming: collect through `chunk` so progress is recorded.
        let mut buffer = bytes::BytesMut::new();
        while let Some(chunk) = self.chunk().await? {
            buffer.extend_from_slice(&chunk);
        }
        Ok(buffer.freeze())
    }

    /// The part as text. Buffered uploads honour the part's `charset`;
    /// streamed ones decode as UTF-8 (invalid sequences replaced).
    pub async fn text(self) -> Result<String, ApiError> {
        if self.progress.is_none() {
            return self.inner.text().await.map_err(map_error);
        }
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
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
        let boundary = boundary_of(request.headers())?;
        let body = std::mem::take(request.body_mut());
        Ok(Multipart {
            inner: multer::Multipart::new(OnceBody(Some(body)), boundary),
            progress: None,
        })
    }
}

/// One documented form field, for [`App::multipart_fields`].
#[derive(Clone, Debug)]
pub struct MultipartField {
    name: String,
    file: bool,
    required: bool,
    content_type: Option<String>,
    description: Option<String>,
}

impl MultipartField {
    /// A text field.
    pub fn text(name: impl Into<String>) -> Self {
        Self::new(name.into(), false)
    }

    /// A file (binary) field.
    pub fn file(name: impl Into<String>) -> Self {
        Self::new(name.into(), true)
    }

    fn new(name: String, file: bool) -> Self {
        Self {
            name,
            file,
            required: false,
            content_type: None,
            description: None,
        }
    }

    /// The part must be present.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// The media type the part is expected to carry (documented under the
    /// operation's `encoding`).
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl<S: Send + Sync + 'static> App<S> {
    /// Documents the form fields of the last registered route in the OpenAPI
    /// document, as a `multipart/form-data` request body (replacing what the
    /// route had). It documents only: it does not validate uploads. Use it
    /// for [`Multipart`] routes and for raw streaming routes, which otherwise
    /// have no `requestBody`. Call it right after registering the route. The
    /// error responses the framework documents for a body route (`400`, `413`,
    /// `415`) are assumed to apply, as they do for the buffered extractor and
    /// [`Multipart::from_stream`].
    ///
    /// # Panics
    ///
    /// Panics if a field name is given twice.
    pub fn multipart_fields(
        &mut self,
        fields: impl IntoIterator<Item = MultipartField>,
    ) -> &mut Self {
        let Some(index) = self.last_route else {
            return self;
        };
        let mut properties = Map::new();
        let mut required = Vec::new();
        let mut encoding = Map::new();
        for field in fields {
            assert!(
                !properties.contains_key(&field.name),
                "multipart field `{}` is documented twice",
                field.name
            );
            let mut schema = Map::new();
            schema.insert("type".to_owned(), json!("string"));
            if field.file {
                schema.insert("format".to_owned(), json!("binary"));
            }
            if let Some(description) = &field.description {
                schema.insert("description".to_owned(), json!(description));
            }
            if let Some(content_type) = &field.content_type {
                encoding.insert(field.name.clone(), json!({ "contentType": content_type }));
            }
            if field.required {
                required.push(json!(field.name));
            }
            properties.insert(field.name, Value::Object(schema));
        }
        let mut schema = Map::new();
        schema.insert("type".to_owned(), json!("object"));
        schema.insert("properties".to_owned(), Value::Object(properties));
        if !required.is_empty() {
            schema.insert("required".to_owned(), Value::Array(required));
        }
        let mut media = Map::new();
        media.insert("schema".to_owned(), Value::Object(schema));
        if !encoding.is_empty() {
            media.insert("encoding".to_owned(), Value::Object(encoding));
        }
        self.metadata[index].operation.request.request_body = Some(json!({
            "required": true,
            "content": { "multipart/form-data": Value::Object(media) }
        }));
        self.invalidate_openapi_cache();
        self
    }
}

impl<S: Send + Sync + 'static> Group<'_, S> {
    /// See [`App::multipart_fields`].
    pub fn multipart_fields(
        &mut self,
        fields: impl IntoIterator<Item = MultipartField>,
    ) -> &mut Self {
        self.app.multipart_fields(fields);
        self
    }
}
