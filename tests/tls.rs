use oas_rs::{TlsConfig, TlsError};

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

#[test]
fn loads_a_pkcs8_pem_pair() {
    let id = identity();
    TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
}

#[test]
fn loads_a_pem_pair_from_files() {
    let id = identity();
    let dir = std::env::temp_dir().join(format!("oas-rs-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert, &id.cert_pem).unwrap();
    std::fs::write(&key, &id.key_pem).unwrap();
    TlsConfig::from_pem_files(&cert, &key).unwrap();
    let missing = TlsConfig::from_pem_files(dir.join("nope.pem"), &key);
    assert!(missing.is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rejects_a_mismatched_key_and_certificate() {
    let (a, b) = (identity(), identity());
    let error: TlsError = TlsConfig::from_pem(a.cert_pem.as_bytes(), b.key_pem.as_bytes())
        .expect_err("mismatched pair must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn rejects_empty_unparsable_and_key_only_input() {
    let id = identity();
    assert!(TlsConfig::from_pem(b"", id.key_pem.as_bytes()).is_err()); // no certificate
    assert!(TlsConfig::from_pem(id.cert_pem.as_bytes(), b"").is_err()); // no key
    assert!(TlsConfig::from_pem(b"not pem at all", b"also not pem").is_err());
    assert!(TlsConfig::from_pem(id.key_pem.as_bytes(), id.key_pem.as_bytes()).is_err()); // key where a cert is expected
}

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use oas_rs::App;
use rustls_pki_types::{CertificateDer, ServerName, pem::PemObject};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, SupportedProtocolVersion, crypto::ring, version},
};

async fn hello() -> &'static str {
    "hello"
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

struct Server {
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<Instant>,
    cert_pem: String,
}

async fn start(handshake_timeout: Option<Duration>) -> Server {
    let id = identity();
    let mut app = App::new();
    app.get("/", hello);
    app.get("/slow", slow);
    let mut runtime = app.build().unwrap();
    if let Some(timeout) = handshake_timeout {
        runtime = runtime.handshake_timeout(timeout);
    }
    let tls = TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
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

fn connector(cert_pem: &str, versions: &[&'static SupportedProtocolVersion]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

async fn https_get(
    server: &Server,
    versions: &[&'static SupportedProtocolVersion],
    path: &str,
) -> String {
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let mut tls = connector(&server.cert_pem, versions)
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    tls.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut out = String::new();
    // A close without close_notify is reported as an error after the data; keep what was read.
    let _ = tls.read_to_string(&mut out).await;
    out
}

#[tokio::test]
async fn serves_https_with_tls_13_and_tls_12() {
    let server = start(None).await;
    for versions in [&[&version::TLS13][..], &[&version::TLS12][..]] {
        let out = https_get(&server, versions, "/").await;
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert!(out.ends_with("hello"), "{out}");
    }
}

#[tokio::test]
async fn keep_alive_works_over_one_tls_connection() {
    let server = start(None).await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let mut tls = connector(&server.cert_pem, &[&version::TLS13])
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    for _ in 0..2 {
        tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0u8; 256];
        while !seen.ends_with(b"hello") {
            let read = tls.read(&mut buffer).await.unwrap();
            assert!(read > 0, "connection closed early");
            seen.extend_from_slice(&buffer[..read]);
        }
        assert!(String::from_utf8_lossy(&seen).starts_with("HTTP/1.1 200"));
    }
}

#[tokio::test]
async fn garbage_and_plain_http_do_not_hurt_the_server() {
    // A short handshake timeout makes the empty-payload case meaningful: the
    // silent connection is dropped by the server instead of burning the read
    // timeout below.
    let server = start(Some(Duration::from_millis(200))).await;
    for payload in [
        &b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"[..],
        &[0u8, 1, 2, 3, 255, 254][..],
        &b""[..],
    ] {
        let mut tcp = TcpStream::connect(server.addr).await.unwrap();
        tcp.write_all(payload).await.unwrap();
        let mut sink = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), tcp.read_to_end(&mut sink)).await;
    }
    let out = https_get(&server, &[&version::TLS13], "/").await;
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
}

#[tokio::test]
async fn a_stalled_handshake_times_out_without_blocking_others() {
    let server = start(Some(Duration::from_millis(200))).await;
    let mut stalled = TcpStream::connect(server.addr).await.unwrap(); // sends nothing
    // Others are served while the stalled connection is open.
    let out = https_get(&server, &[&version::TLS13], "/").await;
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    // The stalled connection is closed by the server after the timeout.
    let mut buffer = [0u8; 16];
    let closed = tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut buffer))
        .await
        .expect("stalled handshake was never closed");
    assert!(matches!(closed, Ok(0) | Err(_)));
}

#[tokio::test]
async fn shutdown_lets_an_in_flight_https_request_finish() {
    let mut server = start(None).await;
    let addr = server.addr;
    let cert = server.cert_pem.clone();
    let client = tokio::spawn(async move {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut tls = connector(&cert, &[&version::TLS13])
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        tls.write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        let _ = tls.read_to_string(&mut out).await;
        out
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let out = client.await.unwrap();
    let returned_at = server.done.await.unwrap();
    assert!(
        out.starts_with("HTTP/1.1 200") && out.ends_with("done"),
        "{out}"
    );
    assert!(returned_at.duration_since(shutdown_at) >= Duration::from_millis(100));
}

#[tokio::test]
async fn a_pending_handshake_does_not_hold_shutdown_past_the_timeout() {
    let mut server = start(None).await;
    let _stalled = TcpStream::connect(server.addr).await.unwrap(); // never handshakes
    tokio::time::sleep(Duration::from_millis(50)).await;
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let returned_at = tokio::time::timeout(Duration::from_secs(5), server.done)
        .await
        .expect("a pending handshake blocked shutdown")
        .unwrap();
    assert!(returned_at.duration_since(shutdown_at) < Duration::from_secs(2));
}

#[tokio::test]
async fn an_idle_keep_alive_https_connection_does_not_block_shutdown() {
    let mut server = start(None).await;
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let mut tls = connector(&server.cert_pem, &[&version::TLS13])
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut buffer = [0u8; 256];
    while !seen.ends_with(b"hello") {
        let read = tls.read(&mut buffer).await.unwrap();
        assert!(read > 0, "connection closed early");
        seen.extend_from_slice(&buffer[..read]);
    }
    // The connection stays open and idle while the server shuts down.
    let shutdown_at = Instant::now();
    server.stop.take().unwrap().send(()).unwrap();
    let returned_at = tokio::time::timeout(Duration::from_secs(5), server.done)
        .await
        .expect("an idle keep-alive connection blocked shutdown")
        .unwrap();
    assert!(returned_at.duration_since(shutdown_at) < Duration::from_secs(2));
    drop(tls);
}

// Streamed chunks over TLS must not be stalled by Nagle's algorithm either
// (Linux kernel behavior; see tests/tcp_nodelay.rs).
#[cfg(target_os = "linux")]
mod common;

#[cfg(target_os = "linux")]
mod nodelay {
    use super::*;

    #[tokio::test]
    async fn streamed_chunks_over_tls_are_not_stalled_by_nagle() {
        let id = identity();
        let mut app = App::new();
        app.get("/stream", crate::common::streamed);
        let runtime = app.build().unwrap();
        let tls = TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(async move {
            runtime
                .serve_tls(listener, tls, async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });

        let tcp = TcpStream::connect(addr).await.unwrap();
        tcp.set_nodelay(true).unwrap();
        let mut tls = connector(&id.cert_pem, &[&version::TLS13])
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        crate::common::assert_streamed_without_nagle_stall(&mut tls).await;
    }
}

#[test]
fn tls_error_keeps_its_source_so_a_missing_file_is_distinguishable() {
    let id = identity();
    let dir = std::env::temp_dir().join(format!("oas-rs-tls-src-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("key.pem");
    std::fs::write(&key, &id.key_pem).unwrap();

    let missing = TlsConfig::from_pem_files(dir.join("nope.pem"), &key).unwrap_err();
    let io_error = std::error::Error::source(&missing)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .expect("a missing file keeps its io::Error as the source");
    assert_eq!(io_error.kind(), std::io::ErrorKind::NotFound);

    let malformed = TlsConfig::from_pem(b"not pem", id.key_pem.as_bytes()).unwrap_err();
    let from_io = std::error::Error::source(&malformed)
        .and_then(|source| source.downcast_ref::<std::io::Error>());
    assert!(from_io.is_none(), "malformed PEM is not an io error");

    std::fs::remove_dir_all(&dir).unwrap();
}
