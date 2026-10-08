use std::fmt;

use futures_util::{SinkExt, StreamExt};
use hyper::upgrade::OnUpgrade;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        self,
        handshake::derive_accept_key,
        protocol::{CloseFrame as WireCloseFrame, Message as WireMessage, Role, WebSocketConfig},
    },
};

use crate::*;

/// Messages larger than this end the session unless
/// [`WebSocketUpgrade::max_message_size`] says otherwise.
const DEFAULT_MAX_MESSAGE: usize = 1024 * 1024;

/// A WebSocket message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Bytes),
    /// A ping; the peer is answered with a pong automatically.
    Ping(Bytes),
    Pong(Bytes),
    /// The peer is closing (or, when sent, asks to close) the session.
    Close(Option<CloseFrame>),
}

/// The status code and reason of a close message.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CloseFrame {
    pub code: u16,
    pub reason: String,
}

impl CloseFrame {
    pub fn new(code: u16, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

/// A failure while reading from or writing to a WebSocket.
#[derive(Debug)]
pub struct WebSocketError(tungstenite::Error);

impl fmt::Display for WebSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for WebSocketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl WebSocketError {
    /// The idle timeout ([`WebSocketUpgrade::idle_timeout`]) expired.
    pub fn is_timeout(&self) -> bool {
        matches!(&self.0, tungstenite::Error::Io(error) if error.kind() == std::io::ErrorKind::TimedOut)
    }

    /// The session ended normally (the close handshake completed or the
    /// connection was closed).
    pub fn is_closed(&self) -> bool {
        matches!(
            self.0,
            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed
        )
    }
}

impl From<WireMessage> for Message {
    fn from(message: WireMessage) -> Self {
        match message {
            WireMessage::Text(text) => Message::Text(text.as_str().to_owned()),
            WireMessage::Binary(bytes) => Message::Binary(bytes),
            WireMessage::Ping(bytes) => Message::Ping(bytes),
            WireMessage::Pong(bytes) => Message::Pong(bytes),
            WireMessage::Close(frame) => Message::Close(frame.map(|frame| CloseFrame {
                code: u16::from(frame.code),
                reason: frame.reason.as_str().to_owned(),
            })),
            // `tungstenite` never yields raw frames when reading.
            WireMessage::Frame(_) => unreachable!("raw frames are not produced when reading"),
        }
    }
}

impl From<Message> for WireMessage {
    fn from(message: Message) -> Self {
        match message {
            Message::Text(text) => WireMessage::text(text),
            Message::Binary(bytes) => WireMessage::Binary(bytes),
            Message::Ping(bytes) => WireMessage::Ping(bytes),
            Message::Pong(bytes) => WireMessage::Pong(bytes),
            Message::Close(frame) => WireMessage::Close(frame.map(|frame| WireCloseFrame {
                code: frame.code.into(),
                reason: frame.reason.into(),
            })),
        }
    }
}

/// An open WebSocket session, handed to the handler given to
/// [`WebSocketUpgrade::on_upgrade`].
pub struct WebSocket {
    inner: WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>,
    idle: Option<Duration>,
}

impl WebSocket {
    /// The next message, or `None` once the session is over. Pings are
    /// answered automatically (they are still returned); an error ends the
    /// session.
    pub async fn recv(&mut self) -> Option<Result<Message, WebSocketError>> {
        let next = match self.idle {
            Some(idle) => match tokio::time::timeout(idle, self.inner.next()).await {
                Ok(next) => next,
                Err(_) => {
                    return Some(Err(WebSocketError(tungstenite::Error::Io(
                        std::io::Error::new(std::io::ErrorKind::TimedOut, "websocket idle timeout"),
                    ))));
                }
            },
            None => self.inner.next().await,
        };
        next.map(|result| result.map(Message::from).map_err(WebSocketError))
    }

    pub async fn send(&mut self, message: Message) -> Result<(), WebSocketError> {
        self.inner
            .send(message.into())
            .await
            .map_err(WebSocketError)
    }

    /// Starts the close handshake. Keep calling [`recv`](Self::recv) until it
    /// returns `None` to let it finish.
    pub async fn close(&mut self, frame: Option<CloseFrame>) -> Result<(), WebSocketError> {
        self.inner
            .close(frame.map(|frame| WireCloseFrame {
                code: frame.code.into(),
                reason: frame.reason.into(),
            }))
            .await
            .map_err(WebSocketError)
    }
}

/// The response of a WebSocket route: the `101 Switching Protocols` handshake,
/// or the rejection (`400`, `403`, `426`) for a request that cannot upgrade.
pub struct WebSocketResponse(HttpResponse);

impl IntoResponse for WebSocketResponse {
    fn into_response(self) -> HttpResponse {
        self.0
    }
}

impl ResponseMetadata for WebSocketResponse {
    fn status_code() -> StatusCode {
        StatusCode::SWITCHING_PROTOCOLS
    }
}

/// The server side of the WebSocket handshake. Use it in a raw route
/// (`App::raw`), which receives the `Request<Incoming>` an upgrade needs:
///
/// ```no_run
/// # use oas_rs::{App, Message, WebSocketUpgrade};
/// # use hyper::body::Incoming;
/// # let mut app = App::new();
/// app.raw(http::Method::GET, "/ws", |request: http::Request<Incoming>| async move {
///     WebSocketUpgrade::new(request)
///         .allow_origin("https://app.example")
///         .on_upgrade(|mut socket| async move {
///             while let Some(Ok(message)) = socket.recv().await {
///                 if let Message::Text(text) = message {
///                     let _ = socket.send(Message::Text(text)).await;
///                 }
///             }
///         })
/// });
/// ```
///
/// Layers (authentication, rate limits) run before the handshake, as for any
/// route. Notes:
///
/// - Browsers do not apply CORS to WebSockets: check the `Origin` with
///   [`allow_origin`](Self::allow_origin) for cookie-authenticated endpoints
///   (cross-site WebSocket hijacking). A request with no `Origin` header (not
///   a browser) is not blocked by that check.
/// - Messages above [`max_message_size`](Self::max_message_size) (1 MiB by
///   default) end the session.
/// - After the upgrade the connection belongs to the handler task: graceful
///   shutdown does not wait for it, so watch your own shutdown signal if open
///   sessions must be closed politely. The session keeps its
///   `max_connections` slot until the handler ends. `header_read_timeout` no
///   longer applies, so use [`idle_timeout`](Self::idle_timeout) (each session
///   also holds a 128 KiB read buffer).
/// - A panic in the handler stays on its own task (`CatchPanic` does not see
///   it): the socket is simply dropped.
/// - HTTP/1.1 only (no RFC 8441 WebSockets over HTTP/2) and no
///   `permessage-deflate`.
pub struct WebSocketUpgrade {
    request: Request<Incoming>,
    protocols: Vec<String>,
    origins: Vec<String>,
    max_message_size: usize,
    idle_timeout: Option<Duration>,
}

/// A subprotocol name is an HTTP token: no spaces, commas or controls.
fn validate_protocol(protocol: &str) {
    assert!(
        !protocol.is_empty()
            && protocol
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b',' && byte != b';'),
        "invalid WebSocket subprotocol name {protocol:?}"
    );
}

/// A client key is 16 random bytes in base64: 24 characters ending in `==`.
fn valid_key(key: &HeaderValue) -> bool {
    let key = key.as_bytes();
    key.len() == 24
        && key.ends_with(b"==")
        && key[..22]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

impl WebSocketUpgrade {
    pub fn new(request: Request<Incoming>) -> Self {
        Self {
            request,
            protocols: Vec::new(),
            origins: Vec::new(),
            max_message_size: DEFAULT_MAX_MESSAGE,
            idle_timeout: None,
        }
    }

    /// Accepts connections from this `Origin`, compared exactly: lowercase
    /// scheme and host, no trailing slash (`https://app.example`), as browsers
    /// send it. Once any origin is listed, a request whose `Origin` header is
    /// present but not listed gets `403`.
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.origins.push(origin.into());
        self
    }

    /// The subprotocols the server speaks, most preferred first: the first of
    /// them that the client also offered is selected, whatever order the
    /// client listed them in. When none matches the session is still accepted
    /// without a subprotocol (a conforming client then closes it).
    ///
    /// # Panics
    ///
    /// Panics if a name is not a valid token (empty, or containing spaces,
    /// commas or control characters).
    pub fn protocols<I, T>(mut self, protocols: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.protocols = protocols.into_iter().map(Into::into).collect();
        self.protocols
            .iter()
            .for_each(|name| validate_protocol(name));
        self
    }

    /// Ends the session when no message arrives for this long:
    /// [`WebSocket::recv`] then returns an error for which
    /// [`WebSocketError::is_timeout`] is true. Without it a silent peer holds
    /// the session forever (hyper's header timeout does not apply after the
    /// upgrade). Send pings from the client, or have the handler ping, to keep
    /// a quiet session alive.
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// The largest message (after reassembling fragments) the session
    /// accepts; the default is 1 MiB.
    pub fn max_message_size(mut self, bytes: usize) -> Self {
        self.max_message_size = bytes;
        self
    }

    /// Completes the handshake and runs `handler` with the open session on its
    /// own task. Returns the response to send; a request that is not a valid
    /// handshake or comes from a disallowed origin yields its rejection
    /// instead and `handler` never runs.
    pub fn on_upgrade<F, Fut>(self, handler: F) -> WebSocketResponse
    where
        F: FnOnce(WebSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Self {
            mut request,
            protocols,
            origins,
            max_message_size,
            idle_timeout,
        } = self;
        let headers = request.headers();
        let contains = |name: header::HeaderName, token: &str| {
            headers
                .get_all(name)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        };
        if request.method() != Method::GET
            || request.version() != http::Version::HTTP_11
            || !contains(header::CONNECTION, "upgrade")
            || !contains(header::UPGRADE, "websocket")
        {
            return rejection(ApiError::bad_request("expected a WebSocket handshake"));
        }
        if headers
            .get("sec-websocket-version")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            != Some("13")
        {
            let mut response = ApiError::new(
                StatusCode::UPGRADE_REQUIRED,
                "Upgrade Required",
                "WebSocket version 13 is required",
            )
            .into_response();
            response
                .headers_mut()
                .insert("sec-websocket-version", HeaderValue::from_static("13"));
            return WebSocketResponse(response);
        }
        let Some(key) = headers
            .get("sec-websocket-key")
            .filter(|value| valid_key(value))
            .cloned()
        else {
            return rejection(ApiError::bad_request(
                "missing or invalid Sec-WebSocket-Key",
            ));
        };
        if !origins.is_empty()
            && let Some(origin) = headers.get(header::ORIGIN)
            && !origin
                .to_str()
                .is_ok_and(|origin| origins.iter().any(|allowed| allowed == origin))
        {
            return rejection(ApiError::new(
                StatusCode::FORBIDDEN,
                "Forbidden",
                "this origin may not open a WebSocket here",
            ));
        }
        let offered: Vec<&str> = headers
            .get_all("sec-websocket-protocol")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .collect();
        let chosen_protocol = protocols
            .iter()
            .find(|ours| offered.contains(&ours.as_str()))
            .cloned();
        // The connection's slot, so the session keeps counting toward
        // `max_connections` after the HTTP connection has been handed over.
        let permit = request
            .extensions()
            .get::<crate::runtime::ConnectionPermit>()
            .cloned();
        let accept = derive_accept_key(key.as_bytes());
        let on_upgrade: OnUpgrade = hyper::upgrade::on(&mut request);
        let config = WebSocketConfig::default()
            .max_message_size(Some(max_message_size))
            .max_frame_size(Some(max_message_size));
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(upgraded) = on_upgrade.await else {
                return;
            };
            let io = hyper_util::rt::TokioIo::new(upgraded);
            let inner = WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
            handler(WebSocket {
                inner,
                idle: idle_timeout,
            })
            .await;
        });
        let mut builder = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header("sec-websocket-accept", accept);
        if let Some(protocol) = chosen_protocol {
            builder = builder.header("sec-websocket-protocol", protocol);
        }
        WebSocketResponse(
            builder
                .body(ResponseBody::full(Bytes::new()))
                .expect("a valid handshake response"),
        )
    }
}

fn rejection(error: ApiError) -> WebSocketResponse {
    WebSocketResponse(error.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "subprotocol")]
    fn a_subprotocol_name_with_a_space_is_rejected() {
        validate_protocol("bad protocol");
    }

    #[test]
    fn plain_tokens_are_valid_subprotocols() {
        validate_protocol("chat");
        validate_protocol("v2.json-patch");
    }

    #[test]
    fn keys_must_be_sixteen_bytes_of_base64() {
        assert!(valid_key(&HeaderValue::from_static(
            "dGhlIHNhbXBsZSBub25jZQ=="
        )));
        assert!(!valid_key(&HeaderValue::from_static("abc")));
        assert!(!valid_key(&HeaderValue::from_static(
            "dGhlIHNhbXBsZSBub25jZQ!!"
        )));
    }
}
