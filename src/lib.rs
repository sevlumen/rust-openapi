//! `oas-rs`: a small, typed HTTP framework built on Hyper and Tokio.
//!
//! The runtime deliberately keeps OpenAPI generation out of request dispatch. The
//! document is assembled when routes are registered and is only serialized when
//! the explicitly registered OpenAPI endpoint is requested.

pub use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::Incoming;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    borrow::Cow,
    collections::HashMap,
    convert::Infallible,
    future::{self, Future},
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

mod app;
mod bearer;
mod catch_panic;
mod codec;
#[cfg(feature = "compression")]
mod compress;
mod cors;
mod error_format;
mod extract;
mod group;
mod handler;
mod middleware;
#[cfg(feature = "multipart")]
mod multipart;
mod openapi;
mod params;
mod path;
mod rate_limit;
mod request_id;
mod response;
mod router;
mod runtime;
mod schema;
mod sse;
#[cfg(feature = "static-files")]
mod static_files;
#[cfg(feature = "tls")]
mod tls;
mod trace;
mod web;
#[cfg(feature = "websocket")]
mod websocket;
pub use app::App;
use app::Operation;
pub use bearer::{BearerAuth, constant_time_eq};
pub use catch_panic::CatchPanic;
pub use codec::*;
#[cfg(feature = "compression")]
pub use compress::Compress;
pub use cors::Cors;
pub use error_format::{ErrorFormat, ErrorInfo};
pub use extract::FromRequest;
pub use extract::{ConnectInfo, Header, HeaderSpec, Headers, Path, Query, State, peer_addr};
pub use group::Group;
use handler::{BoxFuture, HandlerFuture};
pub use handler::{Handler, RawHandler};
#[cfg(test)]
use handler::{HandlerFutureKind, INLINE_FUTURE_SIZE, InlineFuture};
use middleware::{Host, Scope, ScopedLayer};
pub use middleware::{Middleware, Next, RequestBody};
#[cfg(feature = "multipart")]
pub use multipart::{Field, Multipart, MultipartField};
use openapi::OpenApiConfig;
#[cfg(any(test, feature = "swagger"))]
use openapi::SwaggerConfig;
#[cfg(any(test, feature = "swagger"))]
pub use openapi::SwaggerOptions;
use openapi::requirement_json;
#[cfg(any(test, feature = "swagger"))]
use openapi::swagger_html;
pub use openapi::{ApiKeyLocation, BuildError, OpenApiOptions, SecurityScheme};
pub use params::*;
use path::*;
pub use rate_limit::RateLimit;
pub use request_id::RequestId;
pub use response::*;
use router::{
    BodyMode, CaptureMode, CaptureProvider, CaptureSet, DynamicCaptures, DynamicRouteTrie,
    ErasedZeroHandler, HandlerKind, RouteFailure, RouteId, RouteMetadata, RoutePlan, RouteSet,
    Segment, StaticCaptures, StaticResponse, resolve_route_set,
};
#[cfg(test)]
use router::{DynamicRouteNode, NodeId};
use runtime::ConnectionRuntime;
#[cfg(feature = "tls")]
pub use runtime::DEFAULT_HANDSHAKE_TIMEOUT;
use runtime::RuntimeInner;
#[cfg(any(test, feature = "test-util"))]
pub use runtime::TestResponse;
pub use runtime::{AppRuntime, DEFAULT_HEADER_READ_TIMEOUT, DEFAULT_SHUTDOWN_TIMEOUT};
pub use schema::*;
pub use sse::{Event, Sse};
#[cfg(feature = "static-files")]
pub use static_files::ServeDir;
#[cfg(feature = "tls")]
pub use tls::{TlsConfig, TlsError};
pub use trace::{Trace, TraceRecord};
pub use web::{Cookies, Form, Headered, Html, Redirect, ResponseExt, SameSite, SetCookie};
#[cfg(feature = "websocket")]
pub use websocket::{
    CloseFrame, Message, WebSocket, WebSocketError, WebSocketResponse, WebSocketUpgrade,
};

pub use http::Method;
pub use oas_rs_macros::ApiSchema;

#[doc(hidden)]
pub mod __private {
    pub use crate::{OpenApiQuery, decode_query_component, flatten_schema, parse_query_value};
    pub use serde_json;
}

pub type HttpResponse = Response<ResponseBody>;

/// Default upper bound for request bodies collected by body extractors.
pub const DEFAULT_MAX_BODY_SIZE: usize = 1024 * 1024;

fn encode_body_limit(limit: usize) -> u32 {
    assert!(limit > 0, "max_body_size must be greater than zero");
    assert!(
        limit <= u32::MAX as usize,
        "max_body_size must fit in a u32"
    );
    limit as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_schemas_cover_numbers_collections_and_wrappers() {
        assert_eq!(
            f64::schema(),
            json!({ "type": "number", "format": "double" })
        );
        assert_eq!(f32::schema()["format"], "float");
        assert_eq!(u8::schema()["format"], "int32");
        assert_eq!(usize::schema()["format"], "int64");
        assert_eq!(Box::<String>::schema(), json!({ "type": "string" }));
        assert_eq!(
            Vec::<f64>::schema(),
            json!({ "type": "array", "items": { "type": "number", "format": "double" } })
        );
        assert_eq!(
            std::collections::HashMap::<String, bool>::schema(),
            json!({ "type": "object", "additionalProperties": { "type": "boolean" } })
        );
        assert_eq!(Value::schema(), json!({}));
    }

    #[test]
    fn percent_decode_handles_runs_escapes_and_malformed_input() {
        assert_eq!(percent_decode("").unwrap(), "");
        assert_eq!(percent_decode("plain").unwrap(), "plain");
        assert_eq!(percent_decode("%41").unwrap(), "A");
        assert_eq!(percent_decode("a%20b%2Fc").unwrap(), "a b/c");
        assert_eq!(percent_decode("%41%42%43").unwrap(), "ABC");
        assert_eq!(percent_decode("x%C3%A9y").unwrap(), "x\u{e9}y");
        for bad in ["%", "%4", "a%", "a%4", "%zz", "%4g", "%FF"] {
            assert!(percent_decode(bad).is_err(), "{bad} should be rejected");
        }
    }
    use std::{marker::PhantomPinned, mem::size_of, task::Waker};

    type BoxedPathHandler =
        Box<dyn Fn(Path<u64>) -> Pin<Box<dyn Future<Output = &'static str> + Send>> + Send + Sync>;

    fn block_on_without_io<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
        }
    }

    #[tokio::test]
    async fn openapi_document_is_prepared_once_for_server_dispatch() {
        let mut app = App::new();
        app.get("/plaintext", || async { "OK" });
        app.openapi();
        app.swagger().path("/swagger");
        assert!(app.openapi_bytes.is_none());
        assert!(app.swagger_bytes.is_none());

        let runtime = app.build().expect("test app builds");
        let response = runtime
            .oneshot(Method::GET, "/openapi.json", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.body_string().await;
        assert!(body.contains("/plaintext"));

        let mut updated = App::new();
        updated.get("/plaintext", || async { "OK" });
        updated.openapi().path("/spec.json");
        updated.swagger().path("/swagger");
        let runtime = updated.build().expect("updated app builds");
        let response = runtime.oneshot(Method::GET, "/swagger", &[], None).await;
        let swagger_body = response.body_string().await;
        assert!(
            swagger_body.contains("/spec.json"),
            "Swagger cache did not follow the updated OpenAPI path"
        );
    }

    #[tokio::test]
    async fn zero_argument_routes_use_the_fast_dispatch_shape() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });

        let index = resolve_route_set(
            &Method::GET,
            app.static_routes.get("/zero").expect("zero route"),
        )
        .expect("zero route did not resolve");
        assert!(matches!(
            app.plans[index.index()].handler,
            HandlerKind::Zero(_)
        ));

        let response = app
            .build()
            .expect("test app builds")
            .oneshot(Method::GET, "/zero", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body_string().await, "OK");
    }

    #[tokio::test]
    async fn typed_registration_does_not_require_a_cloneable_handler() {
        let handler: BoxedPathHandler = Box::new(|Path(_id)| {
            Box::pin(async { "OK" }) as Pin<Box<dyn Future<Output = &'static str> + Send>>
        });
        let mut app = App::new();
        app.get("/non-clone/{id}", handler);

        let response = app
            .build()
            .expect("test app builds")
            .oneshot(Method::GET, "/non-clone/42", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn runtime_keeps_hot_plans_separate_from_cold_metadata() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });
        app.openapi();

        assert_eq!(app.plans.len(), 1);
        assert_eq!(app.metadata.len(), 1);
    }

    #[test]
    fn build_freezes_route_storage_into_boxed_slices() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });

        let runtime = app.build().expect("test app builds");

        assert_eq!(runtime.inner.plans.len(), 1);
    }

    #[test]
    fn app_builder_is_available_as_the_registration_type() {
        let mut builder = App::new();
        builder.get("/zero", || async { "OK" });
        assert_eq!(builder.plans.len(), 1);
    }

    #[test]
    fn connection_runtime_reuses_one_runtime_owner_for_request_borrows() {
        let mut app = App::new();
        app.get("/zero", || async { "OK" });
        let runtime = app.build().expect("test app builds").inner;
        let connection = ConnectionRuntime::new(Arc::clone(&runtime), None);

        assert_eq!(Arc::strong_count(&runtime), 2);
        let _first = connection.runtime_ref();
        let _second = connection.runtime_ref();
        assert_eq!(Arc::strong_count(&runtime), 2);
    }

    #[test]
    fn no_capture_routes_use_a_shared_empty_params() {
        assert!(std::ptr::eq(Params::empty(), Params::empty()));
        assert!(Params::empty().get("id").is_none());
    }

    #[test]
    fn materialized_params_clone_shares_owned_values() {
        let names = vec!["id".to_owned()];
        let captures = CaptureSet::default().with_capture(0, CaptureRange { start: 1, end: 6 });
        let params = Params::from_match(&names, captures, "/alice", true);
        let cloned = params.clone();

        assert_eq!(params.get("id"), Some("alice"));
        assert_eq!(cloned.get("id"), Some("alice"));
        assert!(Arc::ptr_eq(
            params.owned.as_ref().expect("materialized values"),
            cloned.owned.as_ref().expect("materialized values"),
        ));
        assert!(Arc::ptr_eq(
            &params.owned.as_ref().expect("materialized values").names,
            &cloned.owned.as_ref().expect("materialized values").names,
        ));
    }

    #[test]
    fn params_keep_capture_ranges_in_compact_storage() {
        assert!(size_of::<Params>() <= 88);
    }

    #[test]
    fn route_ids_are_compact_u32_handles() {
        assert_eq!(size_of::<RouteId>(), size_of::<u32>());
        assert_eq!(size_of::<NodeId>(), size_of::<u32>());
        assert!(size_of::<Option<NodeId>>() <= size_of::<Option<usize>>());
        assert!(size_of::<DynamicRouteNode>() <= 88);
        assert!(size_of::<RouteSet>() <= 32);
    }

    #[test]
    fn route_sets_keep_allow_metadata_off_the_hot_slots() {
        let mut routes = RouteSet::default();
        assert!(routes.is_empty());
        routes.insert(Method::POST, 0);
        routes.insert(Method::GET, 1);
        assert_eq!(routes.allowed_methods(), "GET, POST");
        routes.remove(&Method::GET);
        assert_eq!(routes.allowed_methods(), "POST");
        assert!(!routes.is_empty());
        routes.remove(&Method::POST);
        assert!(routes.is_empty());
    }

    #[test]
    fn route_plans_encode_capture_materialization_mode() {
        let mut app = App::new();
        app.get("/plain", || async { "OK" });
        app.get("/path/{id}", |Path(_id): Path<String>| async { "OK" });
        app.get("/params/{id}", |_params: Params| async { "OK" });

        assert!(matches!(app.plans[0].capture_mode, CaptureMode::None));
        assert!(matches!(app.plans[1].capture_mode, CaptureMode::Borrowed));
        assert!(matches!(
            app.plans[2].capture_mode,
            CaptureMode::Materialized
        ));
    }

    #[test]
    fn route_plans_precompute_body_modes() {
        let mut app = App::new();
        app.get("/plain", || async { "OK" });
        app.post("/json", |Json(body): Json<String>| async move { body });
        app.raw_get("/upload", |_request| async { "OK" });

        assert!(matches!(app.plans[0].body_mode, BodyMode::None));
        assert!(matches!(app.plans[1].body_mode, BodyMode::Buffered));
        assert_eq!(app.plans[1].body_limit, DEFAULT_MAX_BODY_SIZE as u32);
        assert!(matches!(app.plans[2].body_mode, BodyMode::Incoming));
    }

    #[test]
    fn buffered_body_limit_is_compiled_into_route_plans() {
        let mut app = App::new();
        app.post("/json", |Json(body): Json<String>| async move { body });
        app.raw_get("/upload", |_request| async { "OK" });

        app.max_body_size(32);
        assert_eq!(app.plans[0].body_limit, 32);
        assert_eq!(app.plans[1].body_limit, 0);

        app.max_body_size(64);
        assert_eq!(app.plans[0].body_limit, 64);
    }

    #[test]
    fn raw_routes_support_explicit_methods() {
        let mut app = App::new();
        app.raw(Method::POST, "/upload", |_request| async { "OK" });

        let index = resolve_route_set(
            &Method::POST,
            app.static_routes.get("/upload").expect("raw route"),
        )
        .expect("raw POST route did not resolve");
        assert!(matches!(
            app.plans[index.index()].handler,
            HandlerKind::Raw(_)
        ));
    }

    #[test]
    fn dynamic_routes_use_a_compiled_method_trie() {
        let mut app = App::new();
        for index in 0..10_000 {
            let path = format!("/dynamic/{index}/{{id}}");
            app.get(&path, || async { "OK" });
        }

        assert!(app.dynamic_routes.node_count() < 20_010);
        let matched = app
            .dynamic_routes
            .find("/dynamic/9999/42")
            .expect("dynamic route match");
        assert_eq!(
            matched.routes.route(&Method::GET).map(RouteId::index),
            Some(9_999)
        );
        assert_eq!(matched.captures.count, 1);
        assert!(app.dynamic_routes.find("/dynamic/missing/42").is_none());
    }

    #[test]
    fn dynamic_trie_restores_captures_after_failed_static_branch() {
        let mut app = App::new();
        app.get("/files/static/other", || async { "static" });
        app.get("/files/{id}/tail", || async { "capture" });

        let matched = app
            .dynamic_routes
            .find("/files/static/tail")
            .expect("capture fallback should match");
        assert_eq!(matched.captures.count, 1);
        assert_eq!(matched.captures.range(0).unwrap().start, 7);
        assert_eq!(matched.captures.range(0).unwrap().end, 13);
    }

    #[test]
    fn report_hot_path_layout_sizes() {
        println!(
            "HandlerFuture={} InlineFuture={} Params={} CaptureMode={} BodyMode={} HandlerKind={} RoutePlan={} RouteMetadata={} RouteSet={} DynamicRouteNode={} RouteFailure={}",
            size_of::<HandlerFuture>(),
            size_of::<InlineFuture>(),
            size_of::<Params>(),
            size_of::<CaptureMode>(),
            size_of::<BodyMode>(),
            size_of::<HandlerKind<()>>(),
            size_of::<RoutePlan<()>>(),
            size_of::<RouteMetadata>(),
            size_of::<RouteSet>(),
            size_of::<DynamicRouteNode>(),
            size_of::<RouteFailure>(),
        );
    }

    #[test]
    fn capture_names_stay_out_of_hot_route_plans() {
        assert_eq!(size_of::<CaptureMode>(), 1);
        assert!(size_of::<RoutePlan<()>>() <= 32);
        assert!(size_of::<RouteFailure>() <= 32);
    }

    #[test]
    fn inline_future_preserves_pinned_application_future() {
        struct PinnedReady {
            _pin: PhantomPinned,
        }

        impl Future for PinnedReady {
            type Output = &'static str;

            fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
                Poll::Ready("OK")
            }
        }

        let response = block_on_without_io(HandlerFuture::from_response_future(PinnedReady {
            _pin: PhantomPinned,
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            block_on_without_io(response.into_body().collect())
                .unwrap()
                .to_bytes(),
            Bytes::from_static(b"OK")
        );
    }

    #[test]
    fn inline_future_polls_a_pinned_pending_future_without_moving_it() {
        struct PinnedPending {
            polled: bool,
            _pin: PhantomPinned,
        }

        impl Future for PinnedPending {
            type Output = &'static str;

            fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
                // Accessing a field through a pinned, !Unpin future is safe as
                // long as the future itself is never moved after pinning.
                let this = unsafe { self.get_unchecked_mut() };
                if this.polled {
                    Poll::Ready("OK")
                } else {
                    this.polled = true;
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }

        let response = block_on_without_io(HandlerFuture::from_response_future(PinnedPending {
            polled: false,
            _pin: PhantomPinned,
        }));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            block_on_without_io(response.into_body().collect())
                .unwrap()
                .to_bytes(),
            Bytes::from_static(b"OK")
        );
    }

    #[test]
    fn inline_future_uses_heap_fallback_for_oversized_application_future() {
        struct OversizedFuture {
            bytes: [u8; INLINE_FUTURE_SIZE + 1],
        }

        impl Future for OversizedFuture {
            type Output = &'static str;

            fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
                let _ = self.bytes[0];
                Poll::Ready("OK")
            }
        }

        let future = HandlerFuture::from_response_future(OversizedFuture {
            bytes: [0; INLINE_FUTURE_SIZE + 1],
        });
        assert!(matches!(&future.0, HandlerFutureKind::Boxed(_)));

        let response = block_on_without_io(future);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            block_on_without_io(response.into_body().collect())
                .unwrap()
                .to_bytes(),
            Bytes::from_static(b"OK")
        );
    }

    #[tokio::test]
    async fn dynamic_terminal_resolves_methods_without_route_scan() {
        let mut app = App::new();
        app.get("/resource/{id}", || async { "GET" });
        app.post("/resource/{id}", || async { "POST" });

        let response = app.oneshot(Method::PUT, "/resource/42", &[], None).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.header("allow"), Some("GET, POST"));

        let response = app
            .oneshot(Method::OPTIONS, "/resource/42", &[], None)
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.header("allow"), Some("GET, POST"));

        let response = app.oneshot(Method::PUT, "/other/missing", &[], None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn typed_no_params_dynamic_route_preserves_header_extraction() {
        struct Trace;

        impl HeaderSpec for Trace {
            const NAME: &'static str = "x-trace";

            fn parse(_value: &str) -> Result<Self, ApiError> {
                Ok(Self)
            }
        }

        let mut app = App::new();
        app.get("/trace/{id}", |Header(_): Header<Trace>| async { "OK" });

        let matched = app
            .dynamic_routes
            .find("/trace/abc")
            .expect("dynamic route should match");
        let index = matched
            .routes
            .route(&Method::GET)
            .expect("dynamic route should resolve");
        assert!(matches!(
            app.plans[index.index()].handler,
            HandlerKind::TypedNoParams(_)
        ));

        let response = app
            .oneshot(Method::GET, "/trace/abc", &[("x-trace", "1")], None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
