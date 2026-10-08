use crate::*;

type Validator = dyn Fn(String) -> BoxFuture<Result<(), ApiError>> + Send + Sync;

/// Middleware that requires `Authorization: Bearer <token>` and asks a
/// user-supplied async validator whether the token is acceptable.
///
/// It enforces authentication; declaring the scheme in the OpenAPI document is
/// separate (see `OpenApiOptions::bearer_auth`). Paths listed in
/// [`exempt_paths`](Self::exempt_paths) skip the check.
#[derive(Clone)]
pub struct BearerAuth {
    validator: Arc<Validator>,
    exempt: Arc<[String]>,
}

impl BearerAuth {
    pub fn new<F, Fut>(validator: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        Self {
            validator: Arc::new(move |token| Box::pin(validator(token))),
            exempt: Arc::from(Vec::new()),
        }
    }

    /// Exact paths (a trailing slash is ignored) that do not require a token.
    pub fn exempt_paths<I, T>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.exempt = paths
            .into_iter()
            .map(|path| normalize_path(&path.into()))
            .collect();
        self
    }
}

fn unauthorized() -> HttpResponse {
    let mut response = ApiError::new(
        StatusCode::UNAUTHORIZED,
        "Unauthorized",
        "a valid bearer token is required",
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn bearer_token(value: &HeaderValue) -> Option<String> {
    let value = value.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_owned())
}

impl Middleware for BearerAuth {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let path = normalize_request_path(request.uri().path());
        if self.exempt.iter().any(|exempt| exempt == path) {
            return next.run(request);
        }
        let Some(token) = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(bearer_token)
        else {
            return Box::pin(async { unauthorized() });
        };
        let validator = Arc::clone(&self.validator);
        Box::pin(async move {
            match validator(token).await {
                Ok(()) => next.run(request).await,
                Err(error) => error.into_response(),
            }
        })
    }
}
