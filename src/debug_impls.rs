//! `Debug` for the public types that hold closures, handlers or other state
//! that cannot be printed. They show the type name only; secrets (tokens,
//! keys) never leak through `{:?}`.

use std::fmt;

use crate::*;

/// Prints as `<redacted>`: stands in for a value that must never reach a log.
pub(crate) struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Whether a header's value may appear in `{:?}` output. Everything not known to
/// be harmless is hidden (default deny), so a custom credential header
/// (`X-Credential`, `X-Access-Code`...) cannot leak just because its name does
/// not look secret.
pub(crate) fn is_safe_header(name: &str) -> bool {
    const SAFE: [&str; 40] = [
        "accept",
        "accept-charset",
        "accept-encoding",
        "accept-language",
        "access-control-request-headers",
        "access-control-request-method",
        "cache-control",
        "connection",
        "content-encoding",
        "content-language",
        "content-length",
        "content-type",
        "date",
        "dnt",
        "expect",
        "forwarded",
        "host",
        "if-match",
        "if-modified-since",
        "if-none-match",
        "if-range",
        "if-unmodified-since",
        "max-forwards",
        "origin",
        "pragma",
        "range",
        "referer",
        "sec-websocket-protocol",
        "sec-websocket-version",
        "te",
        "traceparent",
        "tracestate",
        "transfer-encoding",
        "upgrade",
        "upgrade-insecure-requests",
        "user-agent",
        "via",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ];
    const SAFE_MORE: [&str; 3] = ["x-real-ip", "x-request-id", "x-correlation-id"];
    SAFE.contains(&name)
        || SAFE_MORE.contains(&name)
        || name.starts_with("sec-fetch-")
        || name.starts_with("sec-ch-")
}

macro_rules! opaque {
    ($(#[$meta:meta])* impl[$($gen:tt)*] $ty:ty) => {
        $(#[$meta])*
        impl<$($gen)*> fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }
    };
    ($(#[$meta:meta])* $ty:ty) => {
        $(#[$meta])*
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }
    };
}

opaque!(impl[S] App<S>);
opaque!(impl['a, S] Group<'a, S>);
opaque!(impl[S] AppRuntime<S>);
opaque!(BearerAuth);
opaque!(CatchPanic);
opaque!(Cors);
opaque!(ErrorFormat);
opaque!(HandlerFuture);
opaque!(RateLimit);
opaque!(RequestId);
opaque!(SchemaRegistry);
opaque!(Trace);
opaque!(impl[S] StreamResponse<S>);
opaque!(impl[S] Sse<S>);
opaque!(impl[T] Headered<T>);
opaque!(impl['a, S] OpenApiOptions<'a, S>);
opaque!(#[cfg(any(test, feature = "swagger"))] impl['a, S] SwaggerOptions<'a, S>);
opaque!(
    #[cfg(any(test, feature = "test-util"))]
    TestResponse
);
opaque!(
    #[cfg(feature = "compression")]
    Compress
);
opaque!(
    #[cfg(feature = "multipart")]
    Multipart
);
opaque!(
    #[cfg(feature = "multipart")]
    Field
);
opaque!(
    #[cfg(feature = "static-files")]
    ServeDir
);
opaque!(
    #[cfg(feature = "websocket")]
    WebSocket
);
opaque!(
    #[cfg(feature = "websocket")]
    WebSocketResponse
);
opaque!(
    #[cfg(feature = "websocket")]
    WebSocketUpgrade
);

impl fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(Some(bytes)) => write!(f, "ResponseBody::Full({} bytes)", bytes.len()),
            Self::Full(None) => f.write_str("ResponseBody::Full(consumed)"),
            Self::Stream(_) => f.write_str("ResponseBody::Stream(..)"),
        }
    }
}
