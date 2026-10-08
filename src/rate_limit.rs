use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
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
    /// Keyed by a randomly seeded 64-bit hash of the key, so memory per
    /// bucket is fixed however long the key is.
    buckets: HashMap<u64, Bucket>,
    overflow: Bucket,
    /// When idle buckets were last swept out of a full table.
    last_sweep: Instant,
}

struct Limiter {
    /// Tokens added per second.
    rate: f64,
    burst: f64,
    max_keys: usize,
    key: Arc<KeyFn>,
    hasher: RandomState,
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
/// To bound memory, at most [`max_keys`](Self::max_keys) buckets are kept
/// (each is a fixed-size entry under a hash of the key, however long the key
/// is); idle ones are dropped first, and once the table is full of busy keys
/// new keys share a single overflow bucket, so varying the key cannot bypass
/// the limit. The flip side: a client that can pick its own key freely can
/// keep the table full and push every *new* legitimate key into that shared
/// bucket, so key on something the client cannot choose (a proxy-set header,
/// or an identity checked by a layer registered before this one). Register it
/// early so rejected requests cost as little as possible, and after `Cors` so
/// browsers can read the `429` and preflights are not counted. `Retry-After`
/// is the wait rounded up to whole seconds.
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
                hasher: RandomState::new(),
                state: Mutex::new(State {
                    buckets: HashMap::new(),
                    overflow: Bucket {
                        tokens: burst,
                        last: Instant::now(),
                    },
                    last_sweep: Instant::now(),
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
            hasher: RandomState::new(),
            state: Mutex::new(State {
                buckets: HashMap::new(),
                overflow: Bucket {
                    tokens: old.burst,
                    last: Instant::now(),
                },
                last_sweep: Instant::now(),
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

    /// Limits each client IP address separately, ignoring the port (an IPv4
    /// address on a dual-stack socket is keyed as plain IPv4). Needs
    /// [`AppRuntime::connect_info`](crate::AppRuntime::connect_info)`(true)`;
    /// without it every request lands in the anonymous bucket. Behind a proxy
    /// all clients share the proxy's address: key on a header instead. A single
    /// IPv6 client usually owns a whole /64, so it can rotate addresses past a
    /// per-address limit; key on a prefix with [`key_by`](Self::key_by) if that
    /// matters.
    pub fn key_by_peer_ip(self) -> Self {
        self.key_by(|request| {
            crate::peer_addr(request).map(|peer| peer.ip().to_canonical().to_string())
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
        // `now` was read before taking the lock; never move the clock back.
        bucket.last = bucket.last.max(now);
    }

    /// Takes a token, or returns how long until one is available.
    fn take(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let id = self.hasher.hash_one(key);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let known = state.buckets.contains_key(&id);
        if !known && state.buckets.len() >= self.max_keys {
            // Sweeping is O(table), so a full table is swept at most once per
            // token interval (and at least a second apart): a flood of new keys
            // cannot make every request rescan it under the lock.
            let interval = (1.0 / self.rate).max(1.0);
            if now
                .saturating_duration_since(state.last_sweep)
                .as_secs_f64()
                >= interval
            {
                state.last_sweep = now;
                // A bucket that would be full again carries no information.
                let (rate, burst) = (self.rate, self.burst);
                state.buckets.retain(|_, bucket| {
                    let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
                    bucket.tokens + elapsed * rate < burst
                });
            }
        }
        let overflow = !known && state.buckets.len() >= self.max_keys;
        let mut bucket = if overflow {
            state.overflow
        } else {
            state.buckets.get(&id).copied().unwrap_or(Bucket {
                tokens: self.burst,
                last: now,
            })
        };
        self.refill(&mut bucket, now);
        let outcome = if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(
                Duration::try_from_secs_f64((1.0 - bucket.tokens) / self.rate)
                    .unwrap_or(Duration::MAX),
            )
        };
        if overflow {
            state.overflow = bucket;
        } else {
            state.buckets.insert(id, bucket);
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
