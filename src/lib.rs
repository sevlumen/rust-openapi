//! `oas-rs`: a small, typed HTTP framework built on Hyper and Tokio.
//!
//! The runtime deliberately keeps OpenAPI generation out of request dispatch. The
//! document is assembled when routes are registered and is only serialized when
//! the explicitly registered OpenAPI endpoint is requested.

use bytes::Bytes;
use futures_core::Stream;
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    borrow::Cow,
    collections::HashMap,
    convert::Infallible,
    future::Future,
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
    Full(Full<Bytes>),
    Stream(Pin<Box<dyn Stream<Item = Bytes> + Send + 'static>>),
}

impl ResponseBody {
    fn full(bytes: Bytes) -> Self {
        Self::Full(Full::new(bytes))
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
        unsafe {
            match self.get_unchecked_mut() {
                Self::Full(body) => Pin::new_unchecked(body).poll_frame(context),
                Self::Stream(stream) => match stream.as_mut().poll_next(context) {
                    Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
                    Poll::Ready(None) => Poll::Ready(None),
                    Poll::Pending => Poll::Pending,
                },
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Full(body) => body.is_end_stream(),
            Self::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(body) => body.size_hint(),
            Self::Stream(_) => SizeHint::default(),
        }
    }
}

const INLINE_FUTURE_SIZE: usize = 128;

#[repr(align(64))]
struct FutureStorage([MaybeUninit<u8>; INLINE_FUTURE_SIZE]);

struct InlineFuture {
    storage: FutureStorage,
    poll_fn: unsafe fn(*mut u8, &mut Context<'_>) -> Poll<HttpResponse>,
    drop_fn: unsafe fn(*mut u8),
    _pin: PhantomPinned,
}

impl InlineFuture {
    fn new<F>(future: F) -> Self
    where
        F: Future<Output = HttpResponse> + Send + 'static,
    {
        debug_assert!(size_of::<F>() <= INLINE_FUTURE_SIZE);
        debug_assert!(align_of::<F>() <= align_of::<FutureStorage>());
        let mut storage = FutureStorage([MaybeUninit::uninit(); INLINE_FUTURE_SIZE]);
        unsafe {
            (storage.0.as_mut_ptr() as *mut F).write(future);
        }
        Self {
            storage,
            poll_fn: poll_inline::<F>,
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

unsafe fn poll_inline<F>(storage: *mut u8, context: &mut Context<'_>) -> Poll<HttpResponse>
where
    F: Future<Output = HttpResponse> + Send + 'static,
{
    unsafe { Pin::new_unchecked(&mut *(storage as *mut F)).poll(context) }
}

unsafe fn drop_inline<F>(storage: *mut u8)
where
    F: Future<Output = HttpResponse> + Send + 'static,
{
    unsafe { std::ptr::drop_in_place(storage as *mut F) };
}

pub struct HandlerFuture(HandlerFutureKind);

enum HandlerFutureKind {
    Inline(InlineFuture),
    Boxed(BoxFuture<HttpResponse>),
}

impl HandlerFuture {
    fn from_future<F>(future: F) -> Self
    where
        F: Future<Output = HttpResponse> + Send + 'static,
    {
        if size_of::<F>() <= INLINE_FUTURE_SIZE && align_of::<F>() <= align_of::<FutureStorage>() {
            Self(HandlerFutureKind::Inline(InlineFuture::new(future)))
        } else {
            Self(HandlerFutureKind::Boxed(Box::pin(future)))
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
    ranges: [Option<CaptureRange>; MAX_CAPTURE_PARAMS],
    owned: Option<Vec<(String, String)>>,
}

impl Params {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.owned
            .as_ref()?
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn first_raw<'a>(&self, path: &'a str) -> Option<&'a str> {
        self.ranges[0].map(|range| &path[range.start..range.end])
    }

    fn from_match(
        names: &[String],
        ranges: [Option<CaptureRange>; MAX_CAPTURE_PARAMS],
        count: usize,
        path: &str,
        materialize: bool,
    ) -> Self {
        let owned = materialize.then(|| {
            let mut values = Vec::with_capacity(count);
            for (index, name) in names.iter().enumerate() {
                let range = ranges[index].expect("capture range exists");
                let value = percent_decode(&path[range.start..range.end])
                    .expect("route matching validates percent encoding");
                values.push((name.clone(), value));
            }
            values
        });
        Self { ranges, owned }
    }
}

impl Default for Params {
    fn default() -> Self {
        Self {
            ranges: [None; MAX_CAPTURE_PARAMS],
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
    pub fn new(status: StatusCode, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status,
            title: title.into(),
            detail: detail.into(),
            missing: false,
        }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "Bad Request", detail)
    }

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
    content_length: HeaderValue,
}

impl JsonBytes {
    pub fn new(bytes: Bytes) -> Self {
        let content_length = HeaderValue::from_str(&bytes.len().to_string()).unwrap();
        Self {
            bytes,
            content_length,
        }
    }
}

impl IntoResponse for JsonBytes {
    fn into_response(self) -> HttpResponse {
        response_json_bytes_with_length(StatusCode::OK, self.bytes, self.content_length)
    }
}

/// A response whose chunks are produced lazily by a `Stream`. Streaming is
/// opt-in; ordinary `Full<Bytes>` responses retain their fixed-size body path.
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

    fn call(&self, request: Request<Bytes>, params: Params, state: Arc<S>) -> HandlerFuture;
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
        HandlerFuture::from_future(async move { future.await.into_response() })
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
        if !content_type.starts_with("application/json") {
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
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
{
    type Response = R;
    const NEEDS_PARAMS: bool = false;
    const NEEDS_BODY: bool = false;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn call(&self, _request: Request<Bytes>, _params: Params, _state: Arc<S>) -> HandlerFuture {
        let future = (self)();
        HandlerFuture::from_future(async move { future.await.into_response() })
    }
}

impl<S, F, Fut, R, E1> Handler<S, (E1,)> for F
where
    S: Send + Sync + 'static,
    F: Fn(E1) -> Fut + Clone + Send + Sync + 'static,
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

    fn call(&self, mut request: Request<Bytes>, params: Params, state: Arc<S>) -> HandlerFuture {
        match E1::from_request(&mut request, &params, &state) {
            Ok(value) => {
                let handler = self.clone();
                HandlerFuture::from_future(async move { handler(value).await.into_response() })
            }
            Err(error) => HandlerFuture::from_future(async move { error.into_response() }),
        }
    }
}

impl<S, F, Fut, R, E1, E2> Handler<S, (E1, E2)> for F
where
    S: Send + Sync + 'static,
    F: Fn(E1, E2) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse + ResponseMetadata,
    E1: FromRequest<S>,
    E2: FromRequest<S>,
{
    type Response = R;
    const NEEDS_PARAMS: bool = E1::NEEDS_PARAMS || E2::NEEDS_PARAMS;
    const NEEDS_BODY: bool = E1::NEEDS_BODY || E2::NEEDS_BODY;

    fn openapi_request() -> OpenApiRequest {
        let mut metadata = E1::openapi_request();
        metadata.merge(E2::openapi_request());
        metadata
    }

    fn call(&self, mut request: Request<Bytes>, params: Params, state: Arc<S>) -> HandlerFuture {
        let first = E1::from_request(&mut request, &params, &state);
        let second = E2::from_request(&mut request, &params, &state);
        match (first, second) {
            (Ok(first), Ok(second)) => {
                let handler = self.clone();
                HandlerFuture::from_future(
                    async move { handler(first, second).await.into_response() },
                )
            }
            (Err(error), _) | (_, Err(error)) => {
                HandlerFuture::from_future(async move { error.into_response() })
            }
        }
    }
}

type ErasedHandler<S> = Box<dyn Fn(Request<Bytes>, Params, Arc<S>) -> HandlerFuture + Send + Sync>;
type ErasedRawHandler = Box<dyn Fn(Request<Incoming>) -> HandlerFuture + Send + Sync>;

struct Route<S> {
    method: Method,
    template: String,
    segments: Vec<Segment>,
    capture_names: Arc<[String]>,
    materialize_params: bool,
    needs_body: bool,
    handler: Option<ErasedHandler<S>>,
    raw_handler: Option<ErasedRawHandler>,
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
    static_children: HashMap<String, usize>,
    capture_child: Option<usize>,
    route: Option<usize>,
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

    fn insert(&mut self, segments: &[Segment], route: usize) {
        let mut node_index = 0;
        for segment in segments {
            let next_index = match segment {
                Segment::Static(value) => {
                    if let Some(index) = self.nodes[node_index].static_children.get(value) {
                        *index
                    } else {
                        let index = self.nodes.len();
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
                        let index = self.nodes.len();
                        self.nodes.push(DynamicRouteNode::default());
                        self.nodes[node_index].capture_child = Some(index);
                        index
                    }
                }
            };
            node_index = next_index;
        }
        assert!(
            self.nodes[node_index].route.replace(route).is_none(),
            "duplicate dynamic route pattern"
        );
    }

    fn find(
        &self,
        path: &str,
    ) -> Option<(usize, [Option<CaptureRange>; MAX_CAPTURE_PARAMS], usize)> {
        self.find_node(0, PathParts::new(path), [None; MAX_CAPTURE_PARAMS], 0)
    }

    fn find_node(
        &self,
        node_index: usize,
        mut parts: PathParts<'_>,
        ranges: [Option<CaptureRange>; MAX_CAPTURE_PARAMS],
        capture_count: usize,
    ) -> Option<(usize, [Option<CaptureRange>; MAX_CAPTURE_PARAMS], usize)> {
        let node = &self.nodes[node_index];
        let Some(part) = parts.next() else {
            return node.route.map(|route| (route, ranges, capture_count));
        };

        // Static branches have precedence over captures, but the capture
        // branch remains available if the static branch fails deeper down.
        if let Some(&child) = node.static_children.get(part.value)
            && let Some(found) = self.find_node(child, parts, ranges, capture_count)
        {
            return Some(found);
        }

        if capture_count < MAX_CAPTURE_PARAMS
            && valid_percent_encoding(part.value)
            && let Some(child) = node.capture_child
        {
            let mut captured = ranges;
            captured[capture_count] = Some(CaptureRange {
                start: part.start,
                end: part.end,
            });
            if let Some(found) = self.find_node(child, parts, captured, capture_count + 1) {
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

/// The application router and runtime.
pub struct App<S = ()> {
    state: Arc<S>,
    routes: Vec<Route<S>>,
    static_routes: HashMap<String, Vec<(Method, usize)>>,
    dynamic_routes: HashMap<Method, DynamicRouteTrie>,
    last_route: Option<usize>,
    openapi_path: Option<String>,
    openapi_bytes: Option<Bytes>,
    swagger_path: Option<String>,
    title: String,
    version: String,
}

impl App<()> {
    pub fn new() -> Self {
        Self {
            state: Arc::new(()),
            routes: Vec::new(),
            static_routes: HashMap::new(),
            dynamic_routes: HashMap::new(),
            last_route: None,
            openapi_path: None,
            openapi_bytes: None,
            swagger_path: None,
            title: "oas-rs API".to_owned(),
            version: "0.1.0".to_owned(),
        }
    }

    pub fn with_state<T: Send + Sync + 'static>(self, state: T) -> App<T> {
        assert!(
            self.routes.is_empty(),
            "with_state must be configured before routes"
        );
        App {
            state: Arc::new(state),
            routes: Vec::new(),
            static_routes: HashMap::new(),
            dynamic_routes: HashMap::new(),
            last_route: None,
            openapi_path: self.openapi_path,
            openapi_bytes: self.openapi_bytes,
            swagger_path: self.swagger_path,
            title: self.title,
            version: self.version,
        }
    }
}

impl Default for App<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Send + Sync + 'static> App<S> {
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

    pub fn get<H, A>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: Clone + Handler<S, A>,
    {
        self.add_route(Method::GET, path, handler);
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

    /// Register a GET handler that receives Hyper's streaming `Incoming`
    /// body directly. The framework does not collect or size-limit this body;
    /// the handler owns cancellation and any upload limits.
    pub fn raw_get<H>(&mut self, path: &str, handler: H) -> &mut Self
    where
        H: RawHandler<S>,
    {
        self.add_raw_route(Method::GET, path, handler);
        self
    }

    pub fn tag(&mut self, tag: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.routes[index].operation.tag = Some(tag.into());
            self.openapi_bytes = None;
        }
        self
    }

    pub fn summary(&mut self, summary: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.routes[index].operation.summary = Some(summary.into());
            self.openapi_bytes = None;
        }
        self
    }

    pub fn operation_id(&mut self, operation_id: impl Into<String>) -> &mut Self {
        let operation_id = operation_id.into();
        assert!(
            self.routes.iter().all(|route| {
                route.operation.operation_id.as_deref() != Some(operation_id.as_str())
            }),
            "duplicate operation id"
        );
        if let Some(index) = self.last_route {
            self.routes[index].operation.operation_id = Some(operation_id);
            self.openapi_bytes = None;
        }
        self
    }

    pub fn openapi(&mut self, path: &str) -> &mut Self {
        self.openapi_path = Some(normalize_path(path));
        self.openapi_bytes = None;
        self
    }

    pub fn swagger(&mut self, path: &str) -> &mut Self {
        self.swagger_path = Some(normalize_path(path));
        self
    }

    pub fn openapi_document(&self) -> Value {
        let mut paths = Map::new();
        for route in &self.routes {
            let method = route.method.as_str().to_ascii_lowercase();
            let mut operation = Map::new();
            if let Some(tag) = &route.operation.tag {
                operation.insert("tags".to_owned(), json!([tag]));
            }
            if let Some(summary) = &route.operation.summary {
                operation.insert("summary".to_owned(), json!(summary));
            }
            if let Some(operation_id) = &route.operation.operation_id {
                operation.insert("operationId".to_owned(), json!(operation_id));
            }
            let mut parameters = route.operation.request.parameters.clone();
            let mut path_schema_index = 0;
            parameters.extend(
                route
                    .segments
                    .iter()
                    .filter_map(|segment| match segment {
                        Segment::Capture(name) => {
                            let schema = route
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
            if let Some(request_body) = &route.operation.request.request_body {
                operation.insert("requestBody".to_owned(), request_body.clone());
            }
            let status = route.operation.response_status.as_u16().to_string();
            let mut response = Map::new();
            response.insert(
                "description".to_owned(),
                Value::String(
                    match route.operation.response_status {
                        StatusCode::NO_CONTENT => "No Content",
                        StatusCode::NOT_MODIFIED => "Not Modified",
                        StatusCode::CREATED => "Created",
                        _ => "Success",
                    }
                    .to_owned(),
                ),
            );
            if let Some(schema) = &route.operation.response_schema {
                response.insert(
                    "content".to_owned(),
                    json!({ "application/json": { "schema": schema } }),
                );
            }
            operation.insert("responses".to_owned(), json!({ status: response }));
            paths
                .entry(route.template.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            paths
                .get_mut(&route.template)
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
            response: Some(self.handle(request).await),
        }
    }

    pub async fn listen(
        self,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        self.serve_listener(listener, std::future::pending()).await
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
        let mut app = self;
        app.prepare_openapi();
        let app = Arc::new(app);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let app = Arc::clone(&app);
                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                            let app = Arc::clone(&app);
                            async move {
                                let path = normalize_request_path(request.uri().path());
                                let needs_incoming = app.needs_incoming(request.method(), path);
                                let response = if needs_incoming {
                                    app.handle_incoming(request).await
                                } else {
                                    let (parts, body) = request.into_parts();
                                    let needs_body = app.needs_body(&parts.method, normalize_request_path(parts.uri.path()));
                                    if !needs_body {
                                        app.handle(Request::from_parts(parts, Bytes::new())).await
                                    } else if parts
                                        .headers
                                        .get(header::CONTENT_LENGTH)
                                        .and_then(|value| value.to_str().ok())
                                        .and_then(|value| value.parse::<usize>().ok())
                                        .is_some_and(|length| length > DEFAULT_MAX_BODY_SIZE)
                                    {
                                        response_json(
                                            StatusCode::PAYLOAD_TOO_LARGE,
                                            json!({
                                                "type": "about:blank",
                                                "title": "Payload Too Large",
                                                "status": 413,
                                                "detail": "request body exceeds the configured limit"
                                            }),
                                        )
                                    } else {
                                        match Limited::new(body, DEFAULT_MAX_BODY_SIZE).collect().await {
                                            Ok(body) => app.handle(Request::from_parts(parts, body.to_bytes())).await,
                                            Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => response_json(
                                                StatusCode::PAYLOAD_TOO_LARGE,
                                                json!({
                                                    "type": "about:blank",
                                                    "title": "Payload Too Large",
                                                    "status": 413,
                                                    "detail": "request body exceeds the configured limit"
                                                }),
                                            ),
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
                                    }
                                };
                                Ok::<_, Infallible>(response)
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

    fn add_route<H, A>(&mut self, method: Method, path: &str, handler: H)
    where
        H: Clone + Handler<S, A>,
    {
        let template = normalize_path(path);
        assert!(
            self.routes
                .iter()
                .all(|route| route.method != method || route.template != template),
            "duplicate route"
        );
        let segments = parse_template(&template);
        let capture_names: Arc<[String]> = segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Capture(name) => Some(name.clone()),
                Segment::Static(_) => None,
            })
            .collect::<Vec<_>>()
            .into();
        let erased: ErasedHandler<S> =
            Box::new(move |request, params, state| handler.call(request, params, state));
        let index = self.routes.len();
        if segments
            .iter()
            .all(|segment| matches!(segment, Segment::Static(_)))
        {
            self.static_routes
                .entry(template.clone())
                .or_default()
                .push((method.clone(), index));
        } else {
            self.dynamic_routes
                .entry(method.clone())
                .or_default()
                .insert(&segments, index);
        }
        self.routes.push(Route {
            method,
            template,
            segments,
            capture_names,
            materialize_params: H::NEEDS_PARAMS,
            needs_body: H::NEEDS_BODY,
            handler: Some(erased),
            raw_handler: None,
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
            self.routes
                .iter()
                .all(|route| route.method != method || route.template != template),
            "duplicate route"
        );
        let segments = parse_template(&template);
        let capture_names: Arc<[String]> = segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Capture(name) => Some(name.clone()),
                Segment::Static(_) => None,
            })
            .collect::<Vec<_>>()
            .into();
        let erased: ErasedRawHandler = Box::new(move |request| handler.call(request));
        let index = self.routes.len();
        if segments
            .iter()
            .all(|segment| matches!(segment, Segment::Static(_)))
        {
            self.static_routes
                .entry(template.clone())
                .or_default()
                .push((method.clone(), index));
        } else {
            self.dynamic_routes
                .entry(method.clone())
                .or_default()
                .insert(&segments, index);
        }
        self.routes.push(Route {
            method,
            template,
            segments,
            capture_names,
            materialize_params: false,
            needs_body: false,
            handler: None,
            raw_handler: Some(erased),
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

    fn prepare_openapi(&mut self) {
        if self.openapi_path.is_some() && self.openapi_bytes.is_none() {
            self.openapi_bytes = Some(Bytes::from(self.openapi_document().to_string()));
        }
    }

    async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
        let method = request.method().clone();
        let path = normalize_request_path(request.uri().path());
        if self.openapi_path.as_deref() == Some(path) && method == Method::GET {
            let body = self
                .openapi_bytes
                .clone()
                .unwrap_or_else(|| Bytes::from(self.openapi_document().to_string()));
            return response_json_bytes(StatusCode::OK, body);
        }
        if self.swagger_path.as_deref() == Some(path) && method == Method::GET {
            return response_text(StatusCode::OK, swagger_html(self.openapi_path.as_deref()));
        }
        if method == Method::OPTIONS {
            let allow = self.allowed_methods(path);
            return if allow.is_empty() {
                not_found()
            } else {
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .header(header::ALLOW, allow)
                    .header(header::CONTENT_LENGTH, "0")
                    .body(ResponseBody::full(Bytes::new()))
                    .unwrap()
            };
        }
        if let Some(index) = self.static_route(&method, path) {
            return maybe_head(
                &method,
                (self.routes[index]
                    .handler
                    .as_ref()
                    .expect("typed route handler"))(
                    request,
                    Params::default(),
                    Arc::clone(&self.state),
                )
                .await,
            );
        }
        if method == Method::HEAD
            && let Some(index) = self.static_route(&Method::GET, path)
        {
            return maybe_head(
                &method,
                (self.routes[index]
                    .handler
                    .as_ref()
                    .expect("typed route handler"))(
                    request,
                    Params::default(),
                    Arc::clone(&self.state),
                )
                .await,
            );
        }
        if let Some((index, params)) = self.dynamic_match(&method, path) {
            let route = &self.routes[index];
            return maybe_head(
                &method,
                (route.handler.as_ref().expect("typed route handler"))(
                    request,
                    params,
                    Arc::clone(&self.state),
                )
                .await,
            );
        }
        if method == Method::HEAD
            && let Some((index, params)) = self.dynamic_match(&Method::GET, path)
        {
            let route = &self.routes[index];
            return maybe_head(
                &method,
                (route.handler.as_ref().expect("typed route handler"))(
                    request,
                    params,
                    Arc::clone(&self.state),
                )
                .await,
            );
        }
        if !self.allowed_methods(path).is_empty() {
            return Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, self.allowed_methods(path))
                .header(header::CONTENT_LENGTH, "0")
                .body(ResponseBody::full(Bytes::new()))
                .unwrap();
        }
        not_found()
    }

    async fn handle_incoming(&self, request: Request<Incoming>) -> HttpResponse {
        let method = request.method().clone();
        let path = normalize_request_path(request.uri().path());
        if let Some(index) = self.static_route(&method, path)
            && let Some(handler) = self.routes[index].raw_handler.as_ref()
        {
            return maybe_head(&method, handler(request).await);
        }
        if method == Method::HEAD
            && let Some(index) = self.static_route(&Method::GET, path)
            && let Some(handler) = self.routes[index].raw_handler.as_ref()
        {
            return maybe_head(&method, handler(request).await);
        }
        if let Some(index) = self.dynamic_route(&method, path)
            && let Some(handler) = self.routes[index].raw_handler.as_ref()
        {
            return maybe_head(&method, handler(request).await);
        }
        if method == Method::HEAD
            && let Some(index) = self.dynamic_route(&Method::GET, path)
            && let Some(handler) = self.routes[index].raw_handler.as_ref()
        {
            return maybe_head(&method, handler(request).await);
        }
        unreachable!("raw request was dispatched without a matching raw route")
    }

    fn static_route(&self, method: &Method, path: &str) -> Option<usize> {
        self.static_routes
            .get(path)
            .and_then(|routes| {
                routes
                    .iter()
                    .find(|(route_method, _)| route_method == method)
            })
            .map(|(_, index)| *index)
    }

    fn dynamic_route(&self, method: &Method, path: &str) -> Option<usize> {
        self.dynamic_routes
            .get(method)?
            .find(path)
            .map(|(index, _, _)| index)
    }

    fn dynamic_match(&self, method: &Method, path: &str) -> Option<(usize, Params)> {
        let (index, ranges, count) = self.dynamic_routes.get(method)?.find(path)?;
        let route = &self.routes[index];
        Some((
            index,
            Params::from_match(
                &route.capture_names,
                ranges,
                count,
                path,
                route.materialize_params,
            ),
        ))
    }

    fn needs_body(&self, method: &Method, path: &str) -> bool {
        if *method == Method::OPTIONS {
            return false;
        }
        if let Some(index) = self.static_route(method, path) {
            return self.routes[index].needs_body;
        }
        if *method == Method::HEAD
            && let Some(index) = self.static_route(&Method::GET, path)
        {
            return self.routes[index].needs_body;
        }
        if let Some(index) = self.dynamic_route(method, path) {
            return self.routes[index].needs_body;
        }
        if *method == Method::HEAD
            && let Some(index) = self.dynamic_route(&Method::GET, path)
        {
            return self.routes[index].needs_body;
        }
        false
    }

    fn needs_incoming(&self, method: &Method, path: &str) -> bool {
        if let Some(index) = self.static_route(method, path) {
            return self.routes[index].raw_handler.is_some();
        }
        if *method == Method::HEAD
            && let Some(index) = self.static_route(&Method::GET, path)
        {
            return self.routes[index].raw_handler.is_some();
        }
        if let Some(index) = self.dynamic_route(method, path) {
            return self.routes[index].raw_handler.is_some();
        }
        if *method == Method::HEAD
            && let Some(index) = self.dynamic_route(&Method::GET, path)
        {
            return self.routes[index].raw_handler.is_some();
        }
        false
    }

    fn allowed_methods(&self, path: &str) -> String {
        let mut methods = Vec::new();
        for route in &self.routes {
            if match_route(&route.segments, &route.capture_names, false, path).is_some()
                && !methods
                    .iter()
                    .any(|method| *method == route.method.as_str())
            {
                methods.push(route.method.as_str());
            }
        }
        methods.sort_unstable();
        methods.join(", ")
    }
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

fn match_route(
    segments: &[Segment],
    capture_names: &[String],
    materialize: bool,
    path: &str,
) -> Option<Params> {
    let mut parts = PathParts::new(path);
    let mut ranges = [None; MAX_CAPTURE_PARAMS];
    let mut capture_index = 0;
    for segment in segments {
        let part = parts.next()?;
        match segment {
            Segment::Static(expected) if expected != part.value => return None,
            Segment::Static(_) => {}
            Segment::Capture(_) => {
                if capture_index == MAX_CAPTURE_PARAMS || !valid_percent_encoding(part.value) {
                    return None;
                }
                ranges[capture_index] = Some(CaptureRange {
                    start: part.start,
                    end: part.end,
                });
                capture_index += 1;
            }
        }
    }
    if parts.next().is_some() {
        return None;
    }
    Some(Params::from_match(
        capture_names,
        ranges,
        capture_index,
        path,
        materialize,
    ))
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
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8");
    if body.len() == 2 {
        builder = builder.header(header::CONTENT_LENGTH, HeaderValue::from_static("2"));
    } else {
        builder = builder.header(header::CONTENT_LENGTH, body.len());
    }
    builder.body(ResponseBody::full(body)).unwrap()
}

fn response_json<T: Serialize>(status: StatusCode, value: T) -> HttpResponse {
    response_json_bytes(
        status,
        Bytes::from(serde_json::to_vec(&value).unwrap_or_default()),
    )
}

fn response_json_bytes(status: StatusCode, body: Bytes) -> HttpResponse {
    let content_length = HeaderValue::from_str(&body.len().to_string()).unwrap();
    response_json_bytes_with_length(status, body, content_length)
}

fn response_json_bytes_with_length(
    status: StatusCode,
    body: Bytes,
    content_length: HeaderValue,
) -> HttpResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, content_length)
        .body(ResponseBody::full(body))
        .unwrap()
}

fn not_found() -> HttpResponse {
    response_text(StatusCode::NOT_FOUND, Bytes::from_static(b"Not Found"))
}

fn maybe_head(method: &Method, response: HttpResponse) -> HttpResponse {
    if method != Method::HEAD {
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

    #[test]
    fn dynamic_routes_use_a_compiled_method_trie() {
        let mut app = App::new();
        for index in 0..10_000 {
            let path = format!("/dynamic/{index}/{{id}}");
            app.get(&path, || async { "OK" });
        }

        let trie = app
            .dynamic_routes
            .get(&Method::GET)
            .expect("GET dynamic route trie");
        assert!(trie.node_count() < 20_010);
        assert_eq!(
            app.dynamic_route(&Method::GET, "/dynamic/9999/42"),
            Some(9_999)
        );
        assert_eq!(app.dynamic_route(&Method::GET, "/dynamic/missing/42"), None);
    }
}
