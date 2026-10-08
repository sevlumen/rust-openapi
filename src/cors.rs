use http::header::{self, HeaderName};

use crate::*;

#[derive(Clone)]
enum AllowedHeaders {
    List(Vec<HeaderName>),
    /// Answer a preflight with whatever headers it asks for.
    Mirror,
}

#[derive(Clone)]
struct CorsConfig {
    any_origin: bool,
    origins: Vec<String>,
    methods: Vec<Method>,
    headers: AllowedHeaders,
    credentials: bool,
    expose: Vec<HeaderName>,
    max_age: Option<Duration>,
}

/// Cross-origin resource sharing for browsers.
///
/// Answers preflight requests (`OPTIONS` with `Origin` and
/// `Access-Control-Request-Method`) itself and adds the `Access-Control-*`
/// headers to responses for allowed origins, including error responses.
///
/// Layers run before routing in registration order, so register `Cors`
/// **before** authentication layers such as `BearerAuth`: browsers send
/// preflights without credentials and the preflight must not reach the auth
/// check. A request from an origin that is not allowed is still served; it
/// just gets no CORS headers, so the browser refuses to expose the response.
///
/// By default nothing is allowed: add origins with
/// [`allow_origin`](Self::allow_origin) or [`allow_any_origin`](Self::allow_any_origin).
#[derive(Clone)]
pub struct Cors {
    config: Arc<CorsConfig>,
}

impl Cors {
    pub fn new() -> Self {
        Self {
            config: Arc::new(CorsConfig {
                any_origin: false,
                origins: Vec::new(),
                methods: vec![Method::GET, Method::HEAD, Method::POST],
                headers: AllowedHeaders::List(vec![header::CONTENT_TYPE]),
                credentials: false,
                expose: Vec::new(),
                max_age: None,
            }),
        }
    }

    fn edit(mut self, change: impl FnOnce(&mut CorsConfig)) -> Self {
        change(Arc::make_mut(&mut self.config));
        self
    }

    /// Allows one origin, compared exactly (`https://app.example`, no
    /// trailing slash). Can be called repeatedly. Avoid `"null"` together with
    /// credentials: sandboxed frames and `file://` pages send it.
    ///
    /// # Panics
    ///
    /// Panics unless `origin` is `scheme://host[:port]` (use
    /// [`allow_any_origin`](Self::allow_any_origin) instead of `*`), so a
    /// typo cannot silently allow nothing.
    pub fn allow_origin(self, origin: impl Into<String>) -> Self {
        let origin = origin.into();
        let valid = origin == "null"
            || origin.split_once("://").is_some_and(|(scheme, authority)| {
                !scheme.is_empty()
                    && !authority.is_empty()
                    && !authority.contains(['/', '*', ' ', '?', '#'])
            });
        assert!(
            valid,
            "invalid CORS origin {origin:?}: expected scheme://host[:port] (use allow_any_origin for *)"
        );
        self.edit(|config| config.origins.push(origin))
    }

    /// Allows every origin; responses carry `Access-Control-Allow-Origin: *`.
    ///
    /// # Panics
    ///
    /// Panics if credentials are already allowed: browsers reject `*` with
    /// credentials, so list the origins instead.
    pub fn allow_any_origin(self) -> Self {
        assert!(
            !self.config.credentials,
            "allow_any_origin cannot be combined with credentials; list the origins instead"
        );
        self.edit(|config| config.any_origin = true)
    }

    /// The methods a preflight may ask for (default `GET`, `HEAD`, `POST`);
    /// this replaces the default and matches the method name exactly.
    pub fn allow_methods(self, methods: impl IntoIterator<Item = Method>) -> Self {
        let methods: Vec<Method> = methods.into_iter().collect();
        self.edit(|config| config.methods = methods)
    }

    /// The request headers a preflight may ask for, replacing the default
    /// (`content-type`, which browsers send for a JSON body). Names are
    /// matched without regard to case; add `authorization` here for bearer
    /// tokens.
    ///
    /// # Panics
    ///
    /// Panics on an invalid header name.
    pub fn allow_headers<I, T>(self, headers: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let headers = headers
            .into_iter()
            .map(|name| header_name(name.as_ref()))
            .collect();
        self.edit(|config| config.headers = AllowedHeaders::List(headers))
    }

    /// Allows whatever request headers a preflight asks for.
    pub fn allow_any_header(self) -> Self {
        self.edit(|config| config.headers = AllowedHeaders::Mirror)
    }

    /// Sends `Access-Control-Allow-Credentials: true`.
    ///
    /// # Panics
    ///
    /// Panics if any origin is already allowed (see
    /// [`allow_any_origin`](Self::allow_any_origin)).
    pub fn allow_credentials(self, allow: bool) -> Self {
        assert!(
            !(allow && self.config.any_origin),
            "credentials cannot be combined with allow_any_origin; list the origins instead"
        );
        self.edit(|config| config.credentials = allow)
    }

    /// Response headers scripts may read (`Access-Control-Expose-Headers`).
    ///
    /// # Panics
    ///
    /// Panics on an invalid header name.
    pub fn expose_headers<I, T>(self, headers: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        let headers = headers
            .into_iter()
            .map(|name| header_name(name.as_ref()))
            .collect();
        self.edit(|config| config.expose = headers)
    }

    /// How long browsers may cache a preflight answer
    /// (`Access-Control-Max-Age`, whole seconds).
    pub fn max_age(self, max_age: Duration) -> Self {
        self.edit(|config| config.max_age = Some(max_age))
    }
}

impl Default for Cors {
    fn default() -> Self {
        Self::new()
    }
}

fn header_name(name: &str) -> HeaderName {
    HeaderName::from_bytes(name.as_bytes()).expect("a valid header name")
}

fn join<'a>(names: impl Iterator<Item = &'a str>) -> HeaderValue {
    let joined = names.collect::<Vec<_>>().join(", ");
    HeaderValue::from_str(&joined).expect("header names and methods are valid header values")
}

impl CorsConfig {
    fn allows_origin(&self, origin: &HeaderValue) -> bool {
        self.any_origin
            || origin
                .to_str()
                .is_ok_and(|origin| self.origins.iter().any(|allowed| allowed == origin))
    }

    fn origin_value(&self, origin: &HeaderValue) -> HeaderValue {
        if self.any_origin {
            HeaderValue::from_static("*")
        } else {
            origin.clone()
        }
    }

    /// The headers a preflight asks for, if all of them are allowed.
    fn allowed_request_headers(
        &self,
        requested: Option<&HeaderValue>,
    ) -> Option<Option<HeaderValue>> {
        let requested = requested.and_then(|value| value.to_str().ok());
        match (&self.headers, requested) {
            (_, None) | (_, Some("")) => Some(None),
            (AllowedHeaders::Mirror, Some(requested)) => {
                Some(HeaderValue::from_str(requested).ok())
            }
            (AllowedHeaders::List(allowed), Some(requested)) => {
                let all_allowed = requested.split(',').map(str::trim).all(|name| {
                    allowed
                        .iter()
                        .any(|allowed| allowed.as_str().eq_ignore_ascii_case(name))
                });
                all_allowed.then(|| Some(join(allowed.iter().map(HeaderName::as_str))))
            }
        }
    }

    /// Adds the headers the CORS answer depends on to `Vary`, keeping every
    /// existing value. `extra` names more request headers (a preflight).
    fn add_vary(&self, headers: &mut http::HeaderMap, extra: &[&'static str]) {
        if self.any_origin && extra.is_empty() {
            return;
        }
        let present = |headers: &http::HeaderMap, name: &str| {
            headers
                .get_all(header::VARY)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .any(|token| token.trim().eq_ignore_ascii_case(name) || token.trim() == "*")
        };
        let origin = (!self.any_origin).then_some("Origin");
        for name in origin.into_iter().chain(extra.iter().copied()) {
            if !present(headers, name) {
                headers.append(header::VARY, HeaderValue::from_static(name));
            }
        }
    }

    fn preflight(&self, origin: &HeaderValue, request: &Request<RequestBody>) -> HttpResponse {
        let mut response = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::CONTENT_LENGTH, "0")
            .body(ResponseBody::full(Bytes::new()))
            .expect("a valid preflight response");
        let headers = response.headers_mut();
        let method = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|value| value.to_str().ok());
        let method_allowed = method.is_some_and(|method| {
            self.methods
                .iter()
                .any(|allowed| allowed.as_str() == method)
        });
        let allowed_headers = self.allowed_request_headers(
            request
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_HEADERS),
        );
        if self.allows_origin(origin)
            && method_allowed
            && let Some(allowed_headers) = allowed_headers
        {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                self.origin_value(origin),
            );
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                join(self.methods.iter().map(Method::as_str)),
            );
            if let Some(allowed_headers) = allowed_headers {
                headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, allowed_headers);
            }
            if self.credentials {
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
            if let Some(max_age) = self.max_age {
                headers.insert(
                    header::ACCESS_CONTROL_MAX_AGE,
                    HeaderValue::from(max_age.as_secs()),
                );
            }
        }
        self.add_vary(
            headers,
            &[
                "Access-Control-Request-Method",
                "Access-Control-Request-Headers",
            ],
        );
        response
    }

    /// `origin` is `None` for a request without an `Origin` header. Such a
    /// response still varies on `Origin` (so a shared cache cannot hand it to
    /// a browser), and with any-origin mode it carries `*` like every other.
    fn decorate(&self, origin: Option<&HeaderValue>, response: &mut HttpResponse) {
        let headers = response.headers_mut();
        if let Some(origin) = origin.filter(|origin| self.allows_origin(origin)) {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                self.origin_value(origin),
            );
            if self.credentials {
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
            if !self.expose.is_empty() {
                headers.insert(
                    header::ACCESS_CONTROL_EXPOSE_HEADERS,
                    join(self.expose.iter().map(HeaderName::as_str)),
                );
            }
        } else if self.any_origin {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
        }
        self.add_vary(headers, &[]);
    }
}

impl Middleware for Cors {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let origin = request.headers().get(header::ORIGIN).cloned();
        let config = Arc::clone(&self.config);
        let is_preflight = request.method() == Method::OPTIONS
            && request
                .headers()
                .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
        if let (Some(origin), true) = (&origin, is_preflight) {
            let response = config.preflight(origin, &request);
            return Box::pin(async move { response });
        }
        Box::pin(async move {
            let mut response = next.run(request).await;
            config.decorate(origin.as_ref(), &mut response);
            response
        })
    }
}
