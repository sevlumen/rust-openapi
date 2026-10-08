use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use oas_rs::{App, Event, Sse};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};

struct Events(mpsc::Receiver<Event>);

impl futures_core::Stream for Events {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Event>> {
        self.0.poll_recv(context)
    }
}

#[test]
fn events_are_formatted_per_the_spec() {
    assert_eq!(Event::data("hello").to_string(), "data: hello\n\n");
    assert_eq!(
        Event::data("a\nb\r\nc").to_string(),
        "data: a\ndata: b\ndata: c\n\n"
    );
    assert_eq!(
        Event::data("x")
            .event("update")
            .id("7")
            .retry(Duration::from_millis(1500))
            .to_string(),
        "event: update\nid: 7\nretry: 1500\ndata: x\n\n"
    );
    assert_eq!(Event::comment("ping").to_string(), ": ping\n\n");
    assert_eq!(
        Event::data("x").id("a\0b").to_string(),
        "id: ab\ndata: x\n\n"
    );
    assert_eq!(Event::data("").to_string(), "data: \n\n");
}

#[test]
fn newlines_cannot_be_smuggled_into_names_and_ids() {
    let text = Event::data("x").event("a\nb").id("1\r2").to_string();
    assert!(!text.contains("\nb") && !text.contains("\r"), "{text:?}");
    assert_eq!(text.matches("\n\n").count(), 1, "{text:?}");
}

async fn serve(app: App) -> (std::net::SocketAddr, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let runtime = app.build().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await;
    });
    (addr, stop)
}

async fn read_until(stream: &mut TcpStream, needle: &str, limit: Duration) -> String {
    let mut out = String::new();
    let mut buffer = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + limit;
    while !out.contains(needle) {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, stream.read(&mut buffer)).await {
            Ok(Ok(n)) if n > 0 => out.push_str(&String::from_utf8_lossy(&buffer[..n])),
            _ => break,
        }
    }
    out
}

#[tokio::test]
async fn events_arrive_as_they_are_sent_with_the_right_headers() {
    let (tx, rx) = mpsc::channel(8);
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
    let mut app = App::new();
    app.get("/events", move || {
        let slot = std::sync::Arc::clone(&slot);
        async move {
            let rx = slot.lock().unwrap().take().expect("one client");
            Sse::new(Events(rx))
        }
    });
    let (addr, stop) = serve(app).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    tx.send(Event::data("first")).await.unwrap();
    let head = read_until(&mut stream, "data: first", Duration::from_secs(3)).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/event-stream"),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("cache-control: no-cache"),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase().contains("x-accel-buffering: no"),
        "{head}"
    );
    assert!(
        head.contains("data: first"),
        "the event must arrive before the stream ends: {head}"
    );
    // The stream is still open: a later event still comes through.
    tx.send(Event::data("second")).await.unwrap();
    let more = read_until(&mut stream, "data: second", Duration::from_secs(3)).await;
    assert!(more.contains("data: second"), "{more}");
    drop(tx);
    let _ = stop.send(());
}

#[tokio::test]
async fn keep_alive_comments_fill_the_silence() {
    let (_tx, rx) = mpsc::channel(8);
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
    let mut app = App::new();
    app.get("/events", move || {
        let slot = std::sync::Arc::clone(&slot);
        async move {
            let rx = slot.lock().unwrap().take().expect("one client");
            Sse::new(Events(rx)).keep_alive(Duration::from_millis(100))
        }
    });
    let (addr, stop) = serve(app).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /events HTTP/1.1\r\nHost: t\r\n\r\n")
        .await
        .unwrap();
    let seen = read_until(&mut stream, ": keep-alive", Duration::from_secs(3)).await;
    assert!(seen.contains(": keep-alive"), "{seen}");
    let _ = stop.send(());
}

#[test]
fn the_helper_types_are_usable() {
    let _ = Bytes::new();
}
