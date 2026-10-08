use crate::*;
use http_body::{Body, Frame, SizeHint};

/// The request body seen by middleware: Hyper's streaming body for real
/// connections, or an in-memory buffer for [`AppRuntime::oneshot`]. It can be
/// read, but only the framework constructs it, so the same body always reaches
/// the handler at the end of the chain.
pub struct RequestBody(RequestBodyKind);

enum RequestBodyKind {
    Incoming(Incoming),
    /// In-memory body; only `AppRuntime::oneshot` (test-util) constructs it.
    #[allow(dead_code)]
    Full(Option<Bytes>),
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

/// The rest of the chain: the remaining layers, then normal dispatch.
pub struct Next {
    host: Arc<dyn Host>,
    index: usize,
}

impl Next {
    pub(crate) fn new(host: Arc<dyn Host>) -> Self {
        Self { host, index: 0 }
    }

    /// Runs the remaining layers and then the handler, returning the response.
    pub fn run(self, request: Request<RequestBody>) -> BoxFuture<HttpResponse> {
        match self.host.middleware().get(self.index).cloned() {
            Some(layer) => layer.handle(
                request,
                Next {
                    host: self.host,
                    index: self.index + 1,
                },
            ),
            None => self.host.dispatch(request),
        }
    }
}

/// What the chain needs from the runtime: the layer list and the terminal
/// dispatch. Implemented by `RuntimeInner`.
pub(crate) trait Host: Send + Sync + 'static {
    fn middleware(&self) -> &[Arc<dyn Middleware>];
    fn dispatch(self: Arc<Self>, request: Request<RequestBody>) -> BoxFuture<HttpResponse>;
}

impl<S: Send + Sync + 'static> Host for RuntimeInner<S> {
    fn middleware(&self) -> &[Arc<dyn Middleware>] {
        &self.middleware
    }

    fn dispatch(self: Arc<Self>, request: Request<RequestBody>) -> BoxFuture<HttpResponse> {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            match body.0 {
                RequestBodyKind::Incoming(incoming) => {
                    let request = Request::from_parts(parts, incoming);
                    ConnectionRuntime::new(self).prepare_direct(request).await
                }
                RequestBodyKind::Full(bytes) => {
                    let request = Request::from_parts(parts, bytes.unwrap_or_default());
                    self.runtime_ref().handle(request).await
                }
            }
        })
    }
}
