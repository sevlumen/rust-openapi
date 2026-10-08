use super::*;

/// An immutable application runtime produced by [`App::build`].
pub struct AppRuntime<S = ()> {
    pub(crate) inner: Arc<RuntimeInner<S>>,
    pub(crate) shutdown_timeout: Duration,
    pub(crate) tcp_nodelay: bool,
    pub(crate) header_read_timeout: Option<Duration>,
    pub(crate) max_connections: Option<usize>,
    pub(crate) connect_info: bool,
    pub(crate) connection_error_observer: Option<ErrorObserver>,
    #[cfg(feature = "http2")]
    pub(crate) http2_max_concurrent_streams: Option<u32>,
    #[cfg(feature = "http2")]
    pub(crate) h2c: bool,
    #[cfg(feature = "tls")]
    pub(crate) handshake_timeout: Duration,
}

/// Called with each connection-level error (see
/// [`AppRuntime::on_connection_error`]).
pub(crate) type ErrorObserver = Arc<dyn Fn(&(dyn std::error::Error + 'static)) + Send + Sync>;

/// The immutable routing state, shared (by `Arc`) with every connection and
/// with any middleware chain in flight.
pub(crate) struct RuntimeInner<S> {
    /// How long a buffered request body may take to arrive.
    pub(crate) body_read_timeout: Option<Duration>,
    pub(crate) state: Arc<S>,
    pub(crate) plans: Box<[RoutePlan<S>]>,
    pub(crate) capture_names: Box<[Option<Arc<[String]>>]>,
    pub(crate) static_routes: HashMap<String, RouteSet>,
    pub(crate) dynamic_routes: DynamicRouteTrie,
    pub(crate) middleware: Box<[ScopedLayer]>,
}

impl<S: Send + Sync + 'static> RuntimeInner<S> {
    /// Whether any registered layer covers this request. When none does (no
    /// layers at all, or only scoped layers that miss), the request skips the
    /// chain and takes the allocation-free direct path.
    pub(crate) fn has_matching_layer(&self, method: &Method, path: &str) -> bool {
        self.middleware
            .iter()
            .any(|entry| entry.scope.matches(method, path))
    }

    pub(crate) fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        RuntimeRef {
            state: &self.state,
            plans: &self.plans,
            capture_names: &self.capture_names,
            static_routes: &self.static_routes,
            dynamic_routes: &self.dynamic_routes,
        }
    }
}

/// How long [`AppRuntime::serve_listener`] waits for in-flight requests after
/// the shutdown signal before giving up on the remaining connections.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a buffered request body (JSON, form, multipart extractor) may take
/// to arrive in full (see [`AppRuntime::body_read_timeout`]).
pub const DEFAULT_BODY_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a client may take to send a complete request head before the
/// connection is closed (see [`AppRuntime::header_read_timeout`]). Hyper also
/// applies it while a keep-alive connection waits for its next request.
pub const DEFAULT_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a TLS client may take to finish the handshake before the
/// connection is dropped (see `AppRuntime::handshake_timeout`).
#[cfg(feature = "tls")]
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct RuntimeRef<'a, S> {
    state: &'a Arc<S>,
    plans: &'a [RoutePlan<S>],
    capture_names: &'a [Option<Arc<[String]>>],
    static_routes: &'a HashMap<String, RouteSet>,
    dynamic_routes: &'a DynamicRouteTrie,
}

pub(crate) struct ConnectionRuntime<S> {
    runtime: Arc<RuntimeInner<S>>,
    /// The peer address to attach to each request (only when enabled).
    peer: Option<std::net::SocketAddr>,
    /// The `max_connections` slot of this connection, shared with whatever
    /// outlives the HTTP connection (an upgraded WebSocket).
    #[cfg_attr(not(feature = "websocket"), allow(dead_code))]
    permit: Option<Arc<ConnectionSlot>>,
}

/// The connection's slot, placed in the request extensions so a WebSocket
/// handler can keep it for as long as the session lasts.
#[cfg(feature = "websocket")]
#[derive(Clone)]
#[allow(dead_code)] // held only to keep the slot alive
pub(crate) struct ConnectionPermit(pub(crate) Arc<ConnectionSlot>);

/// The remote address of the connection a request arrived on, stored in the
/// request extensions when [`AppRuntime::connect_info`] is on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PeerAddr(pub(crate) std::net::SocketAddr);

pub(crate) enum PreparedDispatch {
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
        // SAFETY: this is a structural pin projection. `self` is never moved
        // out of or replaced, and the only field that is `!Unpin` (the
        // `HandlerFuture`, which may hold an inline future that must not move)
        // is re-pinned in place with `Pin::new_unchecked`; the `Buffered`
        // future is a `Pin<Box<_>>` and could be polled safely, but shares the
        // same projection.
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
    pub(crate) fn new(runtime: Arc<RuntimeInner<S>>, peer: Option<std::net::SocketAddr>) -> Self {
        Self {
            runtime,
            peer,
            permit: None,
        }
    }

    pub(crate) fn with_permit(mut self, permit: Option<Arc<ConnectionSlot>>) -> Self {
        self.permit = permit;
        self
    }

    pub(crate) fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        self.runtime.runtime_ref()
    }

    pub(crate) fn prepare(&self, mut request: Request<Incoming>) -> PreparedDispatch {
        if let Some(peer) = self.peer {
            request.extensions_mut().insert(PeerAddr(peer));
        }
        #[cfg(feature = "websocket")]
        if let Some(permit) = &self.permit {
            request
                .extensions_mut()
                .insert(ConnectionPermit(Arc::clone(permit)));
        }
        if !self
            .runtime
            .has_matching_layer(request.method(), request.uri().path())
        {
            return self.prepare_direct(request);
        }
        let (parts, body) = request.into_parts();
        let request = Request::from_parts(parts, RequestBody::incoming(body));
        let host: Arc<dyn Host> = self.runtime.clone();
        PreparedDispatch::Buffered(Next::new(host).run(request))
    }

    /// Dispatch without the middleware chain; also the end of the chain.
    pub(crate) fn prepare_direct(&self, request: Request<Incoming>) -> PreparedDispatch {
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
                let timeout = self.runtime.body_read_timeout;
                PreparedDispatch::Buffered(Box::pin(async move {
                    // A body that is already complete finishes at once and never
                    // arms the timer.
                    let collected = match timeout {
                        Some(timeout) => {
                            match tokio::time::timeout(timeout, Limited::new(body, limit).collect())
                                .await
                            {
                                Ok(collected) => collected,
                                Err(_) => return request_timeout_response(),
                            }
                        }
                        None => Limited::new(body, limit).collect().await,
                    };
                    match collected {
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
                        Err(_) => ErrorInfo::new(
                            StatusCode::BAD_REQUEST,
                            "Bad Request",
                            "request body was interrupted",
                        )
                        .attach(response_json(
                            StatusCode::BAD_REQUEST,
                            json!({
                                "type": "about:blank",
                                "title": "Bad Request",
                                "status": 400,
                                "detail": "request body was interrupted"
                            }),
                        )),
                    }
                }))
            }
        }
    }
}

impl<S: Send + Sync + 'static> AppRuntime<S> {
    /// Sets how long a graceful shutdown waits for in-flight requests to
    /// finish (default [`DEFAULT_SHUTDOWN_TIMEOUT`]). Connections still busy
    /// after the timeout are abandoned.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Whether accepted TCP connections get `TCP_NODELAY` (default `true`).
    ///
    /// With Nagle's algorithm left on, a response written in several small
    /// pieces (streamed or chunked bodies) can stall for about 40 ms on Linux
    /// while the kernel waits for a delayed ACK. Passing `false` leaves the
    /// socket exactly as accepted (it inherits the listener's setting, which
    /// is off unless you enabled it when building the listener).
    pub fn tcp_nodelay(mut self, enabled: bool) -> Self {
        self.tcp_nodelay = enabled;
        self
    }

    /// How long a client may take to send a complete request head (default
    /// [`DEFAULT_HEADER_READ_TIMEOUT`]); `None` disables the limit. A slow
    /// client is disconnected, which also closes an idle keep-alive connection
    /// that sends nothing for that long. Applies to HTTP/1.1, plain or over
    /// TLS. It does not cover reading a request body, and HTTP/2 connections
    /// are not covered at all.
    ///
    /// Behind a proxy or load balancer, set it higher than the proxy's idle
    /// timeout toward this server (for example nginx `keepalive_timeout`
    /// 75 s, AWS ALB 60 s), or the proxy may reuse a connection this server
    /// just closed and report a 502.
    pub fn header_read_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.header_read_timeout = timeout;
        self
    }

    /// How long a *buffered* request body (a `Json`, `Form` or `Multipart`
    /// extractor) may take to arrive in full (default
    /// [`DEFAULT_BODY_READ_TIMEOUT`], 60 s); `None` disables the limit. A body
    /// that is still incomplete then gets `408 Request Timeout` and the
    /// connection is closed, so a client cannot hold a handler task and a
    /// connection slot by sending a header and trickling (or never sending)
    /// the body. Raw handlers read their own stream: wrap `next_field()` /
    /// `chunk()` in `tokio::time::timeout` there. The deadline is for the whole
    /// body, so raise it for routes that take large uploads over slow links.
    ///
    /// # Panics
    ///
    /// Panics if the runtime is already being served (call it right after
    /// `build()`).
    pub fn body_read_timeout(mut self, timeout: Option<Duration>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("body_read_timeout must be set before the runtime is served")
            .body_read_timeout = timeout;
        self
    }

    /// Caps the number of open connections (default: unlimited). While the
    /// cap is reached the server stops accepting; further clients wait in the
    /// operating system's listen backlog until a connection closes. Shutdown
    /// is not delayed by the wait.
    ///
    /// Idle keep-alive connections hold their slot until
    /// [`header_read_timeout`](Self::header_read_timeout) closes them, so do
    /// not combine a limit with `header_read_timeout(None)`: enough idle
    /// clients would stop the server from accepting anyone.
    ///
    /// Limits above the semaphore maximum are clamped.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is `Some(0)`: nothing could ever be served.
    pub fn max_connections(mut self, limit: Option<usize>) -> Self {
        assert!(limit != Some(0), "max_connections must be at least 1");
        self.max_connections = limit;
        self
    }

    /// Makes the peer's socket address available to handlers and middleware:
    /// the [`ConnectInfo`] extractor, and [`RateLimit::key_by_peer_ip`]. Off by
    /// default because it adds an extension to every request. Behind a proxy
    /// the peer is the proxy; use a header it sets instead. Unix sockets have
    /// no address, and in-process (`oneshot`) requests none either.
    pub fn connect_info(mut self, enabled: bool) -> Self {
        self.connect_info = enabled;
        self
    }

    /// Calls `observer` with every error that ends a connection: malformed
    /// requests, header timeouts, I/O failures, failed or timed-out TLS
    /// handshakes (an `io::Error`) and HTTP/2 connection errors. A client
    /// that disconnects mid-request also shows up here. Hyper errors can be
    /// told apart with `downcast_ref::<hyper::Error>()`. The default is to
    /// ignore them. A panic in the observer ends that connection's task.
    pub fn on_connection_error(
        mut self,
        observer: impl Fn(&(dyn std::error::Error + 'static)) + Send + Sync + 'static,
    ) -> Self {
        self.connection_error_observer = Some(Arc::new(observer));
        self
    }

    #[cfg(any(test, feature = "test-util"))]
    fn runtime_ref(&self) -> RuntimeRef<'_, S> {
        self.inner.runtime_ref()
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
        let response = if !self
            .inner
            .has_matching_layer(request.method(), request.uri().path())
        {
            self.runtime_ref().handle(request).await
        } else {
            let (parts, body) = request.into_parts();
            let request = Request::from_parts(parts, RequestBody::full(body));
            let host: Arc<dyn Host> = self.inner.clone();
            Next::new(host).run(request).await
        };
        TestResponse {
            response: Some(response),
        }
    }

    pub async fn listen(
        self,
        address: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        self.serve_listener(listener, std::future::pending()).await
    }

    /// Serves HTTP/1.1 (and HTTP/2 when [`h2c`](Self::h2c) is on) on a Unix
    /// domain socket, for example behind a reverse proxy on the same host,
    /// with the same shutdown, timeout, connection limit and observer
    /// behaviour as [`serve_listener`](Self::serve_listener).
    ///
    /// The socket file is created by `bind` with the process umask (often
    /// connectable by everyone: restrict it with `chmod` or the umask), a
    /// stale one must be removed before binding, and it is not removed on
    /// shutdown. Peer credentials are not exposed to handlers.
    #[cfg(unix)]
    pub async fn serve_unix<F>(
        self,
        listener: tokio::net::UnixListener,
        shutdown: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        serve_runtime(self, listener, shutdown).await
    }

    /// Accepts HTTP/2 over plain TCP "with prior knowledge" (h2c) on
    /// [`serve_listener`](Self::serve_listener) and
    /// `serve_unix`, next to HTTP/1.1 on the same port;
    /// the protocol is picked from the first bytes of each connection. Off by
    /// default. Meant for a trusted proxy or load balancer that speaks h2c to
    /// its upstream; browsers do not. The Upgrade-based h2c of RFC 7540 is not
    /// supported. A connection must send its first bytes within
    /// [`header_read_timeout`](Self::header_read_timeout) (that deadline also
    /// covers a partial preface), after which the timeout applies to HTTP/1.1
    /// requests only.
    #[cfg(feature = "http2")]
    pub fn h2c(mut self, enabled: bool) -> Self {
        self.h2c = enabled;
        self
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
    /// In-memory dispatch: used by `oneshot` and as the end of a middleware
    /// chain for buffered requests.
    pub(crate) async fn handle(&self, request: Request<Bytes>) -> HttpResponse {
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

#[derive(Debug, PartialEq, Eq)]
enum AcceptAction {
    /// The failed connection is gone; accept the next one right away.
    Retry,
    /// The process is out of descriptors or memory; wait for it to recover.
    Backoff,
    /// The listener itself is broken; stop serving.
    Fatal,
}

/// A source of connections: a TCP listener or, on Unix, a Unix socket
/// listener. Everything after `accept` is the same for both.
pub(crate) trait Listener {
    type Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static;

    /// The next connection and, where there is one, the peer's socket address.
    fn accept(
        &self,
    ) -> impl Future<Output = std::io::Result<(Self::Io, Option<std::net::SocketAddr>)>> + Send;

    /// `TCP_NODELAY` where it exists; a no-op otherwise.
    fn set_nodelay(_io: &Self::Io) {}
}

impl Listener for tokio::net::TcpListener {
    type Io = tokio::net::TcpStream;

    async fn accept(&self) -> std::io::Result<(Self::Io, Option<std::net::SocketAddr>)> {
        tokio::net::TcpListener::accept(self)
            .await
            .map(|(stream, peer)| (stream, Some(peer)))
    }

    fn set_nodelay(io: &Self::Io) {
        // Failing only leaves the operating system default in place.
        let _ = io.set_nodelay(true);
    }
}

#[cfg(unix)]
impl Listener for tokio::net::UnixListener {
    type Io = tokio::net::UnixStream;

    async fn accept(&self) -> std::io::Result<(Self::Io, Option<std::net::SocketAddr>)> {
        tokio::net::UnixListener::accept(self)
            .await
            .map(|(stream, _)| (stream, None))
    }
}

/// One accepted connection.
pub(crate) struct Accepted<Io> {
    pub(crate) io: Io,
    pub(crate) peer: Option<std::net::SocketAddr>,
    pub(crate) slot: Option<ConnectionSlot>,
}

/// Holds one of the `max_connections` slots until dropped.
pub(crate) type ConnectionSlot = tokio::sync::OwnedSemaphorePermit;

pub(crate) fn connection_limit(limit: Option<usize>) -> Option<Arc<tokio::sync::Semaphore>> {
    limit.map(|limit| {
        Arc::new(tokio::sync::Semaphore::new(
            limit.min(tokio::sync::Semaphore::MAX_PERMITS),
        ))
    })
}

/// An HTTP/1.1 connection builder with the header timeout applied. Hyper's
/// timeouts do nothing without a timer, so one is installed whenever a
/// timeout is set.
pub(crate) fn http1_builder(
    header_read_timeout: Option<Duration>,
) -> hyper::server::conn::http1::Builder {
    let mut builder = hyper::server::conn::http1::Builder::new();
    if header_read_timeout.is_some() {
        builder.timer(hyper_util::rt::TokioTimer::new());
    }
    builder.header_read_timeout(header_read_timeout);
    builder
}

pub(crate) fn report_connection_error(
    observer: &Option<ErrorObserver>,
    error: &(dyn std::error::Error + 'static),
) {
    if let Some(observer) = observer {
        observer(error);
    }
}

const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

fn classify_accept_error(error: &std::io::Error) -> AcceptAction {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset | ErrorKind::Interrupted => {
            return AcceptAction::Retry;
        }
        ErrorKind::OutOfMemory => return AcceptAction::Backoff,
        _ => {}
    }
    match error.raw_os_error() {
        // EMFILE, ENFILE, ENOMEM, ENOBUFS (Linux, macOS/BSD)
        #[cfg(unix)]
        Some(23 | 24 | 12 | 105 | 55) => AcceptAction::Backoff,
        // WSAEMFILE, WSAENOBUFS
        #[cfg(windows)]
        Some(10024 | 10055) => AcceptAction::Backoff,
        _ => AcceptAction::Fatal,
    }
}

/// Waits for the next connection. `Ok(None)` means `shutdown` completed.
/// Transient accept errors are absorbed (see [`classify_accept_error`]); only
/// a broken listener is returned as an error. When `nodelay` is set the
/// accepted socket gets `TCP_NODELAY` (a failure to set it is ignored: the
/// connection still works, just with the operating system default).
///
/// With a connection `limit`, a slot is taken before accepting and handed back
/// with the stream: hold it as long as the connection lives. While every slot
/// is taken this waits (racing `shutdown`) and accepts nothing.
pub(crate) async fn accept_next<L, F>(
    listener: &L,
    shutdown: &mut Pin<&mut F>,
    nodelay: bool,
    limit: Option<&Arc<tokio::sync::Semaphore>>,
) -> Result<Option<Accepted<L::Io>>, std::io::Error>
where
    L: Listener,
    F: Future<Output = ()>,
{
    let slot = match limit {
        Some(semaphore) => tokio::select! {
            _ = shutdown.as_mut() => return Ok(None),
            permit = Arc::clone(semaphore).acquire_owned() => {
                Some(permit.expect("the connection semaphore is never closed"))
            }
        },
        None => None,
    };
    loop {
        tokio::select! {
            _ = shutdown.as_mut() => return Ok(None),
            accepted = listener.accept() => match accepted {
                Ok((io, peer)) => {
                    if nodelay {
                        L::set_nodelay(&io);
                    }
                    return Ok(Some(Accepted { io, peer, slot }));
                }
                Err(error) => match classify_accept_error(&error) {
                    AcceptAction::Retry => continue,
                    AcceptAction::Backoff => {
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                        continue;
                    }
                    AcceptAction::Fatal => return Err(error),
                },
            },
        }
    }
}

async fn serve_runtime<S, L, F>(
    runtime: AppRuntime<S>,
    listener: L,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Send + Sync + 'static,
    L: Listener,
    F: Future<Output = ()> + Send + 'static,
{
    #[cfg(feature = "http2")]
    let (h2c, max_streams) = (runtime.h2c, runtime.http2_max_concurrent_streams);
    let shutdown_timeout = runtime.shutdown_timeout;
    let nodelay = runtime.tcp_nodelay;
    let header_read_timeout = runtime.header_read_timeout;
    let limit = connection_limit(runtime.max_connections);
    let connect_info = runtime.connect_info;
    let observer = runtime.connection_error_observer;
    let runtime = runtime.inner;
    // Every connection task holds a `done` sender and watches `signal`;
    // shutdown fires the signal, then waits for the senders to be dropped.
    // (`GracefulShutdown::watch` cannot wrap an upgradeable connection, and
    // this form also covers the connections that are still sniffing for h2.)
    let (signal, _) = tokio::sync::watch::channel(());
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);
    tokio::pin!(shutdown);
    while let Some(Accepted {
        io: stream,
        peer,
        slot,
    }) = accept_next(&listener, &mut shutdown, nodelay, limit.as_ref()).await?
    {
        let slot = slot.map(Arc::new);
        let connection =
            ConnectionRuntime::new(Arc::clone(&runtime), peer.filter(|_| connect_info))
                .with_permit(slot.clone());
        let service = hyper::service::service_fn(move |request: Request<Incoming>| {
            let prepared = connection.prepare(request);
            async move { Ok::<_, Infallible>(prepared.await) }
        });
        let observer = observer.clone();
        let mut stop = signal.subscribe();
        let done = done_tx.clone();
        #[cfg(feature = "http2")]
        if h2c {
            tokio::spawn(async move {
                let _done = done;
                let _slot = slot;
                let mut stream = stream;
                let mut seen = Vec::new();
                let detection = async {
                    match header_read_timeout {
                        Some(limit) => {
                            tokio::time::timeout(limit, detect_h2(&mut stream, &mut seen))
                                .await
                                .unwrap_or_else(|_| {
                                    Err(std::io::Error::new(
                                        std::io::ErrorKind::TimedOut,
                                        "no request before header_read_timeout",
                                    ))
                                })
                        }
                        None => detect_h2(&mut stream, &mut seen).await,
                    }
                };
                let is_h2 = tokio::select! {
                    // Shutting down while the first bytes are awaited.
                    _ = stop.changed() => return,
                    outcome = detection => match outcome {
                        Ok(is_h2) => is_h2,
                        Err(error) => {
                            report_connection_error(&observer, &error);
                            return;
                        }
                    },
                };
                let io = hyper_util::rt::TokioIo::new(Rewind {
                    prefix: Bytes::from(seen),
                    inner: stream,
                });
                let outcome = if is_h2 {
                    let mut builder = hyper::server::conn::http2::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    );
                    if let Some(limit) = max_streams {
                        builder.max_concurrent_streams(limit);
                    }
                    drive(
                        builder.serve_connection(io, service),
                        &mut stop,
                        |connection| connection.graceful_shutdown(),
                    )
                    .await
                } else {
                    let connection =
                        http1_builder(header_read_timeout).serve_connection(io, service);
                    #[cfg(feature = "websocket")]
                    let connection = connection.with_upgrades();
                    drive(connection, &mut stop, |connection| {
                        connection.graceful_shutdown()
                    })
                    .await
                };
                if let Err(error) = outcome {
                    report_connection_error(&observer, &error);
                }
            });
            continue;
        }
        let io = hyper_util::rt::TokioIo::new(stream);
        let connection = http1_builder(header_read_timeout).serve_connection(io, service);
        #[cfg(feature = "websocket")]
        let connection = connection.with_upgrades();
        tokio::spawn(async move {
            let _done = done;
            let _slot = slot;
            if let Err(error) = drive(connection, &mut stop, |connection| {
                connection.graceful_shutdown()
            })
            .await
            {
                report_connection_error(&observer, &error);
            }
        });
    }
    let _ = signal.send(());
    drop(done_tx);
    let _ = tokio::time::timeout(shutdown_timeout, done_rx.recv()).await;
    Ok(())
}

/// Drives a connection until it ends, asking it to finish gracefully (idle
/// keep-alive closes at once, in-flight requests complete) when `stop` fires.
pub(crate) async fn drive<C, E>(
    connection: C,
    stop: &mut tokio::sync::watch::Receiver<()>,
    graceful: impl FnOnce(Pin<&mut C>),
) -> Result<(), E>
where
    C: Future<Output = Result<(), E>>,
{
    let mut connection = std::pin::pin!(connection);
    tokio::select! {
        result = connection.as_mut() => result,
        _ = stop.changed() => {
            graceful(connection.as_mut());
            connection.await
        }
    }
}

/// The HTTP/2 connection preface a client sends first with prior knowledge.
#[cfg(feature = "http2")]
const H2_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Reads from `io` until the h2 preface is confirmed or ruled out, keeping
/// what was read in `seen`. A first byte that cannot start the preface means
/// HTTP/1.1 at once, so ordinary requests are not delayed.
#[cfg(feature = "http2")]
async fn detect_h2<T: tokio::io::AsyncRead + Unpin>(
    io: &mut T,
    seen: &mut Vec<u8>,
) -> std::io::Result<bool> {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 24];
    while seen.len() < H2_PREFACE.len() {
        if !H2_PREFACE.starts_with(seen) {
            return Ok(false);
        }
        let want = H2_PREFACE.len() - seen.len();
        let read = io.read(&mut chunk[..want]).await?;
        if read == 0 {
            // Closed early: HTTP/1.1 handling reports it like any other.
            return Ok(false);
        }
        seen.extend_from_slice(&chunk[..read]);
    }
    Ok(seen.as_slice() == H2_PREFACE)
}

/// An I/O object that first replays bytes that were already read from it.
#[cfg(feature = "http2")]
struct Rewind<T> {
    prefix: Bytes,
    inner: T,
}

#[cfg(feature = "http2")]
impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Rewind<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.prefix.is_empty() {
            let count = self.prefix.len().min(buffer.remaining());
            let replay = self.prefix.split_to(count);
            buffer.put_slice(&replay);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

#[cfg(feature = "http2")]
impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Rewind<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, data)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(context, buffers)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
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

    /// Every value of a header that may appear more than once.
    pub fn header_all(&self, name: &str) -> Vec<&str> {
        self.response
            .as_ref()
            .map(|response| {
                response
                    .headers()
                    .get_all(name)
                    .iter()
                    .filter_map(|value| value.to_str().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The raw body, for responses that are not UTF-8 (compressed ones).
    pub async fn body_bytes(mut self) -> Vec<u8> {
        let response = self.response.take().unwrap();
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
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

#[cfg(test)]
mod accept_tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn connection_level_errors_retry_immediately() {
        for kind in [
            ErrorKind::ConnectionAborted,
            ErrorKind::ConnectionReset,
            ErrorKind::Interrupted,
        ] {
            assert_eq!(
                classify_accept_error(&Error::from(kind)),
                AcceptAction::Retry,
                "{kind:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn resource_exhaustion_backs_off_instead_of_exiting() {
        // EMFILE, ENFILE, ENOBUFS (Linux), ENOMEM
        for code in [24, 23, 105, 12] {
            assert_eq!(
                classify_accept_error(&Error::from_raw_os_error(code)),
                AcceptAction::Backoff,
                "os error {code}"
            );
        }
    }

    #[tokio::test]
    async fn accepted_sockets_get_tcp_nodelay_only_when_asked() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        for enabled in [true, false] {
            let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let shutdown = std::future::pending::<()>();
            tokio::pin!(shutdown);
            let Accepted { io: accepted, .. } =
                accept_next(&listener, &mut shutdown, enabled, None)
                    .await
                    .unwrap()
                    .expect("a connection");
            assert_eq!(accepted.nodelay().unwrap(), enabled);
        }
    }

    #[test]
    fn unrecoverable_errors_still_stop_the_server() {
        assert_eq!(
            classify_accept_error(&Error::from(ErrorKind::InvalidInput)),
            AcceptAction::Fatal
        );
    }
}
