use super::*;

pub trait FromRequest<S>: Sized + Send + 'static {
    const NEEDS_PARAMS: bool = false;
    /// Whether this extractor reads path captures, even without owned `Params`.
    const NEEDS_CAPTURE: bool = false;
    const NEEDS_BODY: bool = false;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest::default()
    }

    /// Like [`openapi_request`](Self::openapi_request), but named schemas go
    /// into `registry` and are referenced with `$ref`.
    fn openapi_request_with(registry: &mut SchemaRegistry) -> OpenApiRequest {
        let _ = registry;
        Self::openapi_request()
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
    T: FromStr + ApiSchema + Send + 'static,
    T::Err: std::fmt::Display,
{
    const NEEDS_CAPTURE: bool = true;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest {
            path_schemas: vec![<T as ApiSchema>::schema()],
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
        let query = request.uri().query();
        T::parse(query.unwrap_or_default())
            .map(Query)
            // Nothing was sent at all: report it as "missing" so an
            // `Option<Query<T>>` handler gets `None` instead of a `400`.
            .map_err(|error| {
                if query.is_none_or(str::is_empty) {
                    error.into_missing()
                } else {
                    error
                }
            })
    }
}

impl<S: Send + Sync + 'static, T> FromRequest<S> for Json<T>
where
    T: DeserializeOwned + ApiSchema + Send + 'static,
{
    const NEEDS_BODY: bool = true;

    fn openapi_request() -> OpenApiRequest {
        <Self as FromRequest<S>>::openapi_request_with(&mut SchemaRegistry::inline())
    }

    fn openapi_request_with(registry: &mut SchemaRegistry) -> OpenApiRequest {
        OpenApiRequest {
            request_body: Some(json!({
                "required": true,
                "content": {
                    "application/json": {
                        "schema": T::schema_with(registry)
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
        let body = request.body().as_ref();
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
        if !is_json_media_type(media_type) {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Media Type",
                "expected application/json or application/*+json",
            ));
        }
        serde_json::from_slice(body)
            .map(Json)
            .map_err(|error| ApiError::bad_request(error.to_string()))
    }
}

fn is_json_media_type(media_type: &str) -> bool {
    if media_type.eq_ignore_ascii_case("application/json") {
        return true;
    }
    let Some((media_type, subtype)) = media_type.split_once('/') else {
        return false;
    };
    media_type.eq_ignore_ascii_case("application")
        && subtype.len() > b"+json".len()
        && subtype.as_bytes()[subtype.len() - 5..].eq_ignore_ascii_case(b"+json")
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
            .get(&T::HEADER_NAME)
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
    const NEEDS_CAPTURE: bool = T::NEEDS_CAPTURE;
    const NEEDS_BODY: bool = T::NEEDS_BODY;

    fn openapi_request() -> OpenApiRequest {
        <Self as FromRequest<S>>::openapi_request_with(&mut SchemaRegistry::inline())
    }

    fn openapi_request_with(registry: &mut SchemaRegistry) -> OpenApiRequest {
        let mut metadata = T::openapi_request_with(registry);
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

/// The remote address of the connection the request arrived on. Needs
/// [`AppRuntime::connect_info`](crate::AppRuntime::connect_info)`(true)`;
/// otherwise the extractor fails the request with a `500` that says so (a
/// setup mistake, not a client error). Behind a proxy this is the proxy's
/// address. Unix-socket connections and `oneshot` requests have none.
#[derive(Clone, Copy, Debug)]
pub struct ConnectInfo(pub std::net::SocketAddr);

/// The peer address recorded for `request`, if
/// [`AppRuntime::connect_info`](crate::AppRuntime::connect_info) is on.
pub fn peer_addr<B>(request: &Request<B>) -> Option<std::net::SocketAddr> {
    request
        .extensions()
        .get::<crate::runtime::PeerAddr>()
        .map(|peer| peer.0)
}

impl<S: Send + Sync + 'static> FromRequest<S> for ConnectInfo {
    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        peer_addr(request).map(ConnectInfo).ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "ConnectInfo needs AppRuntime::connect_info(true) and a TCP listener",
            )
        })
    }
}

/// All request headers, for code that reads many or dynamic headers. Cloning
/// the map allocates a few times per request that extracts it, so prefer
/// [`Header`] for one or two known headers. It documents no OpenAPI
/// parameters.
///
/// `{:?}` shows every header name but only the values of a short list of
/// harmless headers (`Host`, `User-Agent`, `Content-Type`, `Accept*`,
/// `X-Request-Id`, ...); every other value prints as `<redacted>` (default
/// deny): `Authorization`, `Cookie`, custom credential headers, and also
/// `Referer`, `Sec-WebSocket-Protocol` and the forwarded-address headers, which
/// can carry tokens or personal data. Logging it therefore does not leak them.
/// The map itself (`.0`) is unchanged.
#[derive(Clone)]
pub struct Headers(pub http::HeaderMap);

impl std::fmt::Debug for Headers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut map = f.debug_map();
        for (name, value) in &self.0 {
            if !crate::debug_impls::is_safe_header(name.as_str()) {
                map.entry(&name.as_str(), &crate::debug_impls::Redacted);
            } else {
                map.entry(&name.as_str(), value);
            }
        }
        map.finish()
    }
}

impl<S: Send + Sync + 'static> FromRequest<S> for Headers {
    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        Ok(Headers(request.headers().clone()))
    }
}

pub trait HeaderSpec: Sized + Send + 'static {
    const NAME: &'static str;
    const HEADER_NAME: header::HeaderName = header::HeaderName::from_static(Self::NAME);
    fn parse(value: &str) -> Result<Self, ApiError>;
}
