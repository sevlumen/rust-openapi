use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use http::Request;
use hyper::body::Incoming;
use oas_rs::{App, BearerAuth, Message, Method, WebSocketResponse, WebSocketUpgrade};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_tungstenite::tungstenite::{
    self, client::IntoClientRequest, protocol::Message as ClientMessage,
};

type Client = tokio_tungstenite::WebSocketStream<TcpStream>;

/// Echoes text and binary messages until the peer closes.
fn echo(upgrade: WebSocketUpgrade) -> WebSocketResponse {
    upgrade.on_upgrade(|mut socket| async move {
        while let Some(message) = socket.recv().await {
            match message {
                Ok(Message::Text(text)) => {
                    if socket.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Binary(bytes)) => {
                    if socket.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
}

async fn serve(
    app: App,
) -> (
    std::net::SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let runtime = app
        .build()
        .unwrap()
        .shutdown_timeout(Duration::from_millis(500));
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let _ = runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await;
    });
    (addr, stop, server)
}

fn app_with(
    configure: impl Fn(WebSocketUpgrade) -> WebSocketUpgrade + Clone + Send + Sync + 'static,
) -> App {
    let mut app = App::new();
    app.raw(Method::GET, "/ws", move |request: Request<Incoming>| {
        let configure = configure.clone();
        async move { echo(configure(WebSocketUpgrade::new(request))) }
    });
    app
}

async fn connect(
    addr: std::net::SocketAddr,
    headers: &[(&'static str, &'static str)],
) -> Result<(Client, http::Response<Option<Vec<u8>>>), tungstenite::Error> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(
            http::HeaderName::from_static(name),
            http::HeaderValue::from_static(value),
        );
    }
    tokio_tungstenite::client_async(request, tcp).await
}

#[tokio::test]
async fn text_and_binary_messages_are_echoed() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade)).await;
    let (mut client, response) = connect(addr, &[]).await.unwrap();
    assert_eq!(response.status(), 101);
    client.send(ClientMessage::text("hello")).await.unwrap();
    assert_eq!(
        client.next().await.unwrap().unwrap(),
        ClientMessage::text("hello")
    );
    client
        .send(ClientMessage::binary(vec![1u8, 2, 3]))
        .await
        .unwrap();
    assert_eq!(
        client.next().await.unwrap().unwrap(),
        ClientMessage::binary(vec![1u8, 2, 3])
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn pings_are_answered_with_pongs() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade)).await;
    let (mut client, _) = connect(addr, &[]).await.unwrap();
    client
        .send(ClientMessage::Ping(vec![9u8, 9].into()))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(3), client.next())
        .await
        .expect("no pong")
        .unwrap()
        .unwrap();
    assert_eq!(reply, ClientMessage::Pong(vec![9u8, 9].into()));
    let _ = stop.send(());
}

#[tokio::test]
async fn the_close_handshake_ends_the_session() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade)).await;
    let (mut client, _) = connect(addr, &[]).await.unwrap();
    client.close(None).await.unwrap();
    // The server answers the close and the stream ends.
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = client.next().await {
            if message.is_err() {
                break;
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the session did not end after close");
    let _ = stop.send(());
}

#[tokio::test]
async fn only_allowed_origins_may_connect_when_a_list_is_set() {
    let (addr, stop, _server) = serve(app_with(|upgrade| {
        upgrade.allow_origin("https://app.example")
    }))
    .await;
    let denied = connect(addr, &[("origin", "https://evil.example")])
        .await
        .expect_err("a foreign origin must be refused");
    match denied {
        tungstenite::Error::Http(response) => assert_eq!(response.status(), 403),
        other => panic!("unexpected error: {other:?}"),
    }
    let (_ok, response) = connect(addr, &[("origin", "https://app.example")])
        .await
        .unwrap();
    assert_eq!(response.status(), 101);
    // Clients that send no Origin (not browsers) are not blocked.
    assert!(connect(addr, &[]).await.is_ok());
    let _ = stop.send(());
}

#[tokio::test]
async fn a_subprotocol_is_negotiated() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade.protocols(["chat", "v2"]))).await;
    let (_client, response) = connect(addr, &[("sec-websocket-protocol", "other, v2")])
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("sec-websocket-protocol").unwrap(),
        "v2"
    );
    // No offered protocol matches: the server answers without one, and a
    // conforming client then gives up (the session is not usable for it).
    assert!(
        connect(addr, &[("sec-websocket-protocol", "unknown")])
            .await
            .is_err()
    );
    // A client that offers none gets a plain session.
    let (_client, response) = connect(addr, &[]).await.unwrap();
    assert!(response.headers().get("sec-websocket-protocol").is_none());
    let _ = stop.send(());
}

#[tokio::test]
async fn oversized_messages_end_the_session() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade.max_message_size(1024))).await;
    let (mut client, _) = connect(addr, &[]).await.unwrap();
    client
        .send(ClientMessage::text("x".repeat(100_000)))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match client.next().await {
                Some(Ok(ClientMessage::Text(_))) => panic!("an oversized message was echoed"),
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => break,
            }
        }
    })
    .await;
    assert!(outcome.is_ok(), "the server kept the session open");
    let _ = stop.send(());
}

async fn plain_get(addr: std::net::SocketAddr, extra: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET /ws HTTP/1.1\r\nHost: t\r\nConnection: close\r\n{extra}\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut out = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut out)).await;
    out
}

#[tokio::test]
async fn a_request_that_is_not_a_handshake_is_refused() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade)).await;
    let plain = plain_get(addr, "").await;
    assert!(plain.starts_with("HTTP/1.1 400"), "{plain}");
    let wrong_version = plain_get(
        addr,
        "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n",
    )
    .await;
    assert!(wrong_version.starts_with("HTTP/1.1 426"), "{wrong_version}");
    assert!(
        wrong_version
            .to_ascii_lowercase()
            .contains("sec-websocket-version: 13"),
        "{wrong_version}"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn layers_run_before_the_upgrade() {
    let mut app = app_with(|upgrade| upgrade);
    app.layer(BearerAuth::static_token("secret"));
    let (addr, stop, _server) = serve(app).await;
    let denied = connect(addr, &[])
        .await
        .expect_err("authentication must run first");
    match denied {
        tungstenite::Error::Http(response) => assert_eq!(response.status(), 401),
        other => panic!("unexpected error: {other:?}"),
    }
    let (_client, response) = connect(addr, &[("authorization", "Bearer secret")])
        .await
        .unwrap();
    assert_eq!(response.status(), 101);
    let _ = stop.send(());
}

#[tokio::test]
async fn an_open_websocket_does_not_hold_up_server_shutdown() {
    let (addr, stop, server) = serve(app_with(|upgrade| upgrade)).await;
    let (_client, _) = connect(addr, &[]).await.unwrap();
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("serve_listener did not return")
        .unwrap();
}

#[tokio::test]
async fn an_http_1_0_handshake_is_refused() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade)).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"GET /ws HTTP/1.0\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .await
        .unwrap();
    let mut out = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut out)).await;
    assert!(out.contains(" 400 "), "{out}");
    let _ = stop.send(());
}

#[tokio::test]
async fn a_malformed_key_is_refused() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade)).await;
    let plain = plain_get(
        addr,
        "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: abc\r\nSec-WebSocket-Version: 13\r\n",
    )
    .await;
    assert!(plain.starts_with("HTTP/1.1 400"), "{plain}");
    let _ = stop.send(());
}

#[tokio::test]
async fn the_servers_subprotocol_preference_wins() {
    let (addr, stop, _server) = serve(app_with(|upgrade| upgrade.protocols(["chat", "v2"]))).await;
    for offered in ["v2, chat", "chat, v2"] {
        let (_client, response) = connect(addr, &[("sec-websocket-protocol", offered)])
            .await
            .unwrap();
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            "chat",
            "{offered}"
        );
    }
    let _ = stop.send(());
}

#[tokio::test]
async fn error_format_keeps_the_version_header_of_a_426() {
    let mut app = app_with(|upgrade| upgrade);
    app.layer(oas_rs::ErrorFormat::new(|info: &oas_rs::ErrorInfo| {
        info.respond(serde_json::json!({ "error": info.detail }))
    }));
    let (addr, stop, _server) = serve(app).await;
    let response = plain_get(
        addr,
        "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 426"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("sec-websocket-version: 13"),
        "{response}"
    );
    assert!(response.contains("\"error\""), "{response}");
    let _ = stop.send(());
}

#[tokio::test]
async fn an_open_websocket_keeps_its_connection_slot() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut app = app_with(|upgrade| upgrade);
    app.get("/health", health);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let runtime = app.build().unwrap().max_connections(Some(1));
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = runtime
            .serve_listener(listener, async {
                let _ = stopped.await;
            })
            .await;
    });
    let (client, _) = connect(addr, &[]).await.unwrap();
    // The only slot belongs to the open WebSocket: a second client waits.
    let mut second = TcpStream::connect(addr).await.unwrap();
    second
        .write_all(b"GET /health HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    let waited =
        tokio::time::timeout(Duration::from_millis(400), second.read_to_string(&mut out)).await;
    assert!(
        waited.is_err(),
        "a connection over the limit was served: {out}"
    );
    // Closing the WebSocket frees the slot.
    drop(client);
    let served =
        tokio::time::timeout(Duration::from_secs(3), second.read_to_string(&mut out)).await;
    assert!(served.is_ok() && out.starts_with("HTTP/1.1 200"), "{out}");
    let _ = stop.send(());
}

async fn health() -> &'static str {
    "ok"
}

#[tokio::test]
async fn an_idle_timeout_ends_a_silent_session() {
    let (addr, stop, _server) = serve(app_with(|upgrade| {
        upgrade.idle_timeout(Duration::from_millis(200))
    }))
    .await;
    let (mut client, _) = connect(addr, &[]).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = client.next().await {
            if message.is_err() {
                break;
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "a silent session was kept open");
    let _ = stop.send(());
}

#[test]
fn the_handshake_route_is_documented_as_101() {
    let mut app = App::new();
    app.raw(
        Method::GET,
        "/ws",
        |request: Request<Incoming>| async move {
            WebSocketUpgrade::new(request).on_upgrade(|_socket| async {})
        },
    );
    let doc = app.openapi_document();
    assert_eq!(
        doc["paths"]["/ws"]["get"]["responses"]["101"]["description"],
        "Switching Protocols"
    );
}
