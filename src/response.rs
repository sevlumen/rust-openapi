use crate::*;
use bytes::Bytes;
use futures_core::Stream;
use http_body::{Body, Frame, SizeHint};
use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

/// The fixed or streaming response body used by the Hyper adapter.
pub enum ResponseBody {
    Full(Option<Bytes>),
    Stream(Pin<Box<dyn Stream<Item = Bytes> + Send + 'static>>),
}

impl ResponseBody {
    pub(crate) fn full(bytes: Bytes) -> Self {
        Self::Full(Some(bytes))
    }

    pub(crate) fn stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Bytes> + Send + 'static,
    {
        Self::Stream(Box::pin(stream))
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            Self::Full(body) => Poll::Ready(body.take().map(|bytes| Ok(Frame::data(bytes)))),
            Self::Stream(stream) => match stream.as_mut().poll_next(context) {
                Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Full(body) => body.is_none(),
            Self::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(body) => {
                let mut hint = SizeHint::new();
                let length = body.as_ref().map_or(0, Bytes::len);
                hint.set_exact(length as u64);
                hint
            }
            Self::Stream(_) => SizeHint::default(),
        }
    }
}

/// Problem-details compatible framework error.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub title: String,
    pub detail: String,
    pub(crate) missing: bool,
}

impl ApiError {
    #[cold]
    #[inline(never)]
    pub fn new(status: StatusCode, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status,
            title: title.into(),
            detail: detail.into(),
            missing: false,
        }
    }

    #[cold]
    #[inline(never)]
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "Bad Request", detail)
    }

    #[cold]
    #[inline(never)]
    /// Construct a missing-input error that `Option<T>` extractors convert to
    /// `None`. Custom extractors should use this only when the input is absent;
    /// malformed present input must return [`ApiError::bad_request`] instead.
    pub fn missing(detail: impl Into<String>) -> Self {
        let mut error = Self::bad_request(detail);
        error.missing = true;
        error
    }

    pub(crate) fn is_missing(&self) -> bool {
        self.missing
    }
}

impl IntoResponse for ApiError {
    #[cold]
    #[inline(never)]
    fn into_response(self) -> HttpResponse {
        let body = json!({
            "type": "about:blank",
            "title": self.title,
            "status": self.status.as_u16(),
            "detail": self.detail,
        });
        let info = ErrorInfo::new(self.status, &self.title, &self.detail);
        info.attach(response_json(self.status, body))
    }
}

/// Conversion from handler return values into HTTP responses.
pub trait IntoResponse: Send + 'static {
    fn into_response(self) -> HttpResponse;
}

pub trait ResponseMetadata {
    fn status_code() -> StatusCode {
        StatusCode::OK
    }
    fn response_schema() -> Option<Value> {
        None
    }
    /// Like [`response_schema`](Self::response_schema), but named schemas go
    /// into `registry` and are referenced with `$ref`.
    fn response_schema_with(registry: &mut SchemaRegistry) -> Option<Value> {
        let _ = registry;
        Self::response_schema()
    }
}

impl ResponseMetadata for &'static str {}
impl ResponseMetadata for String {}
impl ResponseMetadata for Bytes {}
impl ResponseMetadata for JsonBytes {}
impl ResponseMetadata for () {
    fn status_code() -> StatusCode {
        StatusCode::NO_CONTENT
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> HttpResponse {
        response_text(StatusCode::OK, Bytes::from_static(self.as_bytes()))
    }
}

impl IntoResponse for String {
    fn into_response(self) -> HttpResponse {
        response_text(StatusCode::OK, Bytes::from(self))
    }
}

impl IntoResponse for Bytes {
    fn into_response(self) -> HttpResponse {
        response_text(StatusCode::OK, self)
    }
}

impl IntoResponse for () {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::CONTENT_LENGTH, "0")
            .body(ResponseBody::full(Bytes::new()))
            .unwrap()
    }
}

/// A pre-serialized JSON response. The bytes are immutable and can be cloned
/// without re-running serde on each request.
#[derive(Clone, Debug)]
pub struct Json<T>(pub T);

impl<T: Serialize + ApiSchema + Send + 'static> IntoResponse for Json<T> {
    fn into_response(self) -> HttpResponse {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => response_json_bytes(StatusCode::OK, Bytes::from(bytes)),
            Err(error) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Serialization Error",
                error.to_string(),
            )
            .into_response(),
        }
    }
}

impl<T: Serialize + ApiSchema + Send + 'static> ResponseMetadata for Json<T> {
    fn response_schema() -> Option<Value> {
        Some(T::schema())
    }
    fn response_schema_with(registry: &mut SchemaRegistry) -> Option<Value> {
        Some(T::schema_with(registry))
    }
}

/// A JSON body serialized once at startup. Cloning this value only clones the
/// immutable `Bytes` handle, so the normal request path does not invoke serde.
#[derive(Clone, Debug)]
pub struct JsonBytes {
    pub bytes: Bytes,
}

impl JsonBytes {
    pub fn new(bytes: Bytes) -> Self {
        Self { bytes }
    }
}

impl IntoResponse for JsonBytes {
    fn into_response(self) -> HttpResponse {
        response_json_bytes(StatusCode::OK, self.bytes)
    }
}

/// A response whose chunks are produced lazily by a `Stream`. Streaming is
/// opt-in; ordinary `Bytes` responses retain their fixed-size body path.
pub struct StreamResponse<S>(pub S);

impl<S> IntoResponse for StreamResponse<S>
where
    S: Stream<Item = Bytes> + Send + 'static,
{
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::OK)
            .body(ResponseBody::stream(self.0))
            .unwrap()
    }
}

impl<S> ResponseMetadata for StreamResponse<S> where S: Stream<Item = Bytes> + Send + 'static {}

/// A JSON response with the conventional `201 Created` status.
#[derive(Clone, Debug)]
pub struct Created<T>(pub T);

impl<T: Serialize + ApiSchema + Send + 'static> IntoResponse for Created<T> {
    fn into_response(self) -> HttpResponse {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => response_json_bytes(StatusCode::CREATED, Bytes::from(bytes)),
            Err(error) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Serialization Error",
                error.to_string(),
            )
            .into_response(),
        }
    }
}

impl<T: Serialize + ApiSchema + Send + 'static> ResponseMetadata for Created<T> {
    fn status_code() -> StatusCode {
        StatusCode::CREATED
    }
    fn response_schema() -> Option<Value> {
        Some(T::schema())
    }
    fn response_schema_with(registry: &mut SchemaRegistry) -> Option<Value> {
        Some(T::schema_with(registry))
    }
}

/// Explicit bodyless response for `204 No Content` handlers.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoContent;

impl IntoResponse for NoContent {
    fn into_response(self) -> HttpResponse {
        ().into_response()
    }
}

impl ResponseMetadata for NoContent {
    fn status_code() -> StatusCode {
        StatusCode::NO_CONTENT
    }
}

/// Explicit bodyless `304 Not Modified` response.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotModified;

impl IntoResponse for NotModified {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::CONTENT_LENGTH, "0")
            .body(ResponseBody::full(Bytes::new()))
            .unwrap()
    }
}

impl ResponseMetadata for NotModified {
    fn status_code() -> StatusCode {
        StatusCode::NOT_MODIFIED
    }
}

impl<T: IntoResponse> IntoResponse for Result<T, ApiError> {
    fn into_response(self) -> HttpResponse {
        match self {
            Ok(value) => value.into_response(),
            Err(error) => error.into_response(),
        }
    }
}

impl<T: ResponseMetadata> ResponseMetadata for Result<T, ApiError> {
    fn status_code() -> StatusCode {
        T::status_code()
    }
    fn response_schema() -> Option<Value> {
        T::response_schema()
    }
    fn response_schema_with(registry: &mut SchemaRegistry) -> Option<Value> {
        T::response_schema_with(registry)
    }
}

pub(crate) fn response_text(status: StatusCode, body: Bytes) -> HttpResponse {
    let length = body.len();
    let mut response = Response::new(ResponseBody::full(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if length == 2 {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("2"));
    } else {
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).unwrap(),
        );
    }
    response
}

pub(crate) fn response_json<T: Serialize>(status: StatusCode, value: T) -> HttpResponse {
    response_json_bytes(
        status,
        Bytes::from(serde_json::to_vec(&value).unwrap_or_default()),
    )
}

pub(crate) fn response_json_bytes(status: StatusCode, body: Bytes) -> HttpResponse {
    let mut response = Response::new(ResponseBody::full(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[cold]
#[inline(never)]
pub(crate) fn payload_too_large_response() -> HttpResponse {
    const DETAIL: &str = "request body exceeds the configured limit";
    ErrorInfo::new(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large", DETAIL).attach(
        response_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({
                "type": "about:blank",
                "title": "Payload Too Large",
                "status": 413,
                "detail": DETAIL
            }),
        ),
    )
}

#[cold]
#[inline(never)]
pub(crate) fn not_found() -> HttpResponse {
    ErrorInfo::new(
        StatusCode::NOT_FOUND,
        "Not Found",
        "the requested resource was not found",
    )
    .attach(response_text(
        StatusCode::NOT_FOUND,
        Bytes::from_static(b"Not Found"),
    ))
}

#[cold]
#[inline(never)]
pub(crate) fn options_response(allow: &str) -> HttpResponse {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::ALLOW, allow)
        .header(header::CONTENT_LENGTH, "0")
        .body(ResponseBody::full(Bytes::new()))
        .unwrap()
}

#[cold]
#[inline(never)]
pub(crate) fn method_not_allowed_response(allow: &str) -> HttpResponse {
    let response = Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(header::ALLOW, allow)
        .header(header::CONTENT_LENGTH, "0")
        .body(ResponseBody::full(Bytes::new()))
        .unwrap();
    ErrorInfo::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "Method Not Allowed",
        "the method is not allowed for this resource",
    )
    .attach(response)
}

pub(crate) fn maybe_head_flag(is_head: bool, response: HttpResponse) -> HttpResponse {
    if !is_head {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    if !parts.headers.contains_key(header::CONTENT_LENGTH)
        && let Some(length) = body.size_hint().exact()
    {
        parts.headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).unwrap(),
        );
    }
    Response::from_parts(parts, ResponseBody::full(Bytes::new()))
}
