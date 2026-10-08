use std::time::Instant;

use crate::*;

/// One finished request, handed to the [`Trace`] callback.
#[derive(Clone, Debug)]
pub struct TraceRecord {
    pub method: Method,
    pub path: String,
    pub status: StatusCode,
    pub elapsed: Duration,
}

/// Middleware that reports every request after its response is produced.
#[derive(Clone)]
pub struct Trace {
    sink: Arc<dyn Fn(&TraceRecord) + Send + Sync>,
}

impl Trace {
    pub fn new(sink: impl Fn(&TraceRecord) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
        }
    }

    /// Prints `METHOD /path STATUS elapsed` lines to standard error.
    pub fn stderr() -> Self {
        Self::new(|record| {
            eprintln!(
                "{} {} {} {:?}",
                record.method,
                record.path,
                record.status.as_u16(),
                record.elapsed
            );
        })
    }
}

impl Middleware for Trace {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let sink = Arc::clone(&self.sink);
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        Box::pin(async move {
            let started = Instant::now();
            let response = next.run(request).await;
            sink(&TraceRecord {
                method,
                path,
                status: response.status(),
                elapsed: started.elapsed(),
            });
            response
        })
    }
}
