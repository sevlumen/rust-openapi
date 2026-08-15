use super::*;

pub trait FromRequest<S>: Sized + Send + 'static {
    const NEEDS_PARAMS: bool = false;
    /// Whether this extractor reads path captures, even without owned `Params`.
    const NEEDS_CAPTURE: bool = false;
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
        T::parse(request.uri().query().unwrap_or_default()).map(Query)
    }
}

impl<S: Send + Sync + 'static, T> FromRequest<S> for Json<T>
where
    T: DeserializeOwned + ApiSchema + Send + 'static,
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
        && subtype.as_bytes().ends_with(b"+json")
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
