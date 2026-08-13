//! `oas-rs`: a small, typed HTTP framework built on Hyper and Tokio.
//!
//! The runtime deliberately keeps OpenAPI generation out of request dispatch. The
//! document is assembled when routes are registered and is only serialized when
//! the explicitly registered OpenAPI endpoint is requested.

use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    future::Future,
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
type HttpResponse = Response<Full<Bytes>>;

const INLINE_FUTURE_SIZE: usize = 128;

#[repr(align(64))]
struct FutureStorage([MaybeUninit<u8>; INLINE_FUTURE_SIZE]);

struct InlineFuture {
    storage: FutureStorage,
    poll_fn: unsafe fn(*mut u8, &mut Context<'_>) -> Poll<HttpResponse>,
    drop_fn: unsafe fn(*mut u8),
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

/// A path capture collection. Values are owned at the dispatch boundary so an
/// extractor can safely outlive the router's matching call.
#[derive(Clone, Debug, Default)]
pub struct Params(Vec<(String, String)>);

impl Params {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

impl<S: Send + Sync + 'static> FromRequest<S> for Params {
    fn from_request(
        _request: &mut Request<Bytes>,
        params: &Params,
        _state: &Arc<S>,
    ) -> BoxFuture<Result<Self, ApiError>> {
        let params = params.clone();
        Box::pin(async move { Ok(params) })
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

pub trait OpenApiQuery {
    fn parameters() -> Vec<Value>;
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
}

impl ApiError {
    pub fn new(status: StatusCode, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status,
            title: title.into(),
            detail: detail.into(),
        }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "Bad Request", detail)
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
            .body(Full::new(Bytes::new()))
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

/// A JSON response with the conventional `201 Created` status.
#[derive(Clone, Debug)]
pub struct Created<T>(pub T);

impl<T: Serialize + OpenApiSchema + Send + 'static> IntoResponse for Created<T> {
    fn into_response(self) -> HttpResponse {
        let mut response = Json(self.0).into_response();
        *response.status_mut() = StatusCode::CREATED;
        response
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
            .body(Full::new(Bytes::new()))
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

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn call(&self, request: Request<Bytes>, params: Params, state: Arc<S>) -> HandlerFuture;
}

pub trait FromRequest<S>: Sized + Send + 'static {
    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    fn from_request(
        request: &mut Request<Bytes>,
        params: &Params,
        state: &Arc<S>,
    ) -> BoxFuture<Result<Self, ApiError>>;
}

impl<S: Send + Sync + 'static> FromRequest<S> for State<S> {
    fn from_request(
        _request: &mut Request<Bytes>,
        _params: &Params,
        state: &Arc<S>,
    ) -> BoxFuture<Result<Self, ApiError>> {
        let state = Arc::clone(state);
        Box::pin(async move { Ok(State(state)) })
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
        _request: &mut Request<Bytes>,
        params: &Params,
        _state: &Arc<S>,
    ) -> BoxFuture<Result<Self, ApiError>> {
        let value = params.0.first().map(|(_, value)| value.clone());
        Box::pin(async move {
            let value = value.ok_or_else(|| ApiError::bad_request("missing path parameter"))?;
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
    ) -> BoxFuture<Result<Self, ApiError>> {
        let query = request.uri().query().unwrap_or_default().to_owned();
        Box::pin(async move { parse_query(&query).map(Query) })
    }
}

impl<S: Send + Sync + 'static, T> FromRequest<S> for Json<T>
where
    T: DeserializeOwned + OpenApiSchema + Send + 'static,
{
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
    ) -> BoxFuture<Result<Self, ApiError>> {
        let body = request.body().clone();
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Box::pin(async move {
            if !content_type
                .as_deref()
                .unwrap_or("")
                .starts_with("application/json")
            {
                return Err(ApiError::new(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "Unsupported Media Type",
                    "expected application/json",
                ));
            }
            serde_json::from_slice(&body)
                .map(Json)
                .map_err(|error| ApiError::bad_request(error.to_string()))
        })
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
    ) -> BoxFuture<Result<Self, ApiError>> {
        let value = request
            .headers()
            .get(T::NAME)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Box::pin(async move {
            let value = value
                .ok_or_else(|| ApiError::bad_request(format!("missing header {}", T::NAME)))?;
            T::parse(&value).map(Header)
        })
    }
}

impl<S, T> FromRequest<S> for Option<T>
where
    S: Send + Sync + 'static,
    T: FromRequest<S>,
{
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
    ) -> BoxFuture<Result<Self, ApiError>> {
        let future = T::from_request(request, params, state);
        Box::pin(async move { Ok(future.await.ok()) })
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

    fn openapi_request() -> OpenApiRequest {
        E1::openapi_request()
    }

    fn call(&self, mut request: Request<Bytes>, params: Params, state: Arc<S>) -> HandlerFuture {
        let result = E1::from_request(&mut request, &params, &state);
        let handler = self.clone();
        HandlerFuture::from_future(async move {
            match result.await {
                Ok(value) => handler(value).await.into_response(),
                Err(error) => error.into_response(),
            }
        })
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

    fn openapi_request() -> OpenApiRequest {
        let mut metadata = E1::openapi_request();
        metadata.merge(E2::openapi_request());
        metadata
    }

    fn call(&self, mut request: Request<Bytes>, params: Params, state: Arc<S>) -> HandlerFuture {
        let first = E1::from_request(&mut request, &params, &state);
        let second = E2::from_request(&mut request, &params, &state);
        let handler = self.clone();
        HandlerFuture::from_future(async move {
            let first = match first.await {
                Ok(value) => value,
                Err(error) => return error.into_response(),
            };
            match second.await {
                Ok(value) => handler(first, value).await.into_response(),
                Err(error) => error.into_response(),
            }
        })
    }
}

type ErasedHandler<S> = Box<dyn Fn(Request<Bytes>, Params, Arc<S>) -> HandlerFuture + Send + Sync>;

struct Route<S> {
    method: Method,
    template: String,
    segments: Vec<Segment>,
    handler: ErasedHandler<S>,
    operation: Operation,
}

#[derive(Clone)]
enum Segment {
    Static(String),
    Capture(String),
}

#[derive(Clone, Default)]
struct Operation {
    tag: Option<String>,
    summary: Option<String>,
    response_status: StatusCode,
    response_schema: Option<Value>,
    request: OpenApiRequest,
}

/// The application router and runtime.
pub struct App<S = ()> {
    state: Arc<S>,
    routes: Vec<Route<S>>,
    static_routes: HashMap<String, Vec<(Method, usize)>>,
    last_route: Option<usize>,
    openapi_path: Option<String>,
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
            last_route: None,
            openapi_path: None,
            swagger_path: None,
            title: "oas-rs API".to_owned(),
            version: "0.1.0".to_owned(),
        }
    }
}

impl Default for App<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Send + Sync + 'static> App<S> {
    pub fn with_state<T: Send + Sync + 'static>(self, state: T) -> App<T> {
        App {
            state: Arc::new(state),
            routes: Vec::new(),
            static_routes: HashMap::new(),
            last_route: None,
            openapi_path: self.openapi_path,
            swagger_path: self.swagger_path,
            title: self.title,
            version: self.version,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
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

    pub fn tag(&mut self, tag: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.routes[index].operation.tag = Some(tag.into());
        }
        self
    }

    pub fn summary(&mut self, summary: impl Into<String>) -> &mut Self {
        if let Some(index) = self.last_route {
            self.routes[index].operation.summary = Some(summary.into());
        }
        self
    }

    pub fn openapi(&mut self, path: &str) -> &mut Self {
        self.openapi_path = Some(normalize_path(path));
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
        let app = Arc::new(self);
        loop {
            let (stream, _) = listener.accept().await?;
            let app = Arc::clone(&app);
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                    let app = Arc::clone(&app);
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body
                            .collect()
                            .await
                            .map(|body| body.to_bytes())
                            .unwrap_or_default();
                        Ok::<_, Infallible>(app.handle(Request::from_parts(parts, body)).await)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
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
        }
        self.routes.push(Route {
            method,
            template,
            segments,
            handler: erased,
            operation: Operation {
                tag: None,
                summary: None,
                response_status: <H::Response as ResponseMetadata>::status_code(),
                response_schema: <H::Response as ResponseMetadata>::response_schema(),
                request: H::openapi_request(),
            },
        });
        self.last_route = Some(index);
    }

    async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
        let method = request.method().clone();
        let path = normalize_request_path(request.uri().path());
        if self.openapi_path.as_deref() == Some(path) && method == Method::GET {
            return response_json_bytes(
                StatusCode::OK,
                Bytes::from(self.openapi_document().to_string()),
            );
        }
        if self.swagger_path.as_deref() == Some(path) && method == Method::GET {
            return response_text(StatusCode::OK, Bytes::from_static(SWAGGER_HTML.as_bytes()));
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
                    .body(Full::new(Bytes::new()))
                    .unwrap()
            };
        }
        if let Some(index) = self.static_route(&method, path) {
            return maybe_head(
                &method,
                (self.routes[index].handler)(request, Params::default(), Arc::clone(&self.state))
                    .await,
            )
            .await;
        }
        if method == Method::HEAD
            && let Some(index) = self.static_route(&Method::GET, path)
        {
            return maybe_head(
                &method,
                (self.routes[index].handler)(request, Params::default(), Arc::clone(&self.state))
                    .await,
            )
            .await;
        }
        for route in &self.routes {
            if route.method == method
                && let Some(params) = match_route(&route.segments, path)
            {
                return maybe_head(
                    &method,
                    (route.handler)(request, params, Arc::clone(&self.state)).await,
                )
                .await;
            }
        }
        if method == Method::HEAD {
            for route in &self.routes {
                if route.method == Method::GET
                    && let Some(params) = match_route(&route.segments, path)
                {
                    return maybe_head(
                        &method,
                        (route.handler)(request, params, Arc::clone(&self.state)).await,
                    )
                    .await;
                }
            }
        }
        if !self.allowed_methods(path).is_empty() {
            return Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, self.allowed_methods(path))
                .header(header::CONTENT_LENGTH, "0")
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
        not_found()
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

    fn allowed_methods(&self, path: &str) -> String {
        let mut methods = Vec::new();
        for route in &self.routes {
            if match_route(&route.segments, path).is_some()
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

fn match_route(segments: &[Segment], path: &str) -> Option<Params> {
    let parts = split_path(path);
    if parts.len() != segments.len() {
        return None;
    }
    let mut captures = Vec::new();
    for (segment, part) in segments.iter().zip(parts) {
        match segment {
            Segment::Static(expected) if expected != part => return None,
            Segment::Static(_) => {}
            Segment::Capture(name) => captures.push((name.clone(), percent_decode(part).ok()?)),
        }
    }
    Some(Params(captures))
}

fn split_path(path: &str) -> Vec<&str> {
    if path == "/" {
        Vec::new()
    } else {
        path.trim_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
            .collect()
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
    builder.body(Full::new(body)).unwrap()
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
        .body(Full::new(body))
        .unwrap()
}

fn not_found() -> HttpResponse {
    response_text(StatusCode::NOT_FOUND, Bytes::from_static(b"Not Found"))
}

async fn maybe_head(method: &Method, response: HttpResponse) -> HttpResponse {
    if method != Method::HEAD {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let body = body
        .collect()
        .await
        .map(|body| body.to_bytes())
        .unwrap_or_default();
    parts.headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&body.len().to_string()).unwrap(),
    );
    Response::from_parts(parts, Full::new(Bytes::new()))
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
    let mut output = String::with_capacity(value.len());
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
            output.push((high * 16 + low) as char);
            index += 3;
        } else {
            output.push(bytes[index] as char);
            index += 1;
        }
    }
    Ok(output)
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

const SWAGGER_HTML: &str = r##"<!doctype html><html><head><title>Swagger UI</title></head><body><div id="swagger-ui">Swagger UI</div><script>fetch('/openapi.json').then(r=>r.json()).then(d=>document.getElementById('swagger-ui').textContent=JSON.stringify(d))</script></body></html>"##;
