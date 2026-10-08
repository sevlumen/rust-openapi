//! Response helpers (extra headers, redirects, HTML, cookies) and the
//! `Cookies` extractor.

use std::fmt;

use http::header::HeaderName;

use crate::*;

/// A response with extra headers and/or another status, built with
/// [`ResponseExt`].
pub struct Headered<T> {
    inner: T,
    status: Option<StatusCode>,
    headers: Vec<(HeaderName, HeaderValue)>,
}

/// Adds headers, a status or cookies to any [`IntoResponse`] value:
///
/// ```
/// use oas_rs::{ResponseExt, SetCookie};
/// async fn login() -> impl oas_rs::IntoResponse {
///     "welcome".with_cookie(SetCookie::new("sid", "abc").http_only())
/// }
/// ```
pub trait ResponseExt: IntoResponse + Sized {
    /// Sets a header, replacing an earlier value (use
    /// [`with_cookie`](Self::with_cookie) for `Set-Cookie`, which accumulates).
    fn with_header(self, name: HeaderName, value: HeaderValue) -> Headered<Self> {
        Headered {
            inner: self,
            status: None,
            headers: vec![(name, value)],
        }
    }

    /// Replaces the response status.
    fn with_status(self, status: StatusCode) -> Headered<Self> {
        Headered {
            inner: self,
            status: Some(status),
            headers: Vec::new(),
        }
    }

    /// Adds a `Set-Cookie` header; several cookies are all sent.
    fn with_cookie(self, cookie: SetCookie) -> Headered<Self> {
        self.with_header(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie.to_string()).expect("a valid Set-Cookie value"),
        )
    }
}

impl<T: IntoResponse> ResponseExt for T {}

impl<T> Headered<T> {
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }

    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_cookie(self, cookie: SetCookie) -> Self {
        self.with_header(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie.to_string()).expect("a valid Set-Cookie value"),
        )
    }
}

impl<T: IntoResponse> IntoResponse for Headered<T> {
    fn into_response(self) -> HttpResponse {
        let mut response = self.inner.into_response();
        if let Some(status) = self.status {
            *response.status_mut() = status;
        }
        for (name, value) in self.headers {
            if name == header::SET_COOKIE {
                response.headers_mut().append(name, value);
            } else {
                response.headers_mut().insert(name, value);
            }
        }
        response
    }
}

impl<T: ResponseMetadata> ResponseMetadata for Headered<T> {
    fn status_code() -> StatusCode {
        T::status_code()
    }

    fn response_schema() -> Option<Value> {
        T::response_schema()
    }

    fn response_schema_with(registry: &mut SchemaRegistry) -> Option<Value> {
        T::response_schema_with(registry)
    }
}

/// A redirect with an empty body. The OpenAPI document lists it as `302`
/// whatever constructor was used.
#[derive(Clone, Debug)]
pub struct Redirect {
    status: StatusCode,
    location: HeaderValue,
}

impl Redirect {
    fn new(status: StatusCode, location: &str) -> Self {
        Self {
            status,
            location: HeaderValue::from_str(location).expect("a valid redirect location"),
        }
    }

    /// `302 Found`.
    pub fn found(location: &str) -> Self {
        Self::new(StatusCode::FOUND, location)
    }

    /// `303 See Other`: follow with `GET`, the usual answer to a form `POST`.
    pub fn see_other(location: &str) -> Self {
        Self::new(StatusCode::SEE_OTHER, location)
    }

    /// `307 Temporary Redirect`: repeat the same method and body.
    pub fn temporary(location: &str) -> Self {
        Self::new(StatusCode::TEMPORARY_REDIRECT, location)
    }

    /// `308 Permanent Redirect`: repeat the same method and body.
    pub fn permanent(location: &str) -> Self {
        Self::new(StatusCode::PERMANENT_REDIRECT, location)
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> HttpResponse {
        Response::builder()
            .status(self.status)
            .header(header::LOCATION, self.location)
            .header(header::CONTENT_LENGTH, "0")
            .body(ResponseBody::full(Bytes::new()))
            .expect("a valid redirect response")
    }
}

impl ResponseMetadata for Redirect {
    fn status_code() -> StatusCode {
        StatusCode::FOUND
    }
}

/// An HTML response (`text/html; charset=utf-8`). Nothing is escaped: pass
/// trusted or already-escaped markup.
#[derive(Clone, Debug)]
pub struct Html<T>(pub T);

impl<T: Into<String> + Send + 'static> IntoResponse for Html<T> {
    fn into_response(self) -> HttpResponse {
        let body = Bytes::from(self.0.into());
        let length = body.len();
        let mut response = Response::new(ResponseBody::full(body));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        response
    }
}

impl<T: Into<String> + Send + 'static> ResponseMetadata for Html<T> {}

/// The `SameSite` attribute of a cookie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    /// Sent on cross-site requests; browsers require `Secure` with it.
    None,
}

/// A `Set-Cookie` header value. Values are written as given: percent-encode
/// anything outside the cookie-octet set yourself.
#[derive(Clone, Debug)]
pub struct SetCookie {
    name: String,
    value: String,
    path: Option<String>,
    domain: Option<String>,
    max_age: Option<Duration>,
    http_only: bool,
    secure: bool,
    same_site: Option<SameSite>,
}

fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn is_cookie_octet(byte: u8) -> bool {
    matches!(byte, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E)
}

fn is_attribute_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte >= 0x20 && byte != 0x7F && byte != b';')
}

impl SetCookie {
    /// # Panics
    ///
    /// Panics if `name` is not a cookie name (an HTTP token) or `value`
    /// contains a character a cookie value may not (space, `"`, `,`, `;`,
    /// `\\`, controls, non-ASCII): a malformed cookie is a programming error.
    pub fn new(name: &str, value: &str) -> Self {
        assert!(is_token(name), "invalid cookie name {name:?}");
        assert!(
            value.bytes().all(is_cookie_octet),
            "invalid cookie value for {name:?}: percent-encode it first"
        );
        Self {
            name: name.to_owned(),
            value: value.to_owned(),
            path: None,
            domain: None,
            max_age: None,
            http_only: false,
            secure: false,
            same_site: None,
        }
    }

    /// # Panics
    ///
    /// Panics if `path` is empty or contains `;` or control characters.
    pub fn path(mut self, path: &str) -> Self {
        assert!(is_attribute_value(path), "invalid cookie Path {path:?}");
        self.path = Some(path.to_owned());
        self
    }

    /// # Panics
    ///
    /// Panics if `domain` is empty or contains `;` or control characters.
    pub fn domain(mut self, domain: &str) -> Self {
        assert!(
            is_attribute_value(domain),
            "invalid cookie Domain {domain:?}"
        );
        self.domain = Some(domain.to_owned());
        self
    }

    /// `Max-Age` in whole seconds; zero deletes the cookie.
    pub fn max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// Hides the cookie from scripts.
    pub fn http_only(mut self) -> Self {
        self.http_only = true;
        self
    }

    /// Sends the cookie over HTTPS only.
    pub fn secure(mut self) -> Self {
        self.secure = true;
        self
    }

    pub fn same_site(mut self, same_site: SameSite) -> Self {
        self.same_site = Some(same_site);
        self
    }
}

impl fmt::Display for SetCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}={}", self.name, self.value)?;
        if let Some(path) = &self.path {
            write!(formatter, "; Path={path}")?;
        }
        if let Some(domain) = &self.domain {
            write!(formatter, "; Domain={domain}")?;
        }
        if let Some(max_age) = self.max_age {
            write!(formatter, "; Max-Age={}", max_age.as_secs())?;
        }
        if self.http_only {
            formatter.write_str("; HttpOnly")?;
        }
        if self.secure {
            formatter.write_str("; Secure")?;
        }
        match self.same_site {
            Some(SameSite::Strict) => formatter.write_str("; SameSite=Strict")?,
            Some(SameSite::Lax) => formatter.write_str("; SameSite=Lax")?,
            Some(SameSite::None) => formatter.write_str("; SameSite=None")?,
            None => {}
        }
        Ok(())
    }
}

/// The cookies of a request, from every `Cookie` header. Malformed pairs are
/// skipped; values are returned as sent (a surrounding pair of quotes is
/// removed) and are not percent-decoded. Documents no OpenAPI parameters.
#[derive(Clone, Debug, Default)]
pub struct Cookies(Vec<(String, String)>);

impl Cookies {
    /// The first cookie called `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

impl<S: Send + Sync + 'static> FromRequest<S> for Cookies {
    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        let mut cookies = Vec::new();
        for value in request.headers().get_all(header::COOKIE) {
            let Ok(value) = value.to_str() else { continue };
            for pair in value.split(';') {
                let Some((name, value)) = pair.split_once('=') else {
                    continue;
                };
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                let value = value.trim();
                let value = value
                    .strip_prefix('"')
                    .and_then(|inner| inner.strip_suffix('"'))
                    .unwrap_or(value);
                cookies.push((name.to_owned(), value.to_owned()));
            }
        }
        Ok(Cookies(cookies))
    }
}

/// A `application/x-www-form-urlencoded` request body, decoded into `T`
/// (`+` is a space, as HTML forms send it). `T` derives `ApiSchema` (and
/// `Deserialize`), which also documents the form in OpenAPI. Other media
/// types get `415`; a missing or malformed field `400`. The body is buffered
/// up to the route's limit.
#[derive(Clone, Debug)]
pub struct Form<T>(pub T);

fn is_form_media_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/x-www-form-urlencoded")
}

impl<S, T> FromRequest<S> for Form<T>
where
    S: Send + Sync + 'static,
    T: DeserializeOwned + OpenApiQuery + Send + 'static,
{
    const NEEDS_BODY: bool = true;

    fn openapi_request() -> OpenApiRequest {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for parameter in T::parameters() {
            let Some(name) = parameter["name"].as_str() else {
                continue;
            };
            if parameter["required"] == true {
                required.push(json!(name));
            }
            properties.insert(name.to_owned(), parameter["schema"].clone());
        }
        let mut schema = Map::new();
        schema.insert("type".to_owned(), json!("object"));
        schema.insert("properties".to_owned(), Value::Object(properties));
        if !required.is_empty() {
            schema.insert("required".to_owned(), Value::Array(required));
        }
        OpenApiRequest {
            request_body: Some(json!({
                "required": true,
                "content": {
                    "application/x-www-form-urlencoded": { "schema": Value::Object(schema) }
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
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !is_form_media_type(content_type) {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Media Type",
                "expected application/x-www-form-urlencoded",
            ));
        }
        let body = std::str::from_utf8(request.body())
            .map_err(|_| ApiError::bad_request("form body is not valid UTF-8"))?;
        // `+` is a space in forms (unlike in query strings here).
        let body = body.replace('+', " ");
        T::parse(&body).map(Form)
    }
}
