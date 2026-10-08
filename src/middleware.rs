use crate::*;
use http_body::{Body, Frame, SizeHint};
use std::fmt;

/// The request body seen by middleware: Hyper's streaming body for real
/// connections, or an in-memory buffer for [`AppRuntime::oneshot`]. It can be
/// read, but only the framework constructs it, so the same body always reaches
/// the handler at the end of the chain. Reading it in a layer consumes it:
/// handlers and extractors downstream see only what is left (for an in-memory
/// request, nothing).
pub struct RequestBody(RequestBodyKind);

enum RequestBodyKind {
    Incoming(Incoming),
    /// In-memory body; only `AppRuntime::oneshot` (test-util) constructs it.
    #[allow(dead_code)]
    Full(Option<Bytes>),
}

impl fmt::Debug for RequestBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match &self.0 {
            RequestBodyKind::Incoming(_) => "incoming",
            RequestBodyKind::Full(_) => "full",
        };
        formatter
            .debug_struct("RequestBody")
            .field("kind", &kind)
            .finish()
    }
}

impl RequestBody {
    pub(crate) fn incoming(body: Incoming) -> Self {
        Self(RequestBodyKind::Incoming(body))
    }

    #[allow(dead_code)]
    pub(crate) fn full(body: Bytes) -> Self {
        Self(RequestBodyKind::Full(Some(body)))
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        match &mut self.get_mut().0 {
            RequestBodyKind::Incoming(body) => Pin::new(body).poll_frame(context),
            RequestBodyKind::Full(bytes) => Poll::Ready(
                bytes
                    .take()
                    .filter(|bytes| !bytes.is_empty())
                    .map(|bytes| Ok(Frame::data(bytes))),
            ),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.0 {
            RequestBodyKind::Incoming(body) => body.is_end_stream(),
            RequestBodyKind::Full(bytes) => bytes.as_ref().is_none_or(|bytes| bytes.is_empty()),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.0 {
            RequestBodyKind::Incoming(body) => body.size_hint(),
            RequestBodyKind::Full(bytes) => {
                SizeHint::with_exact(bytes.as_ref().map_or(0, |bytes| bytes.len() as u64))
            }
        }
    }
}

/// Logic that wraps every request. Layers run before routing, in the order
/// they were registered with [`App::layer`] (the first one is outermost).
///
/// The returned future is `'static`: a hand-written implementation clones what
/// it needs (typically an `Arc`) into the future instead of borrowing `&self`.
/// Plain `async fn`s and closures with the right signature implement this
/// trait automatically.
pub trait Middleware: Send + Sync + 'static {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse>;
}

impl<F, Fut> Middleware for F
where
    F: Fn(Request<RequestBody>, Next) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = HttpResponse> + Send + 'static,
{
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        Box::pin((self)(request, next))
    }
}

/// One segment of a scope pattern: a literal, or `{name}` matching any one
/// segment.
#[derive(Clone, Debug)]
pub(crate) enum ScopeSegment {
    Literal(Box<str>),
    Any,
}

/// The part of the API a layer applies to.
#[derive(Clone)]
pub(crate) enum Scope {
    /// Every request (a global layer).
    All,
    /// Requests whose path starts with the pattern.
    Prefix(Arc<[ScopeSegment]>),
    /// Requests for exactly this path pattern and method.
    Route {
        method: Method,
        segments: Arc<[ScopeSegment]>,
    },
}

impl Scope {
    /// Parses a scope pattern with the same rules as a route template: a
    /// segment is either a literal or a whole `{name}`. Anything else (`{id`,
    /// `{}`, `x{id}`) panics instead of becoming a literal that never matches,
    /// which would silently switch an authentication layer off.
    pub(crate) fn parse(pattern: &str) -> Arc<[ScopeSegment]> {
        PathParts::new(pattern)
            .map(|part| {
                if part.value.starts_with('{') && part.value.ends_with('}') {
                    assert!(part.value.len() > 2, "invalid scope pattern: {pattern:?}");
                    ScopeSegment::Any
                } else {
                    assert!(
                        !part.value.contains('{') && !part.value.contains('}'),
                        "invalid scope pattern: {pattern:?}"
                    );
                    ScopeSegment::Literal(part.value.into())
                }
            })
            .collect::<Vec<_>>()
            .into()
    }

    pub(crate) fn matches(&self, method: &Method, path: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Prefix(segments) => segments_match(segments, path, false),
            Scope::Route {
                method: route_method,
                segments,
            } => {
                (route_method == method
                    || (*route_method == Method::GET && *method == Method::HEAD))
                    && segments_match(segments, path, true)
            }
        }
    }
}

/// Whether a percent-encoded request segment decodes to the literal. A
/// `{capture}` route decodes its segment, so `/api/%61dmin/x` can reach a
/// `/api/{section}/x` handler as `admin`; the scope has to cover it too. Only
/// segments containing `%` are decoded, and an undecodable one never matches.
fn decoded_equals(segment: &str, literal: &str) -> bool {
    segment.contains('%') && percent_decode(segment).is_ok_and(|decoded| decoded == literal)
}

/// Compares path segments with the router's own splitter (repeated slashes are
/// ignored), so a scope matches at least everything the router can route to.
fn segments_match(pattern: &[ScopeSegment], path: &str, exact: bool) -> bool {
    let mut parts = PathParts::new(path);
    for segment in pattern {
        let Some(part) = parts.next() else {
            return false;
        };
        if let ScopeSegment::Literal(literal) = segment
            && part.value != &**literal
            && !decoded_equals(part.value, literal)
        {
            return false;
        }
    }
    !exact || parts.next().is_none()
}

/// A layer together with the part of the API it applies to.
#[derive(Clone)]
pub(crate) struct ScopedLayer {
    pub(crate) scope: Scope,
    pub(crate) layer: Arc<dyn Middleware>,
}

/// The rest of the chain: the remaining layers, then normal dispatch.
pub struct Next {
    host: Arc<dyn Host>,
    index: usize,
}

impl fmt::Debug for Next {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Next")
            .field(
                "remaining_layers",
                &(self.host.middleware().len() - self.index),
            )
            .finish()
    }
}

impl Next {
    pub(crate) fn new(host: Arc<dyn Host>) -> Self {
        Self { host, index: 0 }
    }

    /// Runs the remaining layers and then the handler, returning the response.
    pub fn run(self, request: Request<RequestBody>) -> BoxFuture<HttpResponse> {
        // Skip layers whose scope does not cover this request.
        let mut index = self.index;
        let layers = self.host.middleware();
        while let Some(entry) = layers.get(index) {
            if entry.scope.matches(request.method(), request.uri().path()) {
                let layer = Arc::clone(&entry.layer);
                return layer.handle(
                    request,
                    Next {
                        host: self.host,
                        index: index + 1,
                    },
                );
            }
            index += 1;
        }
        self.host.dispatch(request)
    }
}

/// What the chain needs from the runtime: the layer list and the terminal
/// dispatch. Implemented by `RuntimeInner`.
pub(crate) trait Host: Send + Sync + 'static {
    fn middleware(&self) -> &[ScopedLayer];
    fn dispatch(self: Arc<Self>, request: Request<RequestBody>) -> BoxFuture<HttpResponse>;
}

impl<S: Send + Sync + 'static> Host for RuntimeInner<S> {
    fn middleware(&self) -> &[ScopedLayer] {
        &self.middleware
    }

    fn dispatch(self: Arc<Self>, request: Request<RequestBody>) -> BoxFuture<HttpResponse> {
        let (parts, body) = request.into_parts();
        match body.0 {
            // The prepared dispatch is already a `'static` future, so box it
            // directly instead of wrapping it in a larger async block.
            RequestBodyKind::Incoming(incoming) => Box::pin(
                ConnectionRuntime::new(self).prepare_direct(Request::from_parts(parts, incoming)),
            ),
            RequestBodyKind::Full(bytes) => Box::pin(async move {
                let request = Request::from_parts(parts, bytes.unwrap_or_default());
                self.runtime_ref().handle(request).await
            }),
        }
    }
}
