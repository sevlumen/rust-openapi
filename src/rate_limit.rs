use std::collections::HashMap;
use std::sync::Mutex;

use tokio::time::Instant;

use crate::*;

const DEFAULT_MAX_KEYS: usize = 10_000;

type KeyFn = dyn Fn(&Request<RequestBody>) -> Option<String> + Send + Sync;

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

struct State {
    buckets: HashMap<String, Bucket>,
    overflow: Bucket,
}

struct Limiter {
    /// Tokens added per second.
    rate: f64,
    burst: f64,
    max_keys: usize,
    key: Arc<KeyFn>,
    state: Mutex<State>,
}

/// A token-bucket rate limiter: `limit` requests per `per`, with a burst
/// (default: `limit`). A request that finds the bucket empty gets
/// `429 Too Many Requests` with a `Retry-After` header and never reaches
/// the layers or handler below it.
///
/// By default one bucket is shared by every request. Use
/// [`key_by_header`](Self::key_by_header) or [`key_by`](Self::key_by) to
/// limit each client (an API key, a forwarded address) separately; requests
/// without a key share one anonymous bucket. The state lives in this
/// process: with several instances each one enforces its own limit.
///
/// To bound memory, at most [`max_keys`](Self::max_keys) buckets are kept;
/// idle ones are dropped first, and once the table is full of busy keys new
/// keys share a single overflow bucket, so varying the key cannot bypass the
/// limit. Register it early so rejected requests cost as little as possible,
/// and after `Cors` so browsers can read the `429`.
///
/// There is no built-in key for the peer address (the middleware does not see
/// the socket); behind a proxy key on the header it sets, and only trust
/// that header if the proxy overwrites what clients send.
#[derive(Clone)]
pub struct RateLimit {
    limiter: Arc<Limiter>,
}

impl RateLimit {
    /// # Panics
    ///
    /// Panics if `limit` is 0 or `per` is zero.
    pub fn new(limit: u32, per: Duration) -> Self {
        assert!(limit >= 1, "the rate limit must be at least 1 request");
        assert!(!per.is_zero(), "the rate limit period must not be zero");
        let burst = f64::from(limit);
        Self {
            limiter: Arc::new(Limiter {
                rate: burst / per.as_secs_f64(),
                burst,
                max_keys: DEFAULT_MAX_KEYS,
                key: Arc::new(|_| None),
                state: Mutex::new(State {
                    buckets: HashMap::new(),
                    overflow: Bucket {
                        tokens: burst,
                        last: Instant::now(),
                    },
                }),
            }),
        }
    }

    fn edit(self, change: impl FnOnce(&mut Limiter)) -> Self {
        let old = &*self.limiter;
        let mut limiter = Limiter {
            rate: old.rate,
            burst: old.burst,
            max_keys: old.max_keys,
            key: Arc::clone(&old.key),
            state: Mutex::new(State {
                buckets: HashMap::new(),
                overflow: Bucket {
                    tokens: old.burst,
                    last: Instant::now(),
                },
            }),
        };
        change(&mut limiter);
        let burst = limiter.burst;
        if let Ok(state) = limiter.state.get_mut() {
            state.overflow.tokens = burst;
        }
        Self {
            limiter: Arc::new(limiter),
        }
    }

    /// How many requests may arrive back to back (default: the limit).
    ///
    /// # Panics
    ///
    /// Panics if `burst` is 0.
    pub fn burst(self, burst: u32) -> Self {
        assert!(burst >= 1, "the burst must be at least 1 request");
        self.edit(|limiter| limiter.burst = f64::from(burst))
    }

    /// Limits each distinct value of `header` separately.
    pub fn key_by_header(self, header: &str) -> Self {
        let name = http::HeaderName::from_bytes(header.as_bytes()).expect("a valid header name");
        self.key_by(move |request| {
            request
                .headers()
                .get(&name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        })
    }

    /// Limits each distinct key separately; `None` means the anonymous bucket.
    pub fn key_by(
        self,
        key: impl Fn(&Request<RequestBody>) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.edit(|limiter| limiter.key = Arc::new(key))
    }

    /// The most per-key buckets kept at once (default 10,000).
    ///
    /// # Panics
    ///
    /// Panics if `max_keys` is 0.
    pub fn max_keys(self, max_keys: usize) -> Self {
        assert!(max_keys >= 1, "max_keys must be at least 1");
        self.edit(|limiter| limiter.max_keys = max_keys)
    }
}

impl Limiter {
    fn refill(&self, bucket: &mut Bucket, now: Instant) {
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.last = now;
    }

    /// Takes a token, or returns how long until one is available.
    fn take(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.buckets.contains_key(key) && state.buckets.len() >= self.max_keys {
            // A bucket that would be full again carries no information.
            let (rate, burst) = (self.rate, self.burst);
            state.buckets.retain(|_, bucket| {
                let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
                bucket.tokens + elapsed * rate < burst
            });
        }
        let overflow = !state.buckets.contains_key(key) && state.buckets.len() >= self.max_keys;
        let mut bucket = if overflow {
            state.overflow
        } else {
            state.buckets.get(key).copied().unwrap_or(Bucket {
                tokens: self.burst,
                last: now,
            })
        };
        self.refill(&mut bucket, now);
        let outcome = if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / self.rate))
        };
        if overflow {
            state.overflow = bucket;
        } else {
            state.buckets.insert(key.to_owned(), bucket);
        }
        outcome
    }
}

impl Middleware for RateLimit {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let key = (self.limiter.key)(&request).unwrap_or_default();
        match self.limiter.take(&key, Instant::now()) {
            Ok(()) => next.run(request),
            Err(wait) => Box::pin(async move {
                let mut response = ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too Many Requests",
                    "rate limit exceeded; retry later",
                )
                .into_response();
                let seconds = wait.as_secs_f64().ceil().max(1.0) as u64;
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
                response
            }),
        }
    }
}
