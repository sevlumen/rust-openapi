//! `oas-rs`: a small, typed HTTP framework built on Hyper and Tokio.
//!
//! The runtime deliberately keeps OpenAPI generation out of request dispatch. The
//! document is assembled when routes are registered and is only serialized when
//! the explicitly registered OpenAPI endpoint is requested.

use bytes::Bytes;
use futures_core::Stream;
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::Incoming;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    borrow::Cow,
    collections::HashMap,
    convert::Infallible,
    future::{self, Future},
    marker::PhantomPinned,
    mem::{MaybeUninit, align_of, size_of},
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
};

pub use http::Method;
pub use http::Method as HttpMethod;
pub use oas_rs_macros::OpenApi;
pub use serde_json;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
pub type HttpResponse = Response<ResponseBody>;

/// Default upper bound for request bodies collected by body extractors.
pub const DEFAULT_MAX_BODY_SIZE: usize = 1024 * 1024;

pub enum ResponseBody {
    Full(Option<Bytes>),
    Stream(Pin<Box<dyn Stream<Item = Bytes> + Send + 'static>>),
}

impl ResponseBody {
    fn full(bytes: Bytes) -> Self {
        Self::Full(Some(bytes))
    }

    fn stream<S>(stream: S) -> Self
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

const INLINE_FUTURE_SIZE: usize = 64;

#[repr(align(16))]
struct FutureStorage([MaybeUninit<u8>; INLINE_FUTURE_SIZE]);

struct InlineFuture {
    storage: FutureStorage,
    poll_fn: unsafe fn(*mut u8, &mut Context<'_>) -> Poll<HttpResponse>,
    drop_fn: unsafe fn(*mut u8),
    _pin: PhantomPinned,
}

impl InlineFuture {
    fn new<F, R>(future: F) -> Self
    where
        F: Future<Output = R> + Send + 'static,
        R: IntoResponse,
    {
        debug_assert!(size_of::<F>() <= INLINE_FUTURE_SIZE);
        debug_assert!(align_of::<F>() <= align_of::<FutureStorage>());
        let mut storage = FutureStorage([MaybeUninit::uninit(); INLINE_FUTURE_SIZE]);
        unsafe {
            (storage.0.as_mut_ptr() as *mut F).write(future);
        }
        Self {
            storage,
            poll_fn: poll_response::<F, R>,
            drop_fn: drop_inline::<F>,
            _pin: PhantomPinned,
        }
    }
}

impl Drop for InlineFuture {
    fn drop(&mut self) {
        unsafe { (self.drop_fn)(self.storage.0.as_mut_ptr() as *mut u8) };
    }
}

unsafe fn poll_response<F, R>(storage: *mut u8, context: &mut Context<'_>) -> Poll<HttpResponse>
where
    F: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    match unsafe { Pin::new_unchecked(&mut *(storage as *mut F)).poll(context) } {
        Poll::Ready(value) => Poll::Ready(value.into_response()),
        Poll::Pending => Poll::Pending,
    }
}

unsafe fn drop_inline<F>(storage: *mut u8)
where
    F: Send + 'static,
{
    unsafe { std::ptr::drop_in_place(storage as *mut F) };
}

pub struct HandlerFuture(HandlerFutureKind);

enum HandlerFutureKind {
    Inline(InlineFuture),
    Boxed(BoxFuture<HttpResponse>),
}

impl HandlerFuture {
    fn from_response_future<F, R>(future: F) -> Self
    where
        F: Future<Output = R> + Send + 'static,
        R: IntoResponse,
    {
        if size_of::<F>() <= INLINE_FUTURE_SIZE && align_of::<F>() <= align_of::<FutureStorage>() {
            Self(HandlerFutureKind::Inline(InlineFuture::new::<F, R>(future)))
        } else {
            Self(HandlerFutureKind::Boxed(Box::pin(async move {
                future.await.into_response()
            })))
        }
    }
}

impl Future for HandlerFuture {
    type Output = HttpResponse;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            match &mut self.get_unchecked_mut().0 {
                HandlerFutureKind::Inline(future) => {
                    (future.poll_fn)(future.storage.0.as_mut_ptr() as *mut u8, context)
                }
                HandlerFutureKind::Boxed(future) => Pin::new_unchecked(future).poll(context),
            }
        }
    }
}

const MAX_CAPTURE_PARAMS: usize = 8;

#[derive(Clone, Copy, Debug)]
struct CaptureRange {
    start: usize,
    end: usize,
}

/// A path capture collection. Typed extractors use ranges into the request
/// URI, while the explicit `Params` extractor opts into owned decoded values.
#[derive(Clone, Debug)]
pub struct Params {
    packed: [u64; MAX_CAPTURE_PARAMS],
    count: u8,
    owned: Option<Arc<[(String, String)]>>,
}

static EMPTY_PARAMS: Params = Params {
    packed: [0; MAX_CAPTURE_PARAMS],
    count: 0,
    owned: None,
};

impl Params {
    fn empty() -> &'static Self {
        &EMPTY_PARAMS
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.owned
            .as_ref()?
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn first_raw<'a>(&self, path: &'a str) -> Option<&'a str> {
        self.range(0).map(|range| &path[range.start..range.end])
    }

    fn range(&self, index: usize) -> Option<CaptureRange> {
        (index < self.count as usize).then(|| {
            let packed = self.packed[index];
            CaptureRange {
                start: (packed >> 32) as usize,
                end: packed as u32 as usize,
            }
        })
    }

    fn from_match(names: &[String], captures: CaptureSet, path: &str, materialize: bool) -> Self {
        let owned = materialize.then(|| {
            let mut values = Vec::with_capacity(captures.count as usize);
            for (index, name) in names.iter().enumerate() {
                let range = captures.range(index).expect("capture range exists");
                let value = percent_decode(&path[range.start..range.end])
                    .expect("route matching validates percent encoding");
                values.push((name.clone(), value));
            }
            Arc::from(values.into_boxed_slice())
        });
        Self {
            packed: captures.packed,
            count: captures.count,
            owned,
        }
    }
}

impl Default for Params {
    fn default() -> Self {
        Self {
            packed: [0; MAX_CAPTURE_PARAMS],
            count: 0,
            owned: None,
        }
    }
}

impl<S: Send + Sync + 'static> FromRequest<S> for Params {
    const NEEDS_PARAMS: bool = true;

    fn from_request(
        _request: &mut Request<Bytes>,
        params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        Ok(params.clone())
    }
}

/// Typed state extractor.
#[derive(Clone, Debug)]
pub struct State<T>(pub Arc<T>);

/// Typed path extractor. The first form is convenient for a one-capture route;
/// named multi-capture extraction is available through [`Params`].
#[derive(Clone, Debug)]
pub struct Path<T>(pub T);

/// Typed query extractor backed by serde.
#[derive(Clone, Debug)]
pub struct Query<T>(pub T);

/// Typed header extractor. Implement [`HeaderSpec`] for application-specific
/// header types to keep header names resolved once at startup.
#[derive(Clone, Debug)]
pub struct Header<T>(pub T);

pub trait HeaderSpec: Sized + Send + 'static {
    const NAME: &'static str;
    fn parse(value: &str) -> Result<Self, ApiError>;
}

/// The small set of type schemas that can be inferred without runtime
/// reflection. Applications can implement this trait for their own scalar
/// path types.
pub trait OpenApiType {
    fn schema() -> Value;
}

pub trait OpenApiSchema {
    fn schema() -> Value;
}

pub trait OpenApiQuery: Sized {
    fn parameters() -> Vec<Value>;

    fn parse(query: &str) -> Result<Self, ApiError>
    where
        Self: DeserializeOwned,
    {
        parse_query(query)
    }
}

pub trait QueryValue: Sized {
    fn parse_query_value(value: &str) -> Result<Self, ApiError>;
}

impl<T> QueryValue for T
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    fn parse_query_value(value: &str) -> Result<Self, ApiError> {
        value
            .parse::<Self>()
            .map_err(|error| ApiError::bad_request(error.to_string()))
    }
}

pub fn parse_query_value<T: QueryValue>(value: &str) -> Result<T, ApiError> {
    T::parse_query_value(value)
}

impl OpenApiType for String {
    fn schema() -> Value {
        json!({ "type": "string" })
    }
}

impl OpenApiSchema for String {
    fn schema() -> Value {
        <Self as OpenApiType>::schema()
    }
}

impl OpenApiType for u32 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int32" })
    }
}

impl OpenApiSchema for u32 {
    fn schema() -> Value {
        <Self as OpenApiType>::schema()
    }
}

impl OpenApiType for u64 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int64" })
    }
}

impl OpenApiSchema for u64 {
    fn schema() -> Value {
        <Self as OpenApiType>::schema()
    }
}

impl OpenApiType for i32 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int32" })
    }
}

impl OpenApiSchema for i32 {
    fn schema() -> Value {
        <Self as OpenApiType>::schema()
    }
}

impl OpenApiType for i64 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int64" })
    }
}

impl OpenApiSchema for i64 {
    fn schema() -> Value {
        <Self as OpenApiType>::schema()
    }
}

impl OpenApiType for bool {
    fn schema() -> Value {
        json!({ "type": "boolean" })
    }
}

impl OpenApiSchema for bool {
    fn schema() -> Value {
        <Self as OpenApiType>::schema()
    }
}

impl OpenApiType for uuid::Uuid {
    fn schema() -> Value {
        json!({ "type": "string", "format": "uuid" })
    }
}

impl OpenApiSchema for uuid::Uuid {
    fn schema() -> Value {
        <Self as OpenApiType>::schema()
    }
}

impl<T: OpenApiSchema> OpenApiSchema for Option<T> {
    fn schema() -> Value {
        T::schema()
    }
}

impl<T: OpenApiSchema> OpenApiSchema for Vec<T> {
    fn schema() -> Value {
        json!({ "type": "array", "items": T::schema() })
    }
}

impl<T: OpenApiSchema + ?Sized> OpenApiSchema for &T {
    fn schema() -> Value {
        T::schema()
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenApiRequest {
    path_schemas: Vec<Value>,
    parameters: Vec<Value>,
    request_body: Option<Value>,
}

impl OpenApiRequest {
    fn merge(&mut self, mut other: Self) {
        self.path_schemas.append(&mut other.path_schemas);
        self.parameters.append(&mut other.parameters);
        if self.request_body.is_none() {
            self.request_body = other.request_body;
        }
    }
}

/// Problem-details compatible framework error.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub title: String,
    pub detail: String,
    missing: bool,
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
    fn missing(detail: impl Into<String>) -> Self {
        let mut error = Self::bad_request(detail);
        error.missing = true;
        error
    }

    fn is_missing(&self) -> bool {
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
        response_json(self.status, body)
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

impl<T: Serialize + OpenApiSchema + Send + 'static> IntoResponse for Json<T> {
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

impl<T: Serialize + OpenApiSchema + Send + 'static> ResponseMetadata for Json<T> {
    fn response_schema() -> Option<Value> {
        Some(T::schema())
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

impl<T: Serialize + OpenApiSchema + Send + 'static> IntoResponse for Created<T> {
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

impl<T: Serialize + OpenApiSchema + Send + 'static> ResponseMetadata for Created<T> {
    fn status_code() -> StatusCode {
        StatusCode::CREATED
    }
    fn response_schema() -> Option<Value> {
        Some(T::schema())
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
}

/// A handler implementation is monomorphized at registration time and stored
/// as a single erased service only at the router boundary.
pub trait Handler<S, Args>: Send + Sync + 'static {
    type Response: ResponseMetadata;
    const NEEDS_PARAMS: bool = false;
    const NEEDS_BODY: bool = false;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn call(&self, request: &mut Request<Bytes>, params: &Params, state: &Arc<S>) -> HandlerFuture;

    fn zero_handler(&self) -> Option<ErasedZeroHandler> {
        None
    }
}

/// An escape hatch for handlers that need Hyper's original streaming request
/// body. Unlike typed extractors, a raw handler receives `Incoming` without
/// the framework collecting it first.
pub trait RawHandler<S>: Send + Sync + 'static {
    type Response: ResponseMetadata;

    fn call(&self, request: Request<Incoming>) -> HandlerFuture;
}

impl<S, F, Fut, R> RawHandler<S> for F
where
    S: Send + Sync + 'static,
    F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
{
    type Response = R;

    fn call(&self, request: Request<Incoming>) -> HandlerFuture {
        let future = (self)(request);
        HandlerFuture::from_response_future(future)
    }
}

pub trait FromRequest<S>: Sized + Send + 'static {
    const NEEDS_PARAMS: bool = false;
    const NEEDS_BODY: bool = false;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn from_request(
        request: &mut Request<Bytes>,
        params: &Params,
        state: &Arc<S>,
    ) -> Result<Self, ApiError>;
}

impl<S: Send + Sync + 'static> FromRequest<S> for State<S> {
    fn from_request(
        _request: &mut Request<Bytes>,
        _params: &Params,
        state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        Ok(State(Arc::clone(state)))
    }
}

impl<S: Send + Sync + 'static, T> FromRequest<S> for Path<T>
where
    T: FromStr + OpenApiSchema + Send + 'static,
    T::Err: std::fmt::Display,
{
    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest {
            path_schemas: vec![<T as OpenApiSchema>::schema()],
            ..OpenApiRequest::default()
        }
    }

    fn from_request(
        request: &mut Request<Bytes>,
        params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        params
            .first_raw(request.uri().path())
            .ok_or_else(|| ApiError::bad_request("missing path parameter"))
            .and_then(|value| {
                let value = if value.as_bytes().contains(&b'%') {
                    Cow::Owned(percent_decode(value)?)
                } else {
                    Cow::Borrowed(value)
                };
                value
                    .parse::<T>()
                    .map(Path)
                    .map_err(|error| ApiError::bad_request(error.to_string()))
            })
    }
}

impl<S: Send + Sync + 'static, T> FromRequest<S> for Query<T>
where
    T: DeserializeOwned + OpenApiQuery + Send + 'static,
{
    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest {
            parameters: T::parameters(),
            ..OpenApiRequest::default()
        }
    }

    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        T::parse(request.uri().query().unwrap_or_default()).map(Query)
    }
}

impl<S: Send + Sync + 'static, T> FromRequest<S> for Json<T>
where
    T: DeserializeOwned + OpenApiSchema + Send + 'static,
{
    const NEEDS_BODY: bool = true;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest {
            request_body: Some(json!({
                "required": true,
                "content": {
                    "application/json": {
                        "schema": T::schema()
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
        let body = request.body().clone();
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if body.is_empty() && content_type.is_empty() {
            return Err(ApiError::missing("missing JSON body"));
        }
        let media_type = content_type
            .split(';')
            .next()
            .map(str::trim)
            .unwrap_or_default();
        if !media_type.eq_ignore_ascii_case("application/json") {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Media Type",
                "expected application/json",
            ));
        }
        serde_json::from_slice(&body)
            .map(Json)
            .map_err(|error| ApiError::bad_request(error.to_string()))
    }
}

impl<S: Send + Sync + 'static, T: HeaderSpec> FromRequest<S> for Header<T> {
    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest {
            parameters: vec![json!({
                "in": "header",
                "name": T::NAME,
                "required": true,
                "schema": { "type": "string" }
            })],
            ..OpenApiRequest::default()
        }
    }

    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        let value = request
            .headers()
            .get(T::NAME)
            .ok_or_else(|| ApiError::missing(format!("missing header {}", T::NAME)))?;
        let value = value
            .to_str()
            .map_err(|_| ApiError::bad_request(format!("invalid header {}", T::NAME)))?;
        T::parse(value).map(Header)
    }
}

impl<S, T> FromRequest<S> for Option<T>
where
    S: Send + Sync + 'static,
    T: FromRequest<S>,
{
    const NEEDS_PARAMS: bool = T::NEEDS_PARAMS;
    const NEEDS_BODY: bool = T::NEEDS_BODY;

    fn openapi_request() -> OpenApiRequest {
        let mut metadata = T::openapi_request();
        for parameter in &mut metadata.parameters {
            if let Some(object) = parameter.as_object_mut() {
                object.insert("required".to_owned(), Value::Bool(false));
            }
        }
        if let Some(request_body) = metadata.request_body.as_mut()
            && let Some(object) = request_body.as_object_mut()
        {
            object.insert("required".to_owned(), Value::Bool(false));
        }
        metadata
    }

    fn from_request(
        request: &mut Request<Bytes>,
        params: &Params,
        state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        match T::from_request(request, params, state) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.is_missing() => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl<S, F, Fut, R> Handler<S, ()> for F
where
    S: Send + Sync + 'static,
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
{
    type Response = R;
    const NEEDS_PARAMS: bool = false;
    const NEEDS_BODY: bool = false;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn call(
        &self,
        _request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> HandlerFuture {
        let future = (self)();
        HandlerFuture::from_response_future(future)
    }

    fn zero_handler(&self) -> Option<ErasedZeroHandler> {
        let handler = self.clone();
        Some(Box::new(move || {
            let future = (handler)();
            HandlerFuture::from_response_future(future)
        }))
    }
}

impl<S, F, Fut, R, E1> Handler<S, (E1,)> for F
where
    S: Send + Sync + 'static,
    F: Fn(E1) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
    E1: FromRequest<S>,
{
    type Response = R;
    const NEEDS_PARAMS: bool = E1::NEEDS_PARAMS;
    const NEEDS_BODY: bool = E1::NEEDS_BODY;

    fn openapi_request() -> OpenApiRequest {
        E1::openapi_request()
    }

    fn call(&self, request: &mut Request<Bytes>, params: &Params, state: &Arc<S>) -> HandlerFuture {
        let value = match E1::from_request(request, params, state) {
            Ok(value) => value,
            Err(error) => return HandlerFuture::from_response_future(future::ready(error)),
        };
        HandlerFuture::from_response_future((self)(value))
    }
}

macro_rules! impl_extractor_handler {
    (
        $first:ident : $first_arg:ident
        $(, $rest:ident : $rest_arg:ident)+ $(,)?
    ) => {
        impl<S, F, Fut, R, $first $(, $rest)*> Handler<S, ($first $(, $rest)*,)> for F
        where
            S: Send + Sync + 'static,
            F: Fn($first $(, $rest)*) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = R> + Send + 'static,
            R: IntoResponse + ResponseMetadata,
            $first: FromRequest<S>,
            $($rest: FromRequest<S>,)*
        {
            type Response = R;
            const NEEDS_PARAMS: bool =
                <$first as FromRequest<S>>::NEEDS_PARAMS
                $(|| <$rest as FromRequest<S>>::NEEDS_PARAMS)*;
            const NEEDS_BODY: bool =
                <$first as FromRequest<S>>::NEEDS_BODY
                $(|| <$rest as FromRequest<S>>::NEEDS_BODY)*;

            fn openapi_request() -> OpenApiRequest {
                let mut metadata = <$first as FromRequest<S>>::openapi_request();
                $(metadata.merge(<$rest as FromRequest<S>>::openapi_request());)*
                metadata
            }

            fn call(
                &self,
                request: &mut Request<Bytes>,
                params: &Params,
                state: &Arc<S>,
            ) -> HandlerFuture {
                let $first_arg = match <$first as FromRequest<S>>::from_request(
                    request, params, state,
                ) {
                    Ok(value) => value,
                    Err(error) => return HandlerFuture::from_response_future(future::ready(error)),
                };
                $(
                    let $rest_arg = match <$rest as FromRequest<S>>::from_request(
                        request, params, state,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            return HandlerFuture::from_response_future(future::ready(error));
                        }
                    };
                )*
                let future = (self)($first_arg $(, $rest_arg)*);
                HandlerFuture::from_response_future(future)
            }
        }
    };
}

impl_extractor_handler!(E1: first, E2: second);
impl_extractor_handler!(E1: first, E2: second, E3: third);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth, E6: sixth);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth, E6: sixth, E7: seventh);
impl_extractor_handler!(E1: first, E2: second, E3: third, E4: fourth, E5: fifth, E6: sixth, E7: seventh, E8: eighth);

pub type ErasedZeroHandler = Box<dyn Fn() -> HandlerFuture + Send + Sync>;
type ErasedHandler<S> =
    Box<dyn Fn(&mut Request<Bytes>, &Params, &Arc<S>) -> HandlerFuture + Send + Sync>;
type ErasedRawHandler = Box<dyn Fn(Request<Incoming>) -> HandlerFuture + Send + Sync>;

enum HandlerKind<S> {
    Zero(ErasedZeroHandler),
    Typed(ErasedHandler<S>),
    Raw(ErasedRawHandler),
    // Static payloads are cold registration data; keep them out of every
    // route plan's inline layout while retaining a zero-allocation request
    // path for the static response itself.
    Static(Box<StaticResponse>),
    Builtin(BuiltinHandler),
}

trait CaptureProvider: Copy {
    fn invoke<S: Send + Sync + 'static>(
        self,
        mode: &CaptureMode,
        request: &mut Request<Bytes>,
        metadata: &RouteMetadata,
        handler: &ErasedHandler<S>,
        state: &Arc<S>,
    ) -> HandlerFuture;
}

#[derive(Clone, Copy)]
struct StaticCaptures;

impl CaptureProvider for StaticCaptures {
    fn invoke<S: Send + Sync + 'static>(
        self,
        mode: &CaptureMode,
        request: &mut Request<Bytes>,
        _metadata: &RouteMetadata,
        handler: &ErasedHandler<S>,
        state: &Arc<S>,
    ) -> HandlerFuture {
        debug_assert!(matches!(mode, CaptureMode::None));
        handler(request, Params::empty(), state)
    }
}

#[derive(Clone, Copy)]
struct DynamicCaptures(CaptureSet);

impl CaptureProvider for DynamicCaptures {
    fn invoke<S: Send + Sync + 'static>(
        self,
        mode: &CaptureMode,
        request: &mut Request<Bytes>,
        metadata: &RouteMetadata,
        handler: &ErasedHandler<S>,
        state: &Arc<S>,
    ) -> HandlerFuture {
        let path = normalize_request_path(request.uri().path());
        let params = match mode {
            CaptureMode::None => Params::empty().clone(),
            CaptureMode::Borrowed => Params::from_match(&[], self.0, path, false),
            CaptureMode::Materialized => {
                let names = metadata
                    .capture_names
                    .as_deref()
                    .expect("materialized captures have names");
                Params::from_match(names, self.0, path, true)
            }
        };
        handler(request, &params, state)
    }
}

enum StaticResponse {
    Text {
        body: Bytes,
        content_length: HeaderValue,
    },
    Json {
        body: Bytes,
    },
}

impl StaticResponse {
    fn text(body: &'static str) -> Self {
        Self::Text {
            body: Bytes::from_static(body.as_bytes()),
            content_length: HeaderValue::from_str(&body.len().to_string())
                .expect("static text length is a valid header value"),
        }
    }

    fn json(body: Bytes) -> Self {
        Self::Json { body }
    }

    fn to_response(&self) -> HttpResponse {
        let mut response = Response::new(ResponseBody::full(match self {
            Self::Text { body, .. } | Self::Json { body } => body.clone(),
        }));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, self.content_type());
        if let Self::Text { content_length, .. } = self {
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, content_length.clone());
        }
        response
    }

    fn content_type(&self) -> HeaderValue {
        match self {
            Self::Text { .. } => HeaderValue::from_static("text/plain; charset=utf-8"),
            Self::Json { .. } => HeaderValue::from_static("application/json"),
        }
    }
}

#[derive(Clone, Copy)]
enum BuiltinHandler {
    OpenApi,
    Swagger,
}

struct RoutePlan<S> {
    capture_mode: CaptureMode,
    body_mode: BodyMode,
    handler: HandlerKind<S>,
}

enum CaptureMode {
    None,
    Borrowed,
    Materialized,
}

#[repr(u8)]
#[derive(Clone, Copy)]
enum BodyMode {
    None,
    Buffered,
    Incoming,
}

struct RouteMetadata {
    builtin: bool,
    method: Method,
    template: String,
    segments: Vec<Segment>,
    capture_names: Option<Arc<[String]>>,
    operation: Operation,
}

#[derive(Clone)]
enum Segment {
    Static(String),
    Capture(String),
}

struct DynamicRouteTrie {
    nodes: Vec<DynamicRouteNode>,
}

#[derive(Default)]
struct DynamicRouteNode {
    static_children: HashMap<String, NodeId>,
    capture_child: Option<NodeId>,
    routes: RouteSet,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodeId(u32);

impl NodeId {
    fn new(index: usize) -> Self {
        assert!(
            index <= u32::MAX as usize,
            "dynamic trie exceeds u32::MAX nodes"
        );
        Self(index as u32)
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RouteId(u32);

impl RouteId {
    const NONE: Self = Self(u32::MAX);

    fn new(index: usize) -> Self {
        assert!(index <= u32::MAX as usize, "route count exceeds u32::MAX");
        Self(index as u32)
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

struct RouteSet {
    routes: [RouteId; 7],
    method_mask: u8,
}

impl Default for RouteSet {
    fn default() -> Self {
        Self {
            routes: [RouteId::NONE; 7],
            method_mask: 0,
        }
    }
}

impl RouteSet {
    fn insert(&mut self, method: Method, route: usize) {
        let slot = method_slot(&method).expect("unsupported route method");
        let route = RouteId::new(route);
        assert_eq!(
            self.routes[slot],
            RouteId::NONE,
            "duplicate route pattern and method"
        );
        self.routes[slot] = route;
        self.method_mask |= 1_u8 << slot;
    }

    fn route(&self, method: &Method) -> Option<RouteId> {
        method_slot(method)
            .and_then(|slot| (self.routes[slot] != RouteId::NONE).then_some(self.routes[slot]))
    }

    #[cold]
    #[inline(never)]
    fn allowed_methods(&self) -> String {
        let mut allowed = String::with_capacity(48);
        for (index, name) in METHOD_NAMES.iter().enumerate() {
            if self.method_mask & (1_u8 << index) == 0 {
                continue;
            }
            if !allowed.is_empty() {
                allowed.push_str(", ");
            }
            allowed.push_str(name);
        }
        allowed
    }

    fn remove(&mut self, method: &Method) {
        if let Some(slot) = method_slot(method) {
            self.routes[slot] = RouteId::NONE;
            self.method_mask &= !(1_u8 << slot);
        }
    }

    fn is_empty(&self) -> bool {
        self.method_mask == 0
    }
}

const METHOD_NAMES: [&str; 7] = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];

fn method_slot(method: &Method) -> Option<usize> {
    match method.as_str() {
        "DELETE" => Some(0),
        "GET" => Some(1),
        "HEAD" => Some(2),
        "OPTIONS" => Some(3),
        "PATCH" => Some(4),
        "POST" => Some(5),
        "PUT" => Some(6),
        _ => None,
    }
}

struct DynamicPathMatch<'a> {
    routes: &'a RouteSet,
    captures: CaptureSet,
}

#[derive(Clone, Copy, Debug, Default)]
struct CaptureSet {
    packed: [u64; MAX_CAPTURE_PARAMS],
    count: u8,
}

impl CaptureSet {
    fn range(self, index: usize) -> Option<CaptureRange> {
        (index < self.count as usize).then(|| {
            let packed = self.packed[index];
            CaptureRange {
                start: (packed >> 32) as usize,
                end: packed as u32 as usize,
            }
        })
    }

    fn with_capture(mut self, index: usize, range: CaptureRange) -> Self {
        debug_assert!(range.start <= u32::MAX as usize);
        debug_assert!(range.end <= u32::MAX as usize);
        self.packed[index] = ((range.start as u32 as u64) << 32) | range.end as u32 as u64;
        self.count = (index + 1) as u8;
        self
    }
}

#[derive(Debug)]
enum RouteFailure {
    Options(String),
    MethodNotAllowed(String),
}

fn route_index(routes: &RouteSet, method: &Method) -> Option<RouteId> {
    routes.route(method).or_else(|| {
        (method == Method::HEAD)
            .then(|| routes.route(&Method::GET))
            .flatten()
    })
}

fn resolve_route_set(method: &Method, routes: &RouteSet) -> Result<RouteId, RouteFailure> {
    let index = route_index(routes, method);
    if index.is_none() && *method == Method::OPTIONS {
        return Err(RouteFailure::Options(routes.allowed_methods()));
    }
    let Some(index) = index else {
        return Err(RouteFailure::MethodNotAllowed(routes.allowed_methods()));
    };
    Ok(index)
}

impl Default for DynamicRouteTrie {
    fn default() -> Self {
        Self {
            nodes: vec![DynamicRouteNode::default()],
        }
    }
}

impl DynamicRouteTrie {
    #[cfg(test)]
    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn insert(&mut self, segments: &[Segment], method: Method, route: usize) {
        let mut node_index = 0;
        for segment in segments {
            let next = match segment {
                Segment::Static(value) => {
                    if let Some(index) = self.nodes[node_index].static_children.get(value) {
                        *index
                    } else {
                        let index = NodeId::new(self.nodes.len());
                        self.nodes.push(DynamicRouteNode::default());
                        self.nodes[node_index]
                            .static_children
                            .insert(value.clone(), index);
                        index
                    }
                }
                Segment::Capture(_) => {
                    if let Some(index) = self.nodes[node_index].capture_child {
                        index
                    } else {
                        let index = NodeId::new(self.nodes.len());
                        self.nodes.push(DynamicRouteNode::default());
                        self.nodes[node_index].capture_child = Some(index);
                        index
                    }
                }
            };
            node_index = next.index();
        }
        self.nodes[node_index].routes.insert(method, route);
    }

    fn find(&self, path: &str) -> Option<DynamicPathMatch<'_>> {
        let (node_index, captures) =
            self.find_node(0, PathParts::new(path), CaptureSet::default())?;
        let routes = &self.nodes[node_index].routes;
        Some(DynamicPathMatch { routes, captures })
    }

    fn find_node(
        &self,
        node_index: usize,
        mut parts: PathParts<'_>,
        captures: CaptureSet,
    ) -> Option<(usize, CaptureSet)> {
        let node = &self.nodes[node_index];
        let Some(part) = parts.next() else {
            return (!node.routes.is_empty()).then_some((node_index, captures));
        };

        // Static branches have precedence over captures, but the capture
        // branch remains available if the static branch fails deeper down.
        if let Some(&child) = node.static_children.get(part.value)
            && let Some(found) = self.find_node(child.index(), parts, captures)
        {
            return Some(found);
        }

        if (captures.count as usize) < MAX_CAPTURE_PARAMS
            && valid_percent_encoding(part.value)
            && let Some(child) = node.capture_child
        {
            let captured = captures.with_capture(
                captures.count as usize,
                CaptureRange {
                    start: part.start,
                    end: part.end,
                },
            );
            if let Some(found) = self.find_node(child.index(), parts, captured) {
                return Some(found);
            }
        }
        None
    }
}

#[derive(Clone, Default)]
struct Operation {
    tag: Option<String>,
    summary: Option<String>,
    operation_id: Option<String>,
    response_status: StatusCode,
    response_schema: Option<Value>,
    request: OpenApiRequest,
}

/// Mutable route-registration builder.
pub struct AppBuilder<S = ()> {
    state: Arc<S>,
    plans: Vec<RoutePlan<S>>,
    metadata: Vec<RouteMetadata>,
    static_routes: HashMap<String, RouteSet>,
    dynamic_routes: DynamicRouteTrie,
    last_route: Option<usize>,
    openapi_path: Option<String>,
    openapi_bytes: Option<Bytes>,
    swagger_path: Option<String>,
    openapi_route: Option<usize>,
    swagger_route: Option<usize>,
    title: String,
    version: String,
}

/// Backwards-compatible alias for [`AppBuilder`].
pub type App<S = ()> = AppBuilder<S>;

/// An immutable application runtime produced by [`AppBuilder::build`].
pub struct AppRuntime<S = ()> {
    state: Arc<S>,
    plans: Box<[RoutePlan<S>]>,
    metadata: Box<[RouteMetadata]>,
    static_routes: HashMap<String, RouteSet>,
    dynamic_routes: DynamicRouteTrie,
    openapi_path: Option<String>,
    openapi_bytes: Option<Bytes>,
    title: String,
    version: String,
}

impl AppBuilder<()> {
    pub fn new() -> Self {
        Self {
            state: Arc::new(()),
            plans: Vec::new(),
            metadata: Vec::new(),
            static_routes: HashMap::new(),
            dynamic_routes: DynamicRouteTrie::default(),
            last_route: None,
            openapi_path: None,
            openapi_bytes: None,
            swagger_path: None,
            openapi_route: None,
            swagger_route: None,
            title: "oas-rs API".to_owned(),
            version: "0.1.0".to_owned(),
        }
    }

    pub fn with_state<T: Send + Sync + 'static>(self, state: T) -> AppBuilder<T> {
        assert!(
            self.plans.is_empty(),
            "with_state must be configured before routes"
        );
        AppBuilder {
            state: Arc::new(state),
            plans: Vec::new(),
            metadata: Vec::new(),
            static_routes: HashMap::new(),
            dynamic_routes: DynamicRouteTrie::default(),
            last_route: None,
            openapi_path: self.openapi_path,
            openapi_bytes: self.openapi_bytes,
            swagger_path: self.swagger_path,
            openapi_route: self.openapi_route,
            swagger_route: self.swagger_route,
            title: self.title,
            version: self.version,
        }
    }
}

impl Default for AppBuilder<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Send + Sync + 'static> AppBuilder<S> {
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self.openapi_bytes = None;
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self.openapi_bytes = None;
        self
    }

    /// Freeze route registration into an immutable runtime.
    ///
    /// The returned runtime exposes serving and request execution only; route
    /// registration methods remain available on the builder type.
    pub fn build(mut self) -> AppRuntime<S> {
        self.prepare_openapi();
        AppRuntime {
            state: self.state,
            plans: self.plans.into_boxed_slice(),
            metadata: self.metadata.into_boxed_slice(),
            static_routes: self.static_routes,
            dynamic_routes: self.dynamic_routes,
            openapi_path: self.openapi_path,
            openapi_bytes: self.openapi_bytes,
            title: self.title,
            version: self.version,
        }
    }

    pub fn get<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::GET, path, handler);
        self
    }

    /// Register a GET route whose text response is prepared at startup.
    ///
    /// This is the lowest-overhead route for health checks, readiness probes,
    /// and other immutable text responses. It does not create or poll a
    /// handler future per request.
    pub fn static_text(&mut self, path: &str, body: &'static str) -> &mut Self {
        self.add_static_route(path, StaticResponse::text(body));
        self
    }

    /// Register a GET route whose JSON bytes are prepared by the application.
    ///
    /// The bytes are reference-counted and cloned by handle, so serialization
    /// is not repeated during request dispatch.
    pub fn static_json(&mut self, path: &str, body: Bytes) -> &mut Self {
        self.add_static_route(path, StaticResponse::json(body));
        self
    }

    pub fn head<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::HEAD, path, handler);
        self
    }

    pub fn post<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::POST, path, handler);
        self
    }

    pub fn put<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::PUT, path, handler);
        self
    }

    pub fn patch<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::PATCH, path, handler);
        self
    }

    pub fn delete<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::DELETE, path, handler);
        self
    }

    pub fn options<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::OPTIONS, path, handler);
        self
    }

    /// Register a handler that receives Hyper's streaming `Incoming` body
    /// directly. The framework does not collect or size-limit this body; the
    /// handler owns cancellation and any upload limits.
    pub fn raw<H>(&mut self, method: Method, path: &str, handler: H) -> &mut Self
    where
        H: RawHandler<S>,
    {
        self.add_raw_route(method, path, handler);
        self
    }

    /// Register a GET handler that receives Hyper's streaming `Incoming`
    /// body directly.
    pub fn raw_get<H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: RawHandler<S>,
    {
        self.raw(Method::GET, path, handler)
    }

    pub fn tag(&mut self, tag: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.metadata[index].operation.tag = Some(tag.into());
            self.openapi_bytes = None;
        }
        self
    }

    pub fn summary(&mut self, summary: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.metadata[index].operation.summary = Some(summary.into());
            self.openapi_bytes = None;
        }
        self
    }

    pub fn operation_id(&mut self, operation_id: impl Into<String>) -> &mut Self {
        let operation_id = operation_id.into();
        assert!(
            self.metadata.iter().all(|metadata| {
                metadata.operation.operation_id.as_deref() != Some(operation_id.as_str())
            }),
            "duplicate operation id"
        );
        if let Some(index) = self.last_route {
            self.metadata[index].operation.operation_id = Some(operation_id);
            self.openapi_bytes = None;
        }
        self
    }

    pub fn openapi(&mut self, path: &str) -> &mut Self {
        let path = normalize_path(path);
        self.openapi_path = Some(path.clone());
        self.install_builtin_route(path, BuiltinHandler::OpenApi, true);
        self.openapi_bytes = None;
        self
    }

    pub fn swagger(&mut self, path: &str) -> &mut Self {
        let path = normalize_path(path);
        self.swagger_path = Some(path.clone());
        self.install_builtin_route(path, BuiltinHandler::Swagger, false);
        self
    }

    pub fn openapi_document(&self) -> Value {
        let mut paths = Map::new();
        for metadata in self.metadata.iter() {
            if metadata.builtin {
                continue;
            }
            let method = metadata.method.as_str().to_ascii_lowercase();
            let mut operation = Map::new();
            if let Some(tag) = &metadata.operation.tag {
                operation.insert("tags".to_owned(), json!([tag]));
            }
            if let Some(summary) = &metadata.operation.summary {
                operation.insert("summary".to_owned(), json!(summary));
            }
            if let Some(operation_id) = &metadata.operation.operation_id {
                operation.insert("operationId".to_owned(), json!(operation_id));
            }
            let mut parameters = metadata.operation.request.parameters.clone();
            let mut path_schema_index = 0;
            parameters.extend(
                metadata
                    .segments
                    .iter()
                    .filter_map(|segment| match segment {
                        Segment::Capture(name) => {
                            let schema = metadata
                                .operation
                                .request
                                .path_schemas
                                .get(path_schema_index)
                                .cloned()
                                .unwrap_or_else(|| json!({ "type": "string" }));
                            path_schema_index += 1;
                            Some(json!({
                                "in": "path",
                                "name": name,
                                "required": true,
                                "schema": schema
                            }))
                        }
                        Segment::Static(_) => None,
                    })
                    .collect::<Vec<_>>(),
            );
            if !parameters.is_empty() {
                operation.insert("parameters".to_owned(), Value::Array(parameters));
            }
            if let Some(request_body) = &metadata.operation.request.request_body {
                operation.insert("requestBody".to_owned(), request_body.clone());
            }
            let status = metadata.operation.response_status.as_u16().to_string();
            let mut response = Map::new();
            response.insert(
                "description".to_owned(),
                Value::String(
                    match metadata.operation.response_status {
                        StatusCode::NO_CONTENT => "No Content",
                        StatusCode::NOT_MODIFIED => "Not Modified",
                        StatusCode::CREATED => "Created",
                        _ => "Success",
                    }
                    .to_owned(),
                ),
            );
            if let Some(schema) = &metadata.operation.response_schema {
                response.insert(
                    "content".to_owned(),
                    json!({ "application/json": { "schema": schema } }),
                );
            }
            operation.insert("responses".to_owned(), json!({ status: response }));
            paths
                .entry(metadata.template.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            paths
                .get_mut(&metadata.template)
                .and_then(Value::as_object_mut)
                .unwrap()
                .insert(method, Value::Object(operation));
        }
        json!({
            "openapi": "3.1.0",
            "info": { "title": self.title, "version": self.version },
            "paths": paths,
        })
    }

    pub async fn oneshot(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Bytes>,
    ) -> TestResponse {
        self.oneshot_inner(method, uri, headers, body).await
    }

    async fn oneshot_inner(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Bytes>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            if !name.is_empty() {
                builder = builder.header(*name, *value);
            }
        }
        let request = builder
            .body(body.unwrap_or_default())
            .expect("valid test request");
        TestResponse {
            response: Some(self.handle(request).await),
        }
    }

    pub async fn listen(
        self,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.build().listen(address).await
    }

    /// Serve an already-bound listener until `shutdown` resolves. This is
    /// useful for graceful shutdown and deterministic integration tests.
    pub async fn serve_listener<F>(
        self,
        listener: tokio::net::TcpListener,
        shutdown: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.build().serve_listener(listener, shutdown).await
    }

    fn add_static_route(&mut self, path: &str, response: StaticResponse) {
        let template = normalize_path(path);
        let segments = parse_template(&template);
        assert!(
            segments
                .iter()
                .all(|segment| matches!(segment, Segment::Static(_))),
            "static response routes cannot contain captures"
        );
        assert!(
            self.metadata
                .iter()
                .all(|metadata| metadata.method != Method::GET || metadata.template != template),
            "duplicate route"
        );

        let index = self.plans.len();
        self.static_routes
            .entry(template.clone())
            .or_default()
            .insert(Method::GET, index);
        self.plans.push(RoutePlan {
            capture_mode: CaptureMode::None,
            body_mode: BodyMode::None,
            handler: HandlerKind::Static(Box::new(response)),
        });
        self.metadata.push(RouteMetadata {
            builtin: false,
            method: Method::GET,
            template,
            segments,
            capture_names: None,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                response_status: StatusCode::OK,
                response_schema: None,
                request: OpenApiRequest::default(),
            },
        });
        self.openapi_bytes = None;
        self.last_route = Some(index);
    }

    fn add_route<H, A>(&mut self, method: Method, path: &str, handler: H)
    where
        H: Clone + Handler<S, A>,
    {
        let template = normalize_path(path);
        assert!(
            self.metadata
                .iter()
                .all(|metadata| metadata.method != method || metadata.template != template),
            "duplicate route"
        );
        let segments = parse_template(&template);
        let capture_names = if H::NEEDS_PARAMS {
            Some(
                segments
                    .iter()
                    .filter_map(|segment| match segment {
                        Segment::Capture(name) => Some(name.clone()),
                        Segment::Static(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .into(),
            )
        } else {
            None
        };
        let capture_mode = if capture_names.is_some() {
            CaptureMode::Materialized
        } else if segments
            .iter()
            .any(|segment| matches!(segment, Segment::Capture(_)))
        {
            CaptureMode::Borrowed
        } else {
            CaptureMode::None
        };
        let handler = match handler.zero_handler() {
            Some(zero_handler) => HandlerKind::Zero(zero_handler),
            None => HandlerKind::Typed(Box::new(move |request, params, state| {
                handler.call(request, params, state)
            })),
        };
        let index = self.plans.len();
        if segments
            .iter()
            .all(|segment| matches!(segment, Segment::Static(_)))
        {
            self.static_routes
                .entry(template.clone())
                .or_default()
                .insert(method.clone(), index);
        } else {
            self.dynamic_routes.insert(&segments, method.clone(), index);
        }
        self.plans.push(RoutePlan {
            capture_mode,
            body_mode: if H::NEEDS_BODY {
                BodyMode::Buffered
            } else {
                BodyMode::None
            },
            handler,
        });
        self.metadata.push(RouteMetadata {
            builtin: false,
            method,
            template,
            segments,
            capture_names,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                response_status: <H::Response as ResponseMetadata>::status_code(),
                response_schema: <H::Response as ResponseMetadata>::response_schema(),
                request: H::openapi_request(),
            },
        });
        self.openapi_bytes = None;
        self.last_route = Some(index);
    }

    fn add_raw_route<H>(&mut self, method: Method, path: &str, handler: H)
    where
        H: RawHandler<S>,
    {
        let template = normalize_path(path);
        assert!(
            self.metadata
                .iter()
                .all(|metadata| metadata.method != method || metadata.template != template),
            "duplicate route"
        );
        let segments = parse_template(&template);
        let handler = HandlerKind::Raw(Box::new(move |request| handler.call(request)));
        let index = self.plans.len();
        if segments
            .iter()
            .all(|segment| matches!(segment, Segment::Static(_)))
        {
            self.static_routes
                .entry(template.clone())
                .or_default()
                .insert(method.clone(), index);
        } else {
            self.dynamic_routes.insert(&segments, method.clone(), index);
        }
        self.plans.push(RoutePlan {
            capture_mode: CaptureMode::None,
            body_mode: BodyMode::Incoming,
            handler,
        });
        self.metadata.push(RouteMetadata {
            builtin: false,
            method,
            template,
            segments,
            capture_names: None,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                response_status: <H::Response as ResponseMetadata>::status_code(),
                response_schema: <H::Response as ResponseMetadata>::response_schema(),
                request: OpenApiRequest::default(),
            },
        });
        self.openapi_bytes = None;
        self.last_route = Some(index);
    }

    fn install_builtin_route(&mut self, path: String, builtin: BuiltinHandler, is_openapi: bool) {
        let existing = if is_openapi {
            self.openapi_route
        } else {
            self.swagger_route
        };
        if let Some(index) = existing {
            let old_path = self.metadata[index].template.clone();
            if old_path != path {
                if let Some(routes) = self.static_routes.get_mut(&old_path) {
                    routes.remove(&Method::GET);
                }
                if self
                    .static_routes
                    .get(&old_path)
                    .is_some_and(RouteSet::is_empty)
                {
                    self.static_routes.remove(&old_path);
                }
                self.metadata[index].template = path.clone();
                self.metadata[index].segments = parse_template(&path);
                self.static_routes
                    .entry(path)
                    .or_default()
                    .insert(Method::GET, index);
            }
            self.plans[index].handler = HandlerKind::Builtin(builtin);
            return;
        }

        let index = self.plans.len();
        self.static_routes
            .entry(path.clone())
            .or_default()
            .insert(Method::GET, index);
        self.plans.push(RoutePlan {
            capture_mode: CaptureMode::None,
            body_mode: BodyMode::None,
            handler: HandlerKind::Builtin(builtin),
        });
        self.metadata.push(RouteMetadata {
            builtin: true,
            method: Method::GET,
            template: path,
            segments: Vec::new(),
            capture_names: None,
            operation: Operation {
                tag: None,
                summary: None,
                operation_id: None,
                response_status: StatusCode::OK,
                response_schema: None,
                request: OpenApiRequest::default(),
            },
        });
        if is_openapi {
            self.openapi_route = Some(index);
        } else {
            self.swagger_route = Some(index);
        }
    }

    fn prepare_openapi(&mut self) {
        if self.openapi_path.is_some() && self.openapi_bytes.is_none() {
            self.openapi_bytes = Some(Bytes::from(self.openapi_document().to_string()));
        }
    }

    async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
        let method = request.method().clone();
        let path = normalize_request_path(request.uri().path());
        if let Some(routes) = self.static_routes.get(path) {
            return match resolve_route_set(&method, routes) {
                Ok(index) => self.handle_matched(request, index, StaticCaptures).await,
                Err(RouteFailure::Options(allow)) => options_response(&allow),
                Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
            };
        }
        let Some(path_match) = self.dynamic_routes.find(path) else {
            return not_found();
        };
        match resolve_route_set(&method, path_match.routes) {
            Ok(index) => {
                self.handle_matched(request, index, DynamicCaptures(path_match.captures))
                    .await
            }
            Err(RouteFailure::Options(allow)) => options_response(&allow),
            Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
        }
    }

    async fn handle_matched<C: CaptureProvider>(
        &self,
        request: Request<Bytes>,
        index: RouteId,
        captures: C,
    ) -> HttpResponse {
        let method = request.method().clone();
        let plan = &self.plans[index.index()];
        match &plan.handler {
            HandlerKind::Zero(handler) => return maybe_head(&method, handler().await),
            HandlerKind::Static(response) => {
                return maybe_head(&method, response.to_response());
            }
            HandlerKind::Builtin(builtin) => {
                return maybe_head(&method, self.builtin_response(*builtin));
            }
            HandlerKind::Typed(_) | HandlerKind::Raw(_) => {}
        }
        let mut request = request;
        let HandlerKind::Typed(handler) = &plan.handler else {
            unreachable!("raw routes require Incoming request bodies")
        };
        let response = captures
            .invoke(
                &plan.capture_mode,
                &mut request,
                &self.metadata[index.index()],
                handler,
                &self.state,
            )
            .await;
        maybe_head(&method, response)
    }

    fn builtin_response(&self, builtin: BuiltinHandler) -> HttpResponse {
        match builtin {
            BuiltinHandler::OpenApi => {
                let body = self
                    .openapi_bytes
                    .clone()
                    .unwrap_or_else(|| Bytes::from(self.openapi_document().to_string()));
                response_json_bytes(StatusCode::OK, body)
            }
            BuiltinHandler::Swagger => {
                response_text(StatusCode::OK, swagger_html(self.openapi_path.as_deref()))
            }
        }
    }
}

struct RuntimeRef<'a, S> {
    state: &'a Arc<S>,
    plans: &'a [RoutePlan<S>],
    metadata: &'a [RouteMetadata],
    static_routes: &'a HashMap<String, RouteSet>,
    dynamic_routes: &'a DynamicRouteTrie,
    openapi_path: Option<&'a str>,
    openapi_bytes: Option<&'a Bytes>,
}

struct ConnectionRuntime<S> {
    runtime: Arc<AppRuntime<S>>,
}

enum PreparedDispatch {
    Ready(Option<HttpResponse>),
    Handler {
        is_head: bool,
        future: HandlerFuture,
    },
    Buffered(BoxFuture<HttpResponse>),
}

impl Future for PreparedDispatch {
    type Output = HttpResponse;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            match self.get_unchecked_mut() {
                Self::Ready(response) => {
                    Poll::Ready(response.take().expect("prepared response polled twice"))
                }
                Self::Handler { is_head, future } => {
                    match Pin::new_unchecked(future).poll(context) {
                        Poll::Ready(response) => Poll::Ready(maybe_head_flag(*is_head, response)),
                        Poll::Pending => Poll::Pending,
                    }
                }
                Self::Buffered(future) => Pin::new_unchecked(future).poll(context),
            }
        }
    }
}

impl<S: Send + Sync + 'static> ConnectionRuntime<S> {
    fn new(runtime: Arc<AppRuntime<S>>) -> Self {
        Self { runtime }
    }

    fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        self.runtime.runtime_ref()
    }

    fn prepare(&self, request: Request<Incoming>) -> PreparedDispatch {
        let method = request.method();
        let is_head = *method == Method::HEAD;
        let path_end = normalize_request_path(request.uri().path()).len();
        let router = self.runtime_ref();
        let path = &request.uri().path()[..path_end];
        if let Some(routes) = router.static_routes.get(path) {
            return match resolve_route_set(method, routes) {
                Ok(index) => self.prepare_matched(router, request, index, StaticCaptures, is_head),
                Err(RouteFailure::Options(allow)) => {
                    PreparedDispatch::Ready(Some(options_response(&allow)))
                }
                Err(RouteFailure::MethodNotAllowed(allow)) => {
                    PreparedDispatch::Ready(Some(method_not_allowed_response(&allow)))
                }
            };
        }
        let Some(path_match) = router.dynamic_routes.find(path) else {
            return PreparedDispatch::Ready(Some(not_found()));
        };
        match resolve_route_set(method, path_match.routes) {
            Ok(index) => self.prepare_matched(
                router,
                request,
                index,
                DynamicCaptures(path_match.captures),
                is_head,
            ),
            Err(RouteFailure::Options(allow)) => {
                PreparedDispatch::Ready(Some(options_response(&allow)))
            }
            Err(RouteFailure::MethodNotAllowed(allow)) => {
                PreparedDispatch::Ready(Some(method_not_allowed_response(&allow)))
            }
        }
    }

    fn prepare_matched<C: CaptureProvider + Send + 'static>(
        &self,
        router: RuntimeRef<'_, S>,
        request: Request<Incoming>,
        index: RouteId,
        captures: C,
        is_head: bool,
    ) -> PreparedDispatch {
        let plan = &router.plans[index.index()];
        match plan.body_mode {
            BodyMode::Incoming => match &plan.handler {
                HandlerKind::Raw(handler) => PreparedDispatch::Handler {
                    is_head,
                    future: handler(request),
                },
                HandlerKind::Zero(_)
                | HandlerKind::Typed(_)
                | HandlerKind::Static(_)
                | HandlerKind::Builtin(_) => {
                    unreachable!("only raw handlers may receive Incoming")
                }
            },
            BodyMode::None => match &plan.handler {
                HandlerKind::Zero(handler) => PreparedDispatch::Handler {
                    is_head,
                    future: handler(),
                },
                HandlerKind::Static(response) => {
                    PreparedDispatch::Ready(Some(maybe_head_flag(is_head, response.to_response())))
                }
                HandlerKind::Builtin(builtin) => PreparedDispatch::Ready(Some(maybe_head_flag(
                    is_head,
                    router.builtin_response(*builtin),
                ))),
                HandlerKind::Typed(handler) => {
                    let (parts, _) = request.into_parts();
                    let mut request = Request::from_parts(parts, Bytes::new());
                    let future = captures.invoke(
                        &plan.capture_mode,
                        &mut request,
                        &router.metadata[index.index()],
                        handler,
                        router.state,
                    );
                    PreparedDispatch::Handler { is_head, future }
                }
                HandlerKind::Raw(_) => unreachable!("raw handler requires Incoming"),
            },
            BodyMode::Buffered => {
                let limit = DEFAULT_MAX_BODY_SIZE;
                let runtime = Arc::clone(&self.runtime);
                let (parts, body) = request.into_parts();
                let too_large = parts
                    .headers
                    .get(header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<usize>().ok())
                    .is_some_and(|length| length > limit);
                if too_large {
                    return PreparedDispatch::Ready(Some(response_json(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        json!({
                            "type": "about:blank",
                            "title": "Payload Too Large",
                            "status": 413,
                            "detail": "request body exceeds the configured limit"
                        }),
                    )));
                }
                PreparedDispatch::Buffered(Box::pin(async move {
                    match Limited::new(body, limit).collect().await {
                        Ok(body) => {
                            runtime
                                .runtime_ref()
                                .handle_matched(
                                    Request::from_parts(parts, body.to_bytes()),
                                    index,
                                    captures,
                                )
                                .await
                        }
                        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
                            response_json(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                json!({
                                    "type": "about:blank",
                                    "title": "Payload Too Large",
                                    "status": 413,
                                    "detail": "request body exceeds the configured limit"
                                }),
                            )
                        }
                        Err(_) => response_json(
                            StatusCode::BAD_REQUEST,
                            json!({
                                "type": "about:blank",
                                "title": "Bad Request",
                                "status": 400,
                                "detail": "request body was interrupted"
                            }),
                        ),
                    }
                }))
            }
        }
    }
}

impl<S: Send + Sync + 'static> AppRuntime<S> {
    fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        RuntimeRef {
            state: &self.state,
            plans: &self.plans,
            metadata: &self.metadata,
            static_routes: &self.static_routes,
            dynamic_routes: &self.dynamic_routes,
            openapi_path: self.openapi_path.as_deref(),
            openapi_bytes: self.openapi_bytes.as_ref(),
        }
    }

    /// Return the frozen OpenAPI document built from the cold metadata array.
    pub fn openapi_document(&self) -> Value {
        let mut paths = Map::new();
        for metadata in self.metadata.iter() {
            if metadata.builtin {
                continue;
            }
            let method = metadata.method.as_str().to_ascii_lowercase();
            let mut operation = Map::new();
            if let Some(tag) = &metadata.operation.tag {
                operation.insert("tags".to_owned(), json!([tag]));
            }
            if let Some(summary) = &metadata.operation.summary {
                operation.insert("summary".to_owned(), json!(summary));
            }
            if let Some(operation_id) = &metadata.operation.operation_id {
                operation.insert("operationId".to_owned(), json!(operation_id));
            }
            let mut parameters = metadata.operation.request.parameters.clone();
            let mut path_schema_index = 0;
            parameters.extend(
                metadata
                    .segments
                    .iter()
                    .filter_map(|segment| match segment {
                        Segment::Capture(name) => {
                            let schema = metadata
                                .operation
                                .request
                                .path_schemas
                                .get(path_schema_index)
                                .cloned()
                                .unwrap_or_else(|| json!({ "type": "string" }));
                            path_schema_index += 1;
                            Some(json!({
                                "in": "path",
                                "name": name,
                                "required": true,
                                "schema": schema
                            }))
                        }
                        Segment::Static(_) => None,
                    })
                    .collect::<Vec<_>>(),
            );
            if !parameters.is_empty() {
                operation.insert("parameters".to_owned(), Value::Array(parameters));
            }
            if let Some(request_body) = &metadata.operation.request.request_body {
                operation.insert("requestBody".to_owned(), request_body.clone());
            }
            let status = metadata.operation.response_status.as_u16().to_string();
            let mut response = Map::new();
            response.insert(
                "description".to_owned(),
                Value::String(
                    match metadata.operation.response_status {
                        StatusCode::NO_CONTENT => "No Content",
                        StatusCode::NOT_MODIFIED => "Not Modified",
                        StatusCode::CREATED => "Created",
                        _ => "Success",
                    }
                    .to_owned(),
                ),
            );
            if let Some(schema) = &metadata.operation.response_schema {
                response.insert(
                    "content".to_owned(),
                    json!({ "application/json": { "schema": schema } }),
                );
            }
            operation.insert("responses".to_owned(), json!({ status: response }));
            paths
                .entry(metadata.template.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            paths
                .get_mut(&metadata.template)
                .and_then(Value::as_object_mut)
                .unwrap()
                .insert(method, Value::Object(operation));
        }
        json!({
            "openapi": "3.1.0",
            "info": { "title": self.title, "version": self.version },
            "paths": paths,
        })
    }

    pub async fn oneshot(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Bytes>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            if !name.is_empty() {
                builder = builder.header(*name, *value);
            }
        }
        let request = builder
            .body(body.unwrap_or_default())
            .expect("valid test request");
        TestResponse {
            response: Some(self.runtime_ref().handle(request).await),
        }
    }

    pub async fn listen(
        self,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        self.serve_listener(listener, std::future::pending()).await
    }

    pub async fn serve_listener<F>(
        self,
        listener: tokio::net::TcpListener,
        shutdown: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        serve_runtime(self, listener, shutdown).await
    }
}

impl<'a, S: Send + Sync + 'static> RuntimeRef<'a, S> {
    async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
        let method = request.method().clone();
        let path = normalize_request_path(request.uri().path());
        if let Some(routes) = self.static_routes.get(path) {
            return match resolve_route_set(&method, routes) {
                Ok(index) => self.handle_matched(request, index, StaticCaptures).await,
                Err(RouteFailure::Options(allow)) => options_response(&allow),
                Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
            };
        }
        let Some(path_match) = self.dynamic_routes.find(path) else {
            return not_found();
        };
        match resolve_route_set(&method, path_match.routes) {
            Ok(index) => {
                self.handle_matched(request, index, DynamicCaptures(path_match.captures))
                    .await
            }
            Err(RouteFailure::Options(allow)) => options_response(&allow),
            Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
        }
    }

    async fn handle_matched<C: CaptureProvider>(
        &self,
        request: Request<Bytes>,
        index: RouteId,
        captures: C,
    ) -> HttpResponse {
        let method = request.method().clone();
        let plan = &self.plans[index.index()];
        match &plan.handler {
            HandlerKind::Zero(handler) => return maybe_head(&method, handler().await),
            HandlerKind::Static(response) => {
                return maybe_head(&method, response.to_response());
            }
            HandlerKind::Builtin(builtin) => {
                return maybe_head(&method, self.builtin_response(*builtin));
            }
            HandlerKind::Typed(_) | HandlerKind::Raw(_) => {}
        }
        let mut request = request;
        let HandlerKind::Typed(handler) = &plan.handler else {
            unreachable!("raw routes require Incoming request bodies")
        };
        let response = captures
            .invoke(
                &plan.capture_mode,
                &mut request,
                &self.metadata[index.index()],
                handler,
                self.state,
            )
            .await;
        maybe_head(&method, response)
    }

    fn builtin_response(&self, builtin: BuiltinHandler) -> HttpResponse {
        match builtin {
            BuiltinHandler::OpenApi => {
                let body = self
                    .openapi_bytes
                    .expect("OpenAPI bytes are prepared before runtime serving")
                    .clone();
                response_json_bytes(StatusCode::OK, body)
            }
            BuiltinHandler::Swagger => {
                response_text(StatusCode::OK, swagger_html(self.openapi_path))
            }
        }
    }
}

async fn serve_runtime<S, F>(
    runtime: AppRuntime<S>,
    listener: tokio::net::TcpListener,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let runtime = Arc::new(runtime);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let connection = ConnectionRuntime::new(Arc::clone(&runtime));
                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                        let prepared = connection.prepare(request);
                        async move {
                            Ok::<_, Infallible>(prepared.await)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service)
                        .await;
                });
            }
        }
    }
    Ok(())
}

/// A response returned by [`App::oneshot`] for concise integration tests.
pub struct TestResponse {
    response: Option<HttpResponse>,
}

impl TestResponse {
    pub fn status(&self) -> StatusCode {
        self.response.as_ref().unwrap().status()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.response
            .as_ref()
            .and_then(|response| response.headers().get(name))
            .and_then(|value| value.to_str().ok())
    }

    pub async fn body_string(mut self) -> String {
        let response = self.response.take().unwrap();
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }
}

fn parse_template(path: &str) -> Vec<Segment> {
    split_path(path)
        .into_iter()
        .map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                assert!(part.len() > 2, "invalid route template");
                Segment::Capture(part[1..part.len() - 1].to_owned())
            } else {
                assert!(
                    !part.contains('{') && !part.contains('}'),
                    "invalid route template"
                );
                Segment::Static(part.to_owned())
            }
        })
        .collect()
}

fn split_path(path: &str) -> Vec<&str> {
    PathParts::new(path).map(|part| part.value).collect()
}

#[derive(Clone, Copy)]
struct PathPart<'a> {
    value: &'a str,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct PathParts<'a> {
    path: &'a str,
    next: usize,
}

impl<'a> PathParts<'a> {
    fn new(path: &'a str) -> Self {
        Self { path, next: 0 }
    }
}

impl<'a> Iterator for PathParts<'a> {
    type Item = PathPart<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.path.as_bytes();
        while self.next < bytes.len() && bytes[self.next] == b'/' {
            self.next += 1;
        }
        if self.next >= bytes.len() {
            return None;
        }
        let start = self.next;
        while self.next < bytes.len() && bytes[self.next] != b'/' {
            self.next += 1;
        }
        Some(PathPart {
            value: &self.path[start..self.next],
            start,
            end: self.next,
        })
    }
}

fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_owned()
    } else if path.len() > 1 {
        path.trim_end_matches('/').to_owned()
    } else {
        path.to_owned()
    }
}

fn normalize_request_path(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

fn response_text(status: StatusCode, body: Bytes) -> HttpResponse {
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

fn response_json<T: Serialize>(status: StatusCode, value: T) -> HttpResponse {
    response_json_bytes(
        status,
        Bytes::from(serde_json::to_vec(&value).unwrap_or_default()),
    )
}

fn response_json_bytes(status: StatusCode, body: Bytes) -> HttpResponse {
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
fn not_found() -> HttpResponse {
    response_text(StatusCode::NOT_FOUND, Bytes::from_static(b"Not Found"))
}

#[cold]
#[inline(never)]
fn options_response(allow: &str) -> HttpResponse {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::ALLOW, allow)
        .header(header::CONTENT_LENGTH, "0")
        .body(ResponseBody::full(Bytes::new()))
        .unwrap()
}

#[cold]
#[inline(never)]
fn method_not_allowed_response(allow: &str) -> HttpResponse {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(header::ALLOW, allow)
        .header(header::CONTENT_LENGTH, "0")
        .body(ResponseBody::full(Bytes::new()))
        .unwrap()
}

fn maybe_head(method: &Method, response: HttpResponse) -> HttpResponse {
    maybe_head_flag(method == Method::HEAD, response)
}

fn maybe_head_flag(is_head: bool, response: HttpResponse) -> HttpResponse {
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

fn parse_query<T: DeserializeOwned>(query: &str) -> Result<T, ApiError> {
    let mut object = Map::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key)?;
        let value = percent_decode(value)?;
        let json_value = match value.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            value if value.parse::<i64>().is_ok() => json!(value.parse::<i64>().unwrap()),
            value if value.parse::<f64>().is_ok() => json!(value.parse::<f64>().unwrap()),
            value => Value::String(value.to_owned()),
        };
        object.insert(key, json_value);
    }
    serde_json::from_value(Value::Object(object))
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

fn percent_decode(value: &str) -> Result<String, ApiError> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(ApiError::bad_request("invalid percent encoding"));
            }
            let high = hex(bytes[index + 1])
                .ok_or_else(|| ApiError::bad_request("invalid percent encoding"))?;
            let low = hex(bytes[index + 2])
                .ok_or_else(|| ApiError::bad_request("invalid percent encoding"))?;
            output.push(high * 16 + low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output)
        .map_err(|_| ApiError::bad_request("invalid UTF-8 in percent encoding"))
}

pub fn decode_query_component(value: &str) -> Result<Cow<'_, str>, ApiError> {
    if value.as_bytes().contains(&b'%') {
        Ok(Cow::Owned(percent_decode(value)?))
    } else {
        Ok(Cow::Borrowed(value))
    }
}

fn valid_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || hex(bytes[index + 1]).is_none()
                || hex(bytes[index + 2]).is_none()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn swagger_html(openapi_path: Option<&str>) -> Bytes {
    let openapi_path = openapi_path.unwrap_or("/openapi.json");
    let encoded_path = serde_json::to_string(openapi_path).unwrap();
    Bytes::from(format!(
        "<!doctype html><html><head><title>Swagger UI</title><link rel=\"stylesheet\" href=\"https://unpkg.com/swagger-ui-dist@5/swagger-ui.css\"></head><body><div id=\"swagger-ui\">Loading Swagger UI…</div><script src=\"https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js\"></script><script>window.onload=()=>window.ui=SwaggerUIBundle({{url:{encoded_path},dom_id:'#swagger-ui'}});</script></body></html>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn openapi_document_is_prepared_once_for_server_dispatch() {
        let mut app = App::new();
        app.get("/plaintext", || async { "OK" });
        app.openapi("/openapi.json");
        assert!(app.openapi_bytes.is_none());

        app.prepare_openapi();
        let prepared = app.openapi_bytes.clone().expect("prepared document");
        let response = app
            .handle(
                Request::builder()
                    .method(Method::GET)
                    .uri("/openapi.json")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(app.openapi_bytes.as_ref(), Some(&prepared));
    }

    #[tokio::test]
    async fn zero_argument_routes_use_the_fast_dispatch_shape() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });

        let index = resolve_route_set(
            &Method::GET,
            app.static_routes.get("/zero").expect("zero route"),
        )
        .expect("zero route did not resolve");
        assert!(matches!(
            app.plans[index.index()].handler,
            HandlerKind::Zero(_)
        ));

        let response = app.oneshot(Method::GET, "/zero", &[], None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body_string().await, "OK");
    }

    #[test]
    fn runtime_keeps_hot_plans_separate_from_cold_metadata() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });
        app.openapi("/openapi.json");

        assert_eq!(app.plans.len(), 2);
        assert_eq!(app.metadata.len(), 2);
    }

    #[test]
    fn build_freezes_route_storage_into_boxed_slices() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });

        let runtime = app.build();

        assert_eq!(runtime.plans.len(), 1);
        assert_eq!(runtime.metadata.len(), 1);
    }

    #[test]
    fn app_builder_is_available_as_the_registration_type() {
        let mut builder = AppBuilder::new();
        builder.get("/zero", || async { "OK" });
        assert_eq!(builder.plans.len(), 1);
    }

    #[test]
    fn connection_runtime_reuses_one_runtime_owner_for_request_borrows() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });
        let runtime = Arc::new(app.build());
        let connection = ConnectionRuntime::new(Arc::clone(&runtime));

        assert_eq!(Arc::strong_count(&runtime), 2);
        let _first = connection.runtime_ref();
        let _second = connection.runtime_ref();
        assert_eq!(Arc::strong_count(&runtime), 2);
    }

    #[test]
    fn no_capture_routes_use_a_shared_empty_params() {
        assert!(std::ptr::eq(Params::empty(), Params::empty()));
        assert!(Params::empty().get("id").is_none());
    }

    #[test]
    fn materialized_params_clone_shares_owned_values() {
        let names = vec!["id".to_owned()];
        let captures = CaptureSet::default().with_capture(0, CaptureRange { start: 1, end: 6 });
        let params = Params::from_match(&names, captures, "/alice", true);
        let cloned = params.clone();

        assert_eq!(params.get("id"), Some("alice"));
        assert_eq!(cloned.get("id"), Some("alice"));
        assert!(Arc::ptr_eq(
            params.owned.as_ref().expect("materialized values"),
            cloned.owned.as_ref().expect("materialized values"),
        ));
    }

    #[test]
    fn params_keep_capture_ranges_in_compact_storage() {
        assert!(size_of::<Params>() <= 88);
    }

    #[test]
    fn route_ids_are_compact_u32_handles() {
        assert_eq!(size_of::<RouteId>(), size_of::<u32>());
        assert_eq!(size_of::<NodeId>(), size_of::<u32>());
        assert!(size_of::<Option<NodeId>>() <= size_of::<Option<usize>>());
        assert!(size_of::<DynamicRouteNode>() <= 88);
        assert!(size_of::<RouteSet>() <= 32);
    }

    #[test]
    fn route_sets_keep_allow_metadata_off_the_hot_slots() {
        let mut routes = RouteSet::default();
        assert!(routes.is_empty());
        routes.insert(Method::POST, 0);
        routes.insert(Method::GET, 1);
        assert_eq!(routes.allowed_methods(), "GET, POST");
        routes.remove(&Method::GET);
        assert_eq!(routes.allowed_methods(), "POST");
        assert!(!routes.is_empty());
        routes.remove(&Method::POST);
        assert!(routes.is_empty());
    }

    #[test]
    fn route_plans_encode_capture_materialization_mode() {
        let mut app = App::new();
        app.get("/plain", || async { "OK" });
        app.get("/path/{id}", |Path(_id): Path<String>| async { "OK" });
        app.get("/params/{id}", |_params: Params| async { "OK" });

        assert!(matches!(app.plans[0].capture_mode, CaptureMode::None));
        assert!(matches!(app.plans[1].capture_mode, CaptureMode::Borrowed));
        assert!(matches!(
            app.plans[2].capture_mode,
            CaptureMode::Materialized
        ));
    }

    #[test]
    fn route_plans_precompute_body_modes() {
        let mut app = App::new();
        app.get("/plain", || async { "OK" });
        app.post("/json", |Json(body): Json<String>| async move { body });
        app.raw_get("/upload", |_request| async { "OK" });

        assert!(matches!(app.plans[0].body_mode, BodyMode::None));
        assert!(matches!(app.plans[1].body_mode, BodyMode::Buffered));
        assert!(matches!(app.plans[2].body_mode, BodyMode::Incoming));
    }

    #[test]
    fn raw_routes_support_explicit_methods() {
        let mut app = App::new();
        app.raw(Method::POST, "/upload", |_request| async { "OK" });

        let index = resolve_route_set(
            &Method::POST,
            app.static_routes.get("/upload").expect("raw route"),
        )
        .expect("raw POST route did not resolve");
        assert!(matches!(
            app.plans[index.index()].handler,
            HandlerKind::Raw(_)
        ));
    }

    #[test]
    fn dynamic_routes_use_a_compiled_method_trie() {
        let mut app = App::new();
        for index in 0..10_000 {
            let path = format!("/dynamic/{index}/{{id}}");
            app.get(&path, || async { "OK" });
        }

        assert!(app.dynamic_routes.node_count() < 20_010);
        let matched = app
            .dynamic_routes
            .find("/dynamic/9999/42")
            .expect("dynamic route match");
        assert_eq!(
            matched.routes.route(&Method::GET).map(RouteId::index),
            Some(9_999)
        );
        assert_eq!(matched.captures.count, 1);
        assert!(app.dynamic_routes.find("/dynamic/missing/42").is_none());
    }

    #[test]
    fn report_hot_path_layout_sizes() {
        println!(
            "HandlerFuture={} InlineFuture={} Params={} CaptureMode={} BodyMode={} HandlerKind={} RoutePlan={} RouteMetadata={} RouteSet={} DynamicRouteNode={} RouteFailure={}",
            size_of::<HandlerFuture>(),
            size_of::<InlineFuture>(),
            size_of::<Params>(),
            size_of::<CaptureMode>(),
            size_of::<BodyMode>(),
            size_of::<HandlerKind<()>>(),
            size_of::<RoutePlan<()>>(),
            size_of::<RouteMetadata>(),
            size_of::<RouteSet>(),
            size_of::<DynamicRouteNode>(),
            size_of::<RouteFailure>(),
        );
    }

    #[test]
    fn capture_names_stay_out_of_hot_route_plans() {
        assert_eq!(size_of::<CaptureMode>(), 1);
        assert!(size_of::<RoutePlan<()>>() <= 32);
        assert!(size_of::<RouteFailure>() <= 32);
    }

    #[tokio::test]
    async fn inline_future_preserves_pinned_application_future() {
        struct PinnedReady {
            _pin: PhantomPinned,
        }

        impl Future for PinnedReady {
            type Output = &'static str;

            fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
                Poll::Ready("OK")
            }
        }

        let response = HandlerFuture::from_response_future(PinnedReady {
            _pin: PhantomPinned,
        })
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            Bytes::from_static(b"OK")
        );
    }

    #[tokio::test]
    async fn dynamic_terminal_resolves_methods_without_route_scan() {
        let mut app = App::new();
        app.get("/resource/{id}", || async { "GET" });
        app.post("/resource/{id}", || async { "POST" });

        let response = app.oneshot(Method::PUT, "/resource/42", &[], None).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.header("allow"), Some("GET, POST"));

        let response = app
            .oneshot(Method::OPTIONS, "/resource/42", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.header("allow"), Some("GET, POST"));

        let response = app.oneshot(Method::PUT, "/other/missing", &[], None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
