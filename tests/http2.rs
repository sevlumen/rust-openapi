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
    let id = identity();
    let runtime = app.build().unwrap();
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
