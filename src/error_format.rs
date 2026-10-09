use crate::*;

/// A framework-generated error, as given to an [`ErrorFormat`] hook: bad
/// parameters or bodies, `404`, `405`, `413`, authentication failures from
/// [`BearerAuth`] and every [`ApiError`] returned by a handler.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ErrorInfo {
    pub status: StatusCode,
    pub title: String,
    pub detail: String,
}

impl ErrorInfo {
    pub(crate) fn new(status: StatusCode, title: &str, detail: &str) -> Self {
        Self {
            status,
            title: title.to_owned(),
            detail: detail.to_owned(),
        }
    }

    /// A JSON response with this error's status and the given body.
    pub fn respond(&self, body: impl Serialize) -> HttpResponse {
        response_json(self.status, body)
    }

    /// Marks a response as a framework error so [`ErrorFormat`] can find it.
    pub(crate) fn attach(self, mut response: HttpResponse) -> HttpResponse {
        response.extensions_mut().insert(self);
        response
    }
}

type Hook = dyn Fn(&ErrorInfo) -> HttpResponse + Send + Sync;

/// Middleware that rewrites every framework-generated error response, so an
/// API can keep an existing error contract (for example `{"error": "..."}`)
/// instead of the default problem-details body.
///
/// Responses your own handlers build (a custom `418`, say) are left alone;
/// only errors the framework produces are passed to the hook. `Allow`,
/// `WWW-Authenticate` and `Retry-After` headers are carried over unless the
/// hook sets them.
/// Register it **first** (outermost) so it also covers errors from the
/// layers after it, such as [`BearerAuth`]. The OpenAPI document still
/// describes the default `Problem` schema: turn it off with
/// `app.openapi().document_errors(false)` when you change the format.
#[derive(Clone)]
#[must_use = "middleware does nothing until it is registered with `App::layer`"]
pub struct ErrorFormat {
    hook: Arc<Hook>,
}

impl ErrorFormat {
    pub fn new(hook: impl Fn(&ErrorInfo) -> HttpResponse + Send + Sync + 'static) -> Self {
        Self {
            hook: Arc::new(hook),
        }
    }
}

impl Middleware for ErrorFormat {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let hook = Arc::clone(&self.hook);
        let is_head = request.method() == Method::HEAD;
        Box::pin(async move {
            let response = next.run(request).await;
            let Some(info) = response.extensions().get::<ErrorInfo>().cloned() else {
                return response;
            };
            let mut mapped = hook(&info);
            for name in [
                header::ALLOW,
                header::WWW_AUTHENTICATE,
                header::RETRY_AFTER,
                header::HeaderName::from_static("sec-websocket-version"),
            ] {
                if let Some(value) = response.headers().get(&name)
                    && !mapped.headers().contains_key(&name)
                {
                    mapped.headers_mut().insert(name, value.clone());
                }
            }
            if is_head {
                // Keep the length the GET would have had.
                if !mapped.headers().contains_key(header::CONTENT_LENGTH)
                    && let Some(length) = http_body::Body::size_hint(mapped.body()).exact()
                {
                    mapped
                        .headers_mut()
                        .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
                }
                *mapped.body_mut() = ResponseBody::full(Bytes::new());
            }
            mapped
        })
    }
}
