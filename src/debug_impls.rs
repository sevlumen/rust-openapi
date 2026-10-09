//! `Debug` for the public types that hold closures, handlers or other state
//! that cannot be printed. They show the type name only; secrets (tokens,
//! keys) never leak through `{:?}`.

use std::fmt;

use crate::*;

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
