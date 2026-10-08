//! Nagle's algorithm combined with delayed ACKs stalls a response that is
//! written in several small pieces by about 40 ms on Linux. The server sets
//! `TCP_NODELAY` on accepted connections by default to avoid that; these tests
//! pin the behavior with a streamed response whose chunks are written 1 ms
//! apart. The timing contrast is a Linux kernel behavior, so they only run
//! there. That the option reaches (or is withheld from) the accepted socket is
//! checked deterministically by `accepted_sockets_get_tcp_nodelay_only_when_asked`
//! in `src/runtime.rs`.
#![cfg(target_os = "linux")]

use std::{
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_core::Stream;
use oas_rs::{App, AppRuntime, StreamResponse};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

struct Chunks(tokio::sync::mpsc::Receiver<Bytes>);

impl Stream for Chunks {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Bytes>> {
        self.0.poll_recv(context)
    }
}

async fn streamed() -> StreamResponse<Chunks> {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        for part in ["aaaa", "bbbb", "cccc"] {
            if tx.send(Bytes::from_static(part.as_bytes())).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    StreamResponse(Chunks(rx))
}

fn runtime() -> AppRuntime {
    let mut app = App::new();
    app.get("/stream", streamed);
    app.build().unwrap()
}

async fn spawn_server(runtime: AppRuntime) -> (std::net::SocketAddr, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    (addr, stop)
}

/// Median time to read a complete 3-chunk streamed response on one keep-alive
/// connection (the client itself sets TCP_NODELAY).
async fn median_stream_latency(addr: std::net::SocketAddr, iterations: usize) -> Duration {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.set_nodelay(true).unwrap();
    let mut times = Vec::new();
    let mut buffer = [0u8; 1024];
    for _ in 0..iterations {
        let started = Instant::now();
        stream
            .write_all(b"GET /stream HTTP/1.1\r\nHost: t\r\n\r\n")
            .await
            .unwrap();
        let mut seen = Vec::new();
        while !seen.ends_with(b"0\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0, "connection closed early");
            seen.extend_from_slice(&buffer[..read]);
        }
        times.push(started.elapsed());
    }
    times.sort();
    times[times.len() / 2]
}

#[tokio::test]
async fn streamed_chunks_are_not_stalled_by_nagle_by_default() {
    let (addr, _stop) = spawn_server(runtime()).await;
    let median = median_stream_latency(addr, 21).await;
    assert!(
        median < Duration::from_millis(25),
        "median {median:?}: a Nagle/delayed-ACK stall (about 40 ms) is back"
    );
}
