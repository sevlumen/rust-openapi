use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::*;

type PanicHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Middleware that turns a panic in a handler (or in a layer below it) into a
/// `500` problem-details response, so one bad request does not drop the
/// connection. The panic message is never sent to the client; pass it to
/// [`CatchPanic::with_hook`] to log it.
///
/// A panic while building or polling the downstream chain is caught; one in
/// the hook itself is not. Panics can leave `std::sync::Mutex` state poisoned,
/// so later requests touching it may fail too, and a handler that panicked
/// before reading the request body may make Hyper close the connection after
/// the `500`.
///
/// This needs unwinding: with `panic = "abort"` in the final binary's
/// profile the process still aborts. Register it first (`app.layer(...)`
/// before other layers) to cover every layer.
#[derive(Clone)]
#[must_use = "middleware does nothing until it is registered with `App::layer`"]
pub struct CatchPanic {
    hook: Option<PanicHook>,
}

impl CatchPanic {
    pub fn new() -> Self {
        Self { hook: None }
    }

    /// Calls `hook` with the panic message (or a placeholder for a payload
    /// that is not a string) before the `500` is returned.
    pub fn with_hook(hook: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            hook: Some(Arc::new(hook)),
        }
    }
}

impl Default for CatchPanic {
    fn default() -> Self {
        Self::new()
    }
}

/// Polls a boxed future, converting a panic during `poll` into `Err`. The
/// future that panicked is dropped inside the guard too.
struct Guarded(Option<BoxFuture<HttpResponse>>);

impl Future for Guarded {
    type Output = Result<HttpResponse, Box<dyn Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let future = self.0.as_mut().expect("polled after completion");
        match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(context))) {
            Ok(Poll::Ready(response)) => Poll::Ready(Ok(response)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(payload) => {
                let poisoned = self.0.take();
                // A panicking `Drop` must not escape either; the first panic wins.
                let _ = catch_unwind(AssertUnwindSafe(|| drop(poisoned)));
                Poll::Ready(Err(payload))
            }
        }
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "panic with a non-string payload"
    }
}

impl Middleware for CatchPanic {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let hook = self.hook.clone();
        Box::pin(async move {
            // Building the future runs the layers below and the extractors, which
            // can panic before anything is polled.
            let created = catch_unwind(AssertUnwindSafe(|| Box::pin(next.run(request))));
            let result = match created {
                Ok(future) => Guarded(Some(future)).await,
                Err(payload) => Err(payload),
            };
            match result {
                Ok(response) => response,
                Err(payload) => {
                    if let Some(hook) = hook {
                        hook(panic_message(payload.as_ref()));
                    }
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal Server Error",
                        "the request could not be completed",
                    )
                    .into_response()
                }
            }
        })
    }
}
