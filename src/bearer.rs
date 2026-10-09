use crate::*;

type Validator = dyn Fn(String) -> BoxFuture<Result<(), ApiError>> + Send + Sync;

/// Middleware that requires `Authorization: Bearer <token>` and asks a
/// user-supplied async validator whether the token is acceptable.
///
/// It enforces authentication; declaring the scheme in the OpenAPI document is
/// separate (see `OpenApiOptions::bearer_auth`). Paths listed in
/// [`exempt_paths`](Self::exempt_paths) skip the check.
///
/// Every other request, including CORS preflight `OPTIONS` requests (which
/// browsers send without credentials), gets `401` unless a layer registered
/// before this one answers it. Compare secrets in constant time in the
/// validator: use [`constant_time_eq`], or [`BearerAuth::static_token`] for a
/// single shared token.
#[derive(Clone)]
#[must_use = "middleware does nothing until it is registered with `App::layer`"]
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

    /// Accepts exactly one token, compared with [`constant_time_eq`]. Handy
    /// for service-to-service calls; use [`new`](Self::new) when tokens are
    /// looked up or verified (JWT, database).
    pub fn static_token(token: impl Into<String>) -> Self {
        let expected: Arc<str> = Arc::from(token.into());
        Self::new(move |presented: String| {
            let accepted = constant_time_eq(presented.as_bytes(), expected.as_bytes());
            async move {
                if accepted {
                    Ok(())
                } else {
                    Err(ApiError::new(
                        StatusCode::UNAUTHORIZED,
                        "Unauthorized",
                        "a valid bearer token is required",
                    ))
                }
            }
        })
    }

    /// Exact paths (a trailing slash is ignored) that do not require a token.
    /// Paths must start with `/`. Calling this again replaces the list.
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

/// Compares two byte strings without stopping at the first difference, so the
/// time taken does not reveal how many leading bytes matched. The lengths are
/// not hidden: a different length returns `false` after a pass over the longer
/// input, which is the usual trade-off for tokens of a known size.
pub fn constant_time_eq(a: impl AsRef<[u8]>, b: impl AsRef<[u8]>) -> bool {
    let (a, b) = (a.as_ref(), b.as_ref());
    let mut diff = (a.len() ^ b.len()) as u64;
    for index in 0..a.len().max(b.len()) {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
        diff |= u64::from(x ^ y);
    }
    std::hint::black_box(diff) == 0
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

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn equal_inputs_match() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn any_difference_fails() {
        assert!(!constant_time_eq("secret", "secreT"));
        assert!(!constant_time_eq("secret", "secre"));
        assert!(!constant_time_eq("secret", "secret!"));
        assert!(!constant_time_eq("", "x"));
        // A zero byte suffix must not equal a shorter input.
        assert!(!constant_time_eq(b"ab\0", b"ab"));
    }
}
