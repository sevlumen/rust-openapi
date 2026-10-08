use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use oas_rs::{App, TlsConfig};
use rustls_pki_types::{CertificateDer, ServerName, pem::PemObject};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, crypto::ring},
};

pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn identity() -> Identity {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    Identity {
        cert_pem: certified.cert.pem(),
        key_pem: certified.signing_key.serialize_pem(),
    }
}

fn connector(cert_pem: &str, alpn: &[&[u8]]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    TlsConnector::from(Arc::new(config))
}

async fn hello() -> &'static str {
    "hello"
}

struct Server {
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<Instant>,
    cert_pem: String,
}

async fn start(app: App, http2: bool) -> Server {
    start_with(app, http2, |runtime| runtime).await
}

async fn start_with(
    app: App,
    http2: bool,
    configure: impl FnOnce(oas_rs::AppRuntime) -> oas_rs::AppRuntime,
) -> Server {
    let id = identity();
    let runtime = configure(app.build().unwrap());
    let tls = TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes())
        .unwrap()
        .enable_http2(http2);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let done = tokio::spawn(async move {
        runtime
            .serve_tls(listener, tls, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        Instant::now()
    });
    Server {
        addr,
        stop: Some(stop),
        done,
        cert_pem: id.cert_pem,
    }
}

fn app_with_hello() -> App {
    let mut app = App::new();
    app.get("/", hello);
    app
}

async fn handshake(
    server: &Server,
    alpn: &[&[u8]],
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    connector(&server.cert_pem, alpn)
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
}

#[tokio::test]
async fn alpn_negotiates_h2_when_enabled() {
    let server = start(app_with_hello(), true).await;
    let tls = handshake(&server, &[b"h2", b"http/1.1"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
}

#[tokio::test]
async fn alpn_falls_back_to_http11_for_clients_that_only_offer_it() {
    let server = start(app_with_hello(), true).await;
    let tls = handshake(&server, &[b"http/1.1"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
}

#[tokio::test]
async fn an_h2_only_client_is_rejected_when_http2_is_disabled() {
    let server = start(app_with_hello(), false).await;
    assert!(handshake(&server, &[b"h2"]).await.is_err());
    // The server keeps serving others.
    let tls = handshake(&server, &[b"http/1.1"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
}

type H2Sender = hyper::client::conn::http2::SendRequest<Full<Bytes>>;

async fn h2_client(server: &Server) -> H2Sender {
    let tls = handshake(server, &[b"h2"]).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender
}

fn get(path: &str) -> http::Request<Full<Bytes>> {
    http::Request::builder()
        .method("GET")
        .uri(format!("https://localhost{path}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}

async fn text(response: http::Response<hyper::body::Incoming>) -> (u16, http::Version, String) {
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        parts.status.as_u16(),
        parts.version,
        String::from_utf8(bytes.to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn serves_a_request_over_h2() {
    let server = start(app_with_hello(), true).await;
    let mut sender = h2_client(&server).await;
    let (status, version, body) = text(sender.send_request(get("/")).await.unwrap()).await;
    assert_eq!(
        (status, version, body.as_str()),
        (200, http::Version::HTTP_2, "hello")
    );
}

#[tokio::test]
async fn http11_clients_still_work_when_http2_is_enabled() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let server = start(app_with_hello(), true).await;
    let mut tls = handshake(&server, &[b"http/1.1"]).await.unwrap();
    tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    let _ = tls.read_to_string(&mut out).await;
    assert!(
        out.starts_with("HTTP/1.1 200") && out.ends_with("hello"),
        "{out}"
    );
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

#[tokio::test]
async fn many_concurrent_streams_share_one_connection() {
    let mut app = App::new();
    app.get("/slow", slow);
    let server = start(app, true).await;
    let sender = h2_client(&server).await;
    let started = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let mut sender = sender.clone();
        tasks.push(tokio::spawn(async move {
            text(sender.send_request(get("/slow")).await.unwrap()).await
        }));
    }
    for task in tasks {
        let (status, _, body) = task.await.unwrap();
        assert_eq!((status, body.as_str()), (200, "done"));
    }
    // 20 streams of 300 ms: concurrent means well under 20 * 300 ms.
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn status_and_method_semantics_match_http11_over_h2() {
    let server = start(app_with_hello(), true).await;
    let mut sender = h2_client(&server).await;
    let (status, _, _) = text(sender.send_request(get("/missing")).await.unwrap()).await;
    assert_eq!(status, 404);
    let post = http::Request::builder()
        .method("POST")
        .uri("https://localhost/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let (status, _, _) = text(sender.send_request(post).await.unwrap()).await;
    assert_eq!(status, 405);
    let head = http::Request::builder()
        .method("HEAD")
        .uri("https://localhost/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = sender.send_request(head).await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn a_large_json_body_round_trips_over_h2() {
    use oas_rs::Json;
    async fn echo(Json(value): Json<serde_json::Value>) -> Json<serde_json::Value> {
        Json(value)
    }
    let mut app = App::new();
    app.post("/echo", echo).body_limit(2 * 1024 * 1024);
    let server = start(app, true).await;
    let mut sender = h2_client(&server).await;
    let big = serde_json::json!({ "data": "x".repeat(512 * 1024) });
    let body = serde_json::to_vec(&big).unwrap();
    let request = http::Request::builder()
        .method("POST")
        .uri("https://localhost/echo")
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.clone())))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let echoed = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(echoed.len(), body.len());
}

#[tokio::test]
async fn middleware_runs_over_h2() {
    use oas_rs::{ApiError, BearerAuth};
    let mut app = app_with_hello();
    app.layer(BearerAuth::new(|token: String| async move {
        if token == "good" {
            Ok(())
        } else {
            Err(ApiError::missing("bad"))
        }
    }));
    let server = start(app, true).await;
    let mut sender = h2_client(&server).await;
    let (status, _, _) = text(sender.send_request(get("/")).await.unwrap()).await;
    assert_eq!(status, 401);
    let request = http::Request::builder()
        .uri("https://localhost/")
        .header("authorization", "Bearer good")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let (status, _, body) = text(sender.send_request(request).await.unwrap()).await;
    assert_eq!((status, body.as_str()), (200, "hello"));
}

#[tokio::test]
async fn shutdown_lets_an_in_flight_h2_stream_finish() {
    let mut app = App::new();
    app.get("/slow", slow);
    let mut server = start(app, true).await;
    let mut sender = h2_client(&server).await;
    let request =
        tokio::spawn(async move { text(sender.send_request(get("/slow")).await.unwrap()).await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let (status, _, body) = request.await.unwrap();
    assert_eq!((status, body.as_str()), (200, "done"));
    let returned_at = server.done.await.unwrap();
    assert!(returned_at.duration_since(shutdown_at) >= Duration::from_millis(100));
}

#[tokio::test]
async fn an_idle_h2_connection_does_not_block_shutdown() {
    let mut server = start(app_with_hello(), true).await;
    let mut sender = h2_client(&server).await;
    let (status, _, _) = text(sender.send_request(get("/")).await.unwrap()).await;
    assert_eq!(status, 200);
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let returned_at = tokio::time::timeout(Duration::from_secs(5), server.done)
        .await
        .expect("an idle h2 connection blocked shutdown")
        .unwrap();
    assert!(returned_at.duration_since(shutdown_at) < Duration::from_secs(2));
    drop(sender);
}

#[tokio::test]
async fn max_concurrent_streams_refuses_streams_over_the_limit() {
    let mut app = App::new();
    app.get("/slow", slow);
    let server = start_with(app, true, |runtime| {
        runtime.http2_max_concurrent_streams(Some(2))
    })
    .await;
    let sender = h2_client(&server).await;
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let mut sender = sender.clone();
        tasks.push(tokio::spawn(async move {
            match sender.send_request(get("/slow")).await {
                Ok(response) => Some(text(response).await.0),
                Err(_) => None,
            }
        }));
    }
    let mut served = 0;
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            Some(200) => served += 1,
            Some(other) => panic!("unexpected status {other}"),
            None => refused += 1,
        }
    }
    assert_eq!(served, 2, "exactly the allowed streams are served");
    assert_eq!(refused, 2, "the streams over the limit are refused");
    // The connection itself is still usable afterwards.
    let mut sender = sender.clone();
    let (status, _, _) = text(sender.send_request(get("/slow")).await.unwrap()).await;
    assert_eq!(status, 200);
}

#[test]
#[should_panic(expected = "at least 1")]
fn zero_concurrent_streams_is_rejected() {
    let _ = App::new()
        .build()
        .unwrap()
        .http2_max_concurrent_streams(Some(0));
}
