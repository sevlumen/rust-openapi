//! `oas-rs`: a small, typed HTTP framework built on Hyper and Tokio.
//!
//! The runtime deliberately keeps OpenAPI generation out of request dispatch. The
//! document is assembled when routes are registered and is only serialized when
//! the explicitly registered OpenAPI endpoint is requested.

use bytes::Bytes;
use futures_core::Stream;
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body::Body;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::Incoming;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    borrow::Cow,
    collections::HashMap,
    convert::Infallible,
    future::{self, Future},
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
};

mod app;
mod extract;
mod handler;
mod openapi;
mod response;
mod router;
mod runtime;
pub use app::App;
pub use extract::FromRequest;
use handler::{BoxFuture, HandlerFuture};
#[cfg(test)]
use handler::{HandlerFutureKind, INLINE_FUTURE_SIZE, InlineFuture};
use openapi::OpenApiConfig;
#[cfg(any(test, feature = "swagger"))]
use openapi::SwaggerConfig;
#[cfg(any(test, feature = "swagger"))]
pub use openapi::SwaggerOptions;
pub use openapi::{BuildError, OpenApiOptions};
pub use response::ResponseBody;
#[doc(hidden)]
pub use router::ErasedZeroHandler;
use router::{
    BodyMode, CaptureMode, CaptureProvider, CaptureSet, DynamicCaptures, DynamicRouteTrie,
    HandlerKind, RouteFailure, RouteId, RouteMetadata, RoutePlan, RouteSet, Segment,
    StaticCaptures, StaticResponse, resolve_route_set,
};
#[cfg(test)]
use router::{DynamicRouteNode, NodeId};
pub use runtime::AppRuntime;
#[cfg(test)]
use runtime::ConnectionRuntime;
#[cfg(any(test, feature = "test-util"))]
pub use runtime::TestResponse;

pub use http::Method;
pub use oas_rs_macros::ApiSchema;

#[doc(hidden)]
pub mod __private {
    pub use crate::{OpenApiQuery, decode_query_component, parse_query_value};
    pub use serde_json;
}

pub type HttpResponse = Response<ResponseBody>;

/// Default upper bound for request bodies collected by body extractors.
pub const DEFAULT_MAX_BODY_SIZE: usize = 1024 * 1024;

fn encode_body_limit(limit: usize) -> u32 {
    assert!(limit > 0, "max_body_size must be greater than zero");
    assert!(
        limit <= u32::MAX as usize,
        "max_body_size must fit in a u32"
    );
    limit as u32
}

const MAX_CAPTURE_PARAMS: usize = 8;

#[derive(Clone, Copy, Debug)]
struct CaptureRange {
    start: usize,
    end: usize,
}

/// A path capture collection. Typed extractors use ranges into the request
/// URI, while the explicit `Params` extractor opts into owned decoded values.
#[derive(Debug)]
struct MaterializedParams {
    names: Arc<[String]>,
    values: Box<[String]>,
}

type OwnedParams = Arc<MaterializedParams>;

#[derive(Clone, Debug)]
pub struct Params {
    packed: [u64; MAX_CAPTURE_PARAMS],
    count: u8,
    owned: Option<OwnedParams>,
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
        let owned = self.owned.as_ref()?;
        owned
            .names
            .iter()
            .zip(owned.values.iter())
            .find(|(key, _)| key.as_str() == name)
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
            Self::materialized_names(
                Arc::from(names.to_owned().into_boxed_slice()),
                &captures,
                path,
            )
            .expect("materialized capture values are valid")
        });
        Self {
            packed: captures.packed,
            count: captures.count,
            owned,
        }
    }

    fn from_materialized_names(
        names: Arc<[String]>,
        captures: CaptureSet,
        path: &str,
    ) -> Result<Self, ApiError> {
        Ok(Self {
            packed: captures.packed,
            count: captures.count,
            owned: Some(Self::materialized_names(names, &captures, path)?),
        })
    }

    fn materialized_names(
        names: Arc<[String]>,
        captures: &CaptureSet,
        path: &str,
    ) -> Result<OwnedParams, ApiError> {
        let mut values = Vec::with_capacity(captures.count as usize);
        for index in 0..captures.count as usize {
            let range = captures.range(index).expect("capture range exists");
            values.push(percent_decode(&path[range.start..range.end])?);
        }
        Ok(Arc::new(MaterializedParams {
            names,
            values: values.into_boxed_slice(),
        }))
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
    const NEEDS_CAPTURE: bool = true;

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
    const HEADER_NAME: header::HeaderName = header::HeaderName::from_static(Self::NAME);
    fn parse(value: &str) -> Result<Self, ApiError>;
}

/// The small set of type schemas that can be inferred without runtime
/// reflection. Applications can implement this trait for their own scalar
/// path types.
pub trait ApiSchema {
    fn schema() -> Value;
}

#[doc(hidden)]
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

#[doc(hidden)]
pub fn parse_query_value<T: QueryValue>(value: &str) -> Result<T, ApiError> {
    T::parse_query_value(value)
}

impl ApiSchema for String {
    fn schema() -> Value {
        json!({ "type": "string" })
    }
}

impl ApiSchema for u32 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int32" })
    }
}

impl ApiSchema for u64 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int64" })
    }
}

impl ApiSchema for i32 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int32" })
    }
}

impl ApiSchema for i64 {
    fn schema() -> Value {
        json!({ "type": "integer", "format": "int64" })
    }
}

impl ApiSchema for bool {
    fn schema() -> Value {
        json!({ "type": "boolean" })
    }
}

#[cfg(feature = "uuid")]
impl ApiSchema for uuid::Uuid {
    fn schema() -> Value {
        json!({ "type": "string", "format": "uuid" })
    }
}

impl<T: ApiSchema> ApiSchema for Option<T> {
    fn schema() -> Value {
        T::schema()
    }
}

impl<T: ApiSchema> ApiSchema for Vec<T> {
    fn schema() -> Value {
        json!({ "type": "array", "items": T::schema() })
    }
}

impl<T: ApiSchema + ?Sized> ApiSchema for &T {
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
    /// Construct a missing-input error that `Option<T>` extractors convert to
    /// `None`. Custom extractors should use this only when the input is absent;
    /// malformed present input must return [`ApiError::bad_request`] instead.
    pub fn missing(detail: impl Into<String>) -> Self {
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
    /// Whether any extractor reads path captures from the router.
    const NEEDS_CAPTURE: bool = false;
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
    const NEEDS_CAPTURE: bool = E1::NEEDS_CAPTURE;
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
            const NEEDS_CAPTURE: bool =
                <$first as FromRequest<S>>::NEEDS_CAPTURE
                $(|| <$rest as FromRequest<S>>::NEEDS_CAPTURE)*;
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

#[derive(Clone, Default)]
struct Operation {
    tag: Option<String>,
    summary: Option<String>,
    operation_id: Option<String>,
    response_status: StatusCode,
    response_schema: Option<Value>,
    request: OpenApiRequest,
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
fn payload_too_large_response() -> HttpResponse {
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

#[doc(hidden)]
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

#[cfg(any(test, feature = "swagger"))]
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
    use std::{marker::PhantomPinned, mem::size_of, task::Waker};

    type BoxedPathHandler =
        Box<dyn Fn(Path<u64>) -> Pin<Box<dyn Future<Output = &'static str> + Send>> + Send + Sync>;

    fn block_on_without_io<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
        }
    }

    #[tokio::test]
    async fn openapi_document_is_prepared_once_for_server_dispatch() {
        let mut app = App::new();
        app.get("/plaintext", || async { "OK" });
        app.openapi();
        app.swagger().path("/swagger");
        assert!(app.openapi_bytes.is_none());
        assert!(app.swagger_bytes.is_none());

        let runtime = app.build().expect("test app builds");
        let response = runtime
            .oneshot(Method::GET, "/openapi.json", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.body_string().await;
        assert!(body.contains("/plaintext"));

        let mut updated = App::new();
        updated.get("/plaintext", || async { "OK" });
        updated.openapi().path("/spec.json");
        updated.swagger().path("/swagger");
        let runtime = updated.build().expect("updated app builds");
        let response = runtime.oneshot(Method::GET, "/swagger", &[], None).await;
        let swagger_body = response.body_string().await;
        assert!(
            swagger_body.contains("/spec.json"),
            "Swagger cache did not follow the updated OpenAPI path"
        );
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

        let response = app
            .build()
            .expect("test app builds")
            .oneshot(Method::GET, "/zero", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body_string().await, "OK");
    }

    #[tokio::test]
    async fn typed_registration_does_not_require_a_cloneable_handler() {
        let handler: BoxedPathHandler = Box::new(|Path(_id)| {
            Box::pin(async { "OK" }) as Pin<Box<dyn Future<Output = &'static str> + Send>>
        });
        let mut app = App::new();
        app.get("/non-clone/{id}", handler);

        let response = app
            .build()
            .expect("test app builds")
            .oneshot(Method::GET, "/non-clone/42", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn runtime_keeps_hot_plans_separate_from_cold_metadata() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });
        app.openapi();

        assert_eq!(app.plans.len(), 1);
        assert_eq!(app.metadata.len(), 1);
    }

    #[test]
    fn build_freezes_route_storage_into_boxed_slices() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });

        let runtime = app.build().expect("test app builds");

        assert_eq!(runtime.plans.len(), 1);
    }

    #[test]
    fn app_builder_is_available_as_the_registration_type() {
        let mut builder = App::new();
        builder.get("/zero", || async { "OK" });
        assert_eq!(builder.plans.len(), 1);
    }

    #[test]
    fn connection_runtime_reuses_one_runtime_owner_for_request_borrows() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });
        let runtime = Arc::new(app.build().expect("test app builds"));
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
        assert!(Arc::ptr_eq(
            &params.owned.as_ref().expect("materialized values").names,
            &cloned.owned.as_ref().expect("materialized values").names,
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
        assert_eq!(app.plans[1].body_limit, DEFAULT_MAX_BODY_SIZE as u32);
        assert!(matches!(app.plans[2].body_mode, BodyMode::Incoming));
    }

    #[test]
    fn buffered_body_limit_is_compiled_into_route_plans() {
        let mut app = App::new();
        app.post("/json", |Json(body): Json<String>| async move { body });
        app.raw_get("/upload", |_request| async { "OK" });

        app.max_body_size(32);
        assert_eq!(app.plans[0].body_limit, 32);
        assert_eq!(app.plans[1].body_limit, 0);

        app.max_body_size(64);
        assert_eq!(app.plans[0].body_limit, 64);
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
    fn dynamic_trie_restores_captures_after_failed_static_branch() {
        let mut app = App::new();
        app.get("/files/static/other", || async { "static" });
        app.get("/files/{id}/tail", || async { "capture" });

        let matched = app
            .dynamic_routes
            .find("/files/static/tail")
            .expect("capture fallback should match");
        assert_eq!(matched.captures.count, 1);
        assert_eq!(matched.captures.range(0).unwrap().start, 7);
        assert_eq!(matched.captures.range(0).unwrap().end, 13);
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

    #[test]
    fn inline_future_preserves_pinned_application_future() {
        struct PinnedReady {
            _pin: PhantomPinned,
        }

        impl Future for PinnedReady {
            type Output = &'static str;

            fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
                Poll::Ready("OK")
            }
        }

        let response = block_on_without_io(HandlerFuture::from_response_future(PinnedReady {
            _pin: PhantomPinned,
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            block_on_without_io(response.into_body().collect())
                .unwrap()
                .to_bytes(),
            Bytes::from_static(b"OK")
        );
    }

    #[test]
    fn inline_future_polls_a_pinned_pending_future_without_moving_it() {
        struct PinnedPending {
            polled: bool,
            _pin: PhantomPinned,
        }

        impl Future for PinnedPending {
            type Output = &'static str;

            fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
                // Accessing a field through a pinned, !Unpin future is safe as
                // long as the future itself is never moved after pinning.
                let this = unsafe { self.get_unchecked_mut() };
                if this.polled {
                    Poll::Ready("OK")
                } else {
                    this.polled = true;
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }

        let response = block_on_without_io(HandlerFuture::from_response_future(PinnedPending {
            polled: false,
            _pin: PhantomPinned,
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            block_on_without_io(response.into_body().collect())
                .unwrap()
                .to_bytes(),
            Bytes::from_static(b"OK")
        );
    }

    #[test]
    fn inline_future_uses_heap_fallback_for_oversized_application_future() {
        struct OversizedFuture {
            bytes: [u8; INLINE_FUTURE_SIZE + 1],
        }

        impl Future for OversizedFuture {
            type Output = &'static str;

            fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
                let _ = self.bytes[0];
                Poll::Ready("OK")
            }
        }

        let future = HandlerFuture::from_response_future(OversizedFuture {
            bytes: [0; INLINE_FUTURE_SIZE + 1],
        });
        assert!(matches!(&future.0, HandlerFutureKind::Boxed(_)));

        let response = block_on_without_io(future);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            block_on_without_io(response.into_body().collect())
                .unwrap()
                .to_bytes(),
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

    #[tokio::test]
    async fn typed_no_params_dynamic_route_preserves_header_extraction() {
        struct Trace;

        impl HeaderSpec for Trace {
            const NAME: &'static str = "x-trace";

            fn parse(_value: &str) -> Result<Self, ApiError> {
                Ok(Self)
            }
        }

        let mut app = App::new();
        app.get("/trace/{id}", |Header(_): Header<Trace>| async { "OK" });

        let matched = app
            .dynamic_routes
            .find("/trace/abc")
            .expect("dynamic route should match");
        let index = matched
            .routes
            .route(&Method::GET)
            .expect("dynamic route should resolve");
        assert!(matches!(
            app.plans[index.index()].handler,
            HandlerKind::TypedNoParams(_)
        ));

        let response = app
            .oneshot(Method::GET, "/trace/abc", &[("x-trace", "1")], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
