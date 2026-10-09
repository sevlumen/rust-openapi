use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::*;

const MAX_LEN: usize = 128;

/// Middleware that gives every request an id: it keeps a usable
/// `X-Request-Id` sent by the client (1 to 128 characters of `A-Z a-z 0-9 - _ . :`),
/// otherwise generates one, makes it visible to handlers as the request
/// header and copies it onto the response, including `404`/`405` ones.
///
/// Generated ids are unique within the process and across restarts, but they
/// are not random: do not use them as secrets.
#[derive(Clone)]
#[must_use = "middleware does nothing until it is registered with `App::layer`"]
pub struct RequestId {
    name: http::HeaderName,
    prefix: u64,
    counter: Arc<AtomicU64>,
}

impl RequestId {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u64);
        Self {
            name: http::HeaderName::from_static("x-request-id"),
            prefix: nanos ^ (u64::from(std::process::id()) << 40),
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Uses another header name instead of `x-request-id`.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid header name.
    pub fn header_name(mut self, name: &str) -> Self {
        self.name = http::HeaderName::from_bytes(name.as_bytes()).expect("a valid header name");
        self
    }

    fn generate(&self) -> HeaderValue {
        let sequence = self.counter.fetch_add(1, Ordering::Relaxed);
        HeaderValue::from_str(&format!("{:x}-{sequence:x}", self.prefix))
            .expect("hex digits and '-' are valid header characters")
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

fn usable(value: &HeaderValue) -> bool {
    let bytes = value.as_bytes();
    (1..=MAX_LEN).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

impl Middleware for RequestId {
    fn handle(&self, mut request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let id = match request.headers().get(&self.name) {
            Some(value) if usable(value) => value.clone(),
            _ => self.generate(),
        };
        request.headers_mut().insert(self.name.clone(), id.clone());
        let name = self.name.clone();
        Box::pin(async move {
            let mut response = next.run(request).await;
            response.headers_mut().insert(name, id);
            response
        })
    }
}
