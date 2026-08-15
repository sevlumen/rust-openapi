use super::*;

/// An immutable application runtime produced by [`App::build`].
pub struct AppRuntime<S = ()> {
    pub(crate) state: Arc<S>,
    pub(crate) plans: Box<[RoutePlan<S>]>,
    pub(crate) capture_names: Box<[Option<Arc<[String]>>]>,
    pub(crate) static_routes: HashMap<String, RouteSet>,
    pub(crate) dynamic_routes: DynamicRouteTrie,
}

pub(crate) struct RuntimeRef<'a, S> {
    state: &'a Arc<S>,
    plans: &'a [RoutePlan<S>],
    capture_names: &'a [Option<Arc<[String]>>],
    static_routes: &'a HashMap<String, RouteSet>,
    dynamic_routes: &'a DynamicRouteTrie,
}

pub(crate) struct ConnectionRuntime<S> {
    runtime: Arc<AppRuntime<S>>,
}

enum PreparedDispatch {
    Ready(Option<HttpResponse>),
    Handler {
        is_head: bool,
        future: HandlerFuture,
    },
    Buffered(BoxFuture<HttpResponse>),
}

impl Future for PreparedDispatch {
    type Output = HttpResponse;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            match self.get_unchecked_mut() {
                Self::Ready(response) => {
                    Poll::Ready(response.take().expect("prepared response polled twice"))
                }
                Self::Handler { is_head, future } => {
                    match Pin::new_unchecked(future).poll(context) {
                        Poll::Ready(response) => Poll::Ready(maybe_head_flag(*is_head, response)),
                        Poll::Pending => Poll::Pending,
                    }
                }
                Self::Buffered(future) => Pin::new_unchecked(future).poll(context),
            }
        }
    }
}

impl<S: Send + Sync + 'static> ConnectionRuntime<S> {
    pub(crate) fn new(runtime: Arc<AppRuntime<S>>) -> Self {
        Self { runtime }
    }

    pub(crate) fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        self.runtime.runtime_ref()
    }

    fn prepare(&self, request: Request<Incoming>) -> PreparedDispatch {
        let method = request.method();
        let is_head = *method == Method::HEAD;
        let path_end = normalize_request_path(request.uri().path()).len();
        let router = self.runtime_ref();
        let path = &request.uri().path()[..path_end];
        if let Some(routes) = router.static_routes.get(path) {
            return match resolve_route_set(method, routes) {
                Ok(index) => self.prepare_matched(router, request, index, StaticCaptures, is_head),
                Err(RouteFailure::Options(allow)) => {
                    PreparedDispatch::Ready(Some(options_response(&allow)))
                }
                Err(RouteFailure::MethodNotAllowed(allow)) => {
                    PreparedDispatch::Ready(Some(method_not_allowed_response(&allow)))
                }
            };
        }
        let Some(path_match) = router.dynamic_routes.find(path) else {
            return PreparedDispatch::Ready(Some(not_found()));
        };
        match resolve_route_set(method, path_match.routes) {
            Ok(index) => self.prepare_matched(
                router,
                request,
                index,
                DynamicCaptures(path_match.captures),
                is_head,
            ),
            Err(RouteFailure::Options(allow)) => {
                PreparedDispatch::Ready(Some(options_response(&allow)))
            }
            Err(RouteFailure::MethodNotAllowed(allow)) => {
                PreparedDispatch::Ready(Some(method_not_allowed_response(&allow)))
            }
        }
    }

    fn prepare_matched<C: CaptureProvider + Send + 'static>(
        &self,
        router: RuntimeRef<'_, S>,
        request: Request<Incoming>,
        index: RouteId,
        captures: C,
        is_head: bool,
    ) -> PreparedDispatch {
        let plan = &router.plans[index.index()];
        match plan.body_mode {
            BodyMode::Incoming => match &plan.handler {
                HandlerKind::Raw(handler) => PreparedDispatch::Handler {
                    is_head,
                    future: handler(request),
                },
                HandlerKind::Zero(_)
                | HandlerKind::TypedNoParams(_)
                | HandlerKind::Typed(_)
                | HandlerKind::Static(_) => {
                    unreachable!("only raw handlers may receive Incoming")
                }
            },
            BodyMode::None => match &plan.handler {
                HandlerKind::Zero(handler) => PreparedDispatch::Handler {
                    is_head,
                    future: handler(),
                },
                HandlerKind::Static(response) => {
                    PreparedDispatch::Ready(Some(maybe_head_flag(is_head, response.to_response())))
                }
                HandlerKind::TypedNoParams(handler) => {
                    let (parts, _) = request.into_parts();
                    let mut request = Request::from_parts(parts, Bytes::new());
                    let future = handler(&mut request, router.state);
                    PreparedDispatch::Handler { is_head, future }
                }
                HandlerKind::Typed(handler) => {
                    let (parts, _) = request.into_parts();
                    let mut request = Request::from_parts(parts, Bytes::new());
                    let future = captures.invoke(
                        &plan.capture_mode,
                        &mut request,
                        router.capture_names[index.index()].as_ref(),
                        handler,
                        router.state,
                    );
                    PreparedDispatch::Handler { is_head, future }
                }
                HandlerKind::Raw(_) => unreachable!("raw handler requires Incoming"),
            },
            BodyMode::Buffered => {
                let limit = plan.body_limit as usize;
                let runtime = Arc::clone(&self.runtime);
                let (parts, body) = request.into_parts();
                let too_large = parts
                    .headers
                    .get(header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<usize>().ok())
                    .is_some_and(|length| length > limit);
                if too_large {
                    return PreparedDispatch::Ready(Some(payload_too_large_response()));
                }
                PreparedDispatch::Buffered(Box::pin(async move {
                    match Limited::new(body, limit).collect().await {
                        Ok(body) => {
                            runtime
                                .runtime_ref()
                                .handle_matched(
                                    Request::from_parts(parts, body.to_bytes()),
                                    index,
                                    captures,
                                    is_head,
                                )
                                .await
                        }
                        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
                            payload_too_large_response()
                        }
                        Err(_) => response_json(
                            StatusCode::BAD_REQUEST,
                            json!({
                                "type": "about:blank",
                                "title": "Bad Request",
                                "status": 400,
                                "detail": "request body was interrupted"
                            }),
                        ),
                    }
                }))
            }
        }
    }
}

impl<S: Send + Sync + 'static> AppRuntime<S> {
    fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        RuntimeRef {
            state: &self.state,
            plans: &self.plans,
            capture_names: &self.capture_names,
            static_routes: &self.static_routes,
            dynamic_routes: &self.dynamic_routes,
        }
    }

    #[cfg(any(test, feature = "test-util"))]
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
            response: Some(self.runtime_ref().handle(request).await),
        }
    }

    pub async fn listen(
        self,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        self.serve_listener(listener, std::future::pending()).await
    }

    pub async fn serve_listener<F>(
        self,
        listener: tokio::net::TcpListener,
        shutdown: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        serve_runtime(self, listener, shutdown).await
    }
}

impl<'a, S: Send + Sync + 'static> RuntimeRef<'a, S> {
    #[cfg(any(test, feature = "test-util"))]
    async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
        let is_head = request.method() == Method::HEAD;
        let path = normalize_request_path(request.uri().path());
        if let Some(routes) = self.static_routes.get(path) {
            return match resolve_route_set(request.method(), routes) {
                Ok(index) => {
                    self.handle_matched(request, index, StaticCaptures, is_head)
                        .await
                }
                Err(RouteFailure::Options(allow)) => options_response(&allow),
                Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
            };
        }
        let Some(path_match) = self.dynamic_routes.find(path) else {
            return not_found();
        };
        match resolve_route_set(request.method(), path_match.routes) {
            Ok(index) => {
                self.handle_matched(
                    request,
                    index,
                    DynamicCaptures(path_match.captures),
                    is_head,
                )
                .await
            }
            Err(RouteFailure::Options(allow)) => options_response(&allow),
            Err(RouteFailure::MethodNotAllowed(allow)) => method_not_allowed_response(&allow),
        }
    }

    async fn handle_matched<C: CaptureProvider>(
        &self,
        request: Request<Bytes>,
        index: RouteId,
        captures: C,
        is_head: bool,
    ) -> HttpResponse {
        let plan = &self.plans[index.index()];
        match &plan.handler {
            HandlerKind::Zero(handler) => return maybe_head_flag(is_head, handler().await),
            HandlerKind::Static(response) => {
                return maybe_head_flag(is_head, response.to_response());
            }
            HandlerKind::TypedNoParams(_) | HandlerKind::Typed(_) | HandlerKind::Raw(_) => {}
        }
        let mut request = request;
        if matches!(plan.body_mode, BodyMode::Buffered)
            && request.body().len() > plan.body_limit as usize
        {
            return payload_too_large_response();
        }
        let response = match &plan.handler {
            HandlerKind::TypedNoParams(handler) => handler(&mut request, self.state).await,
            HandlerKind::Typed(handler) => {
                captures
                    .invoke(
                        &plan.capture_mode,
                        &mut request,
                        self.capture_names[index.index()].as_ref(),
                        handler,
                        self.state,
                    )
                    .await
            }
            HandlerKind::Raw(_) => unreachable!("raw routes require Incoming request bodies"),
            HandlerKind::Zero(_) | HandlerKind::Static(_) => {
                unreachable!("fast handlers returned before typed dispatch")
            }
        };
        maybe_head_flag(is_head, response)
    }
}

async fn serve_runtime<S, F>(
    runtime: AppRuntime<S>,
    listener: tokio::net::TcpListener,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let runtime = Arc::new(runtime);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let connection = ConnectionRuntime::new(Arc::clone(&runtime));
                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                        let prepared = connection.prepare(request);
                        async move {
                            Ok::<_, Infallible>(prepared.await)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service)
                        .await;
                });
            }
        }
    }
    Ok(())
}

/// A response returned by [`App::oneshot`] for concise integration tests.
#[cfg(any(test, feature = "test-util"))]
pub struct TestResponse {
    pub(crate) response: Option<HttpResponse>,
}

#[cfg(any(test, feature = "test-util"))]
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
