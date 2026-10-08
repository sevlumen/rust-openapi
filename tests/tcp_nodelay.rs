//! Nagle's algorithm combined with delayed ACKs stalls a response that is
//! written in several small pieces by about 40 ms on Linux. The server sets
//! `TCP_NODELAY` on accepted connections by default to avoid that; this test
//! pins the behavior with a streamed response whose chunks are written 1 ms
//! apart. The timing contrast is a Linux kernel behavior, so it only runs
//! there. That the option reaches (or is withheld from) the accepted socket is
//! checked deterministically by `accepted_sockets_get_tcp_nodelay_only_when_asked`
//! in `src/runtime.rs`.
#![cfg(target_os = "linux")]

mod common;

use oas_rs::App;
use tokio::{net::TcpListener, net::TcpStream, sync::oneshot};

#[tokio::test]
async fn streamed_chunks_are_not_stalled_by_nagle_by_default() {
    let mut app = App::new();
    app.get("/stream", common::streamed);
    let runtime = app.build().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });

    // The client sets TCP_NODELAY itself so only the server side is measured.
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.set_nodelay(true).unwrap();
    common::assert_streamed_without_nagle_stall(&mut stream).await;
}
