//! `oas-rs`: a small, typed HTTP framework built on Hyper and Tokio.
//!
//! The runtime deliberately keeps OpenAPI generation out of request dispatch. The
//! document is assembled when routes are registered and is only serialized when
//! the explicitly registered OpenAPI endpoint is requested.

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap, convert::Infallible, future::Future, pin::Pin, str::FromStr, sync::Arc,
};

pub use http::Method;
pub use http::Method as HttpMethod;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
type HttpResponse = Response<Full<Bytes>>;

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
            .body(Full::new(Bytes::new()))
            .unwrap()
    }
}

/// A pre-serialized JSON response. The bytes are immutable and can be cloned
/// without re-running serde on each request.
#[derive(Clone, Debug)]
pub struct Json<T>(pub T);

impl<T: Serialize + Send + 'static> IntoResponse for Json<T> {
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

/// A JSON body serialized once at startup. Cloning this value only clones the
/// immutable `Bytes` handle, so the normal request path does not invoke serde.
#[derive(Clone, Debug)]
pub struct JsonBytes(pub Bytes);

impl JsonBytes {
    pub fn new(bytes: Bytes) -> Self {
        Self(bytes)
    }
}

impl IntoResponse for JsonBytes {
    fn into_response(self) -> HttpResponse {
        response_json_bytes(StatusCode::OK, self.0)
    }
}

/// A JSON response with the conventional `201 Created` status.
#[derive(Clone, Debug)]
pub struct Created<T>(pub T);

impl<T: Serialize + Send + 'static> IntoResponse for Created<T> {
    fn into_response(self) -> HttpResponse {
        let mut response = Json(self.0).into_response();
        *response.status_mut() = StatusCode::CREATED;
        response
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

impl<T: IntoResponse> IntoResponse for Result<T, ApiError> {
    fn into_response(self) -> HttpResponse {
        match self {
            Ok(value) => value.into_response(),
            Err(error) => error.into_response(),
        }
    }
}

/// A handler implementation is monomorphized at registration time and stored
/// as a single erased service only at the router boundary.
pub trait Handler<S, Args>: Send + Sync + 'static {
    fn call(
        &self,
        request: Request<Bytes>,
        params: Params,
        state: Arc<S>,
    ) -> BoxFuture<HttpResponse>;
}

pub trait FromRequest<S>: Sized + Send + 'static {
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
    T: FromStr + Send + 'static,
    T::Err: std::fmt::Display,
{
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
    T: DeserializeOwned + Send + 'static,
{
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
    T: DeserializeOwned + Send + 'static,
{
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

impl<S, F, Fut, R> Handler<S, ()> for F
where
    S: Send + Sync + 'static,
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    fn call(
        &self,
        _request: Request<Bytes>,
        _params: Params,
        _state: Arc<S>,
    ) -> BoxFuture<HttpResponse> {
        let future = (self)();
        Box::pin(async move { future.await.into_response() })
    }
}

impl<S, F, Fut, R, E1> Handler<S, (E1,)> for F
where
    S: Send + Sync + 'static,
    F: Fn(E1) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse,
    E1: FromRequest<S>,
{
    fn call(
        &self,
        mut request: Request<Bytes>,
        params: Params,
        state: Arc<S>,
    ) -> BoxFuture<HttpResponse> {
        let result = E1::from_request(&mut request, &params, &state);
        let handler = self.clone();
        Box::pin(async move {
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
    R: IntoResponse,
    E1: FromRequest<S>,
    E2: FromRequest<S>,
{
    fn call(
        &self,
        mut request: Request<Bytes>,
        params: Params,
        state: Arc<S>,
    ) -> BoxFuture<HttpResponse> {
        let first = E1::from_request(&mut request, &params, &state);
        let second = E2::from_request(&mut request, &params, &state);
        let handler = self.clone();
        Box::pin(async move {
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

type ErasedHandler<S> =
    Box<dyn Fn(Request<Bytes>, Params, Arc<S>) -> BoxFuture<HttpResponse> + Send + Sync>;

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
}

/// The application router and runtime.
pub struct App<S = ()> {
    state: Arc<S>,
    routes: Vec<Route<S>>,
    static_routes: HashMap<(Method, String), usize>,
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
            let parameters = route
                .segments
                .iter()
                .filter_map(|segment| match segment {
                    Segment::Capture(name) => Some(json!({
                        "in": "path",
                        "name": name,
                        "required": true,
                        "schema": if name == "id" { json!({"type":"string", "format":"uuid"}) } else { json!({"type":"string"}) }
                    })),
                    Segment::Static(_) => None,
                })
                .collect::<Vec<_>>();
            if !parameters.is_empty() {
                operation.insert("parameters".to_owned(), Value::Array(parameters));
            }
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
                .insert((method.clone(), template.clone()), index);
        }
        self.routes.push(Route {
            method,
            template,
            segments,
            handler: erased,
            operation: Operation::default(),
        });
        self.last_route = Some(index);
    }

    async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
        let method = request.method().clone();
        let path = normalize_path(request.uri().path());
        if self.openapi_path.as_deref() == Some(path.as_str()) && method == Method::GET {
            return response_json_bytes(
                StatusCode::OK,
                Bytes::from(self.openapi_document().to_string()),
            );
        }
        if self.swagger_path.as_deref() == Some(path.as_str()) && method == Method::GET {
            return response_text(StatusCode::OK, Bytes::from_static(SWAGGER_HTML.as_bytes()));
        }
        if method == Method::OPTIONS {
            let allow = self.allowed_methods(&path);
            return if allow.is_empty() {
                not_found()
            } else {
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .header(header::ALLOW, allow)
                    .body(Full::new(Bytes::new()))
                    .unwrap()
            };
        }
        if let Some(index) = self.static_routes.get(&(method.clone(), path.clone())) {
            return (self.routes[*index].handler)(
                request,
                Params::default(),
                Arc::clone(&self.state),
            )
            .await;
        }
        if method == Method::HEAD
            && let Some(index) = self.static_routes.get(&(Method::GET, path.clone()))
        {
            return (self.routes[*index].handler)(
                request,
                Params::default(),
                Arc::clone(&self.state),
            )
            .await;
        }
        for route in &self.routes {
            if route.method == method
                && let Some(params) = match_route(&route.segments, &path)
            {
                return (route.handler)(request, params, Arc::clone(&self.state)).await;
            }
        }
        if !self.allowed_methods(&path).is_empty() {
            return Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, self.allowed_methods(&path))
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
        not_found()
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
                Segment::Capture(part[1..part.len() - 1].to_owned())
            } else {
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
            Segment::Capture(name) => captures.push((name.clone(), part.to_owned())),
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

fn response_text(status: StatusCode, body: Bytes) -> HttpResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(body))
        .unwrap()
}

fn response_json<T: Serialize>(status: StatusCode, value: T) -> HttpResponse {
    response_json_bytes(
        status,
        Bytes::from(serde_json::to_vec(&value).unwrap_or_default()),
    )
}

fn response_json_bytes(status: StatusCode, body: Bytes) -> HttpResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(body))
        .unwrap()
}

fn not_found() -> HttpResponse {
    response_text(StatusCode::NOT_FOUND, Bytes::from_static(b"Not Found"))
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
