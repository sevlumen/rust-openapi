//! Fixtures shared by the Nagle / `TCP_NODELAY` tests (Linux only).

use std::{
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_core::Stream;
use oas_rs::StreamResponse;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A body stream fed by a channel.
pub struct Chunks(tokio::sync::mpsc::Receiver<Bytes>);

impl Stream for Chunks {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Bytes>> {
        self.0.poll_recv(context)
    }
}

/// A response written as three small chunks, 1 ms apart.
pub async fn streamed() -> StreamResponse<Chunks> {
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

/// Median time to read a complete streamed response over one keep-alive
/// connection (`GET /stream`).
pub async fn median_stream_latency<S>(stream: &mut S, iterations: usize) -> Duration
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut times = Vec::new();
    let mut buffer = [0u8; 1024];
    for _ in 0..iterations {
        let started = Instant::now();
        stream
            .write_all(b"GET /stream HTTP/1.1\r\nHost: localhost\r\n\r\n")
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

/// Asserts that streamed chunks are not stalled by Nagle's algorithm (a stall
/// is about 40 ms; a healthy median is a few ms). Wall-clock measurements can
/// be disturbed by a loaded machine, so up to three attempts are made and one
/// good median is enough. A real regression stalls every sample of every
/// attempt, so it still fails.
pub async fn assert_streamed_without_nagle_stall<S>(stream: &mut S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut medians = Vec::new();
    for _ in 0..3 {
        let median = median_stream_latency(stream, 21).await;
        if median < Duration::from_millis(25) {
            return;
        }
        medians.push(median);
    }
    panic!("medians {medians:?}: a Nagle/delayed-ACK stall (about 40 ms) is back");
}
