use std::fmt;

use futures_core::Stream;

use crate::*;

/// One server-sent event. `Display` gives the wire form.
#[derive(Clone, Debug, Default)]
pub struct Event {
    event: Option<String>,
    id: Option<String>,
    retry: Option<Duration>,
    data: Option<String>,
    comment: Option<String>,
}

/// Removes line breaks, which would end a field early.
fn one_line(value: &str) -> String {
    value.chars().filter(|c| *c != '\n' && *c != '\r').collect()
}

impl Event {
    /// An event carrying `data`; line breaks in it become several `data:` lines.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            data: Some(data.into()),
            ..Self::default()
        }
    }

    /// A comment line, ignored by clients (useful as a heartbeat).
    pub fn comment(text: impl AsRef<str>) -> Self {
        Self {
            comment: Some(one_line(text.as_ref())),
            ..Self::default()
        }
    }

    /// The event name (`event:`). Line breaks are removed.
    pub fn event(mut self, name: impl AsRef<str>) -> Self {
        self.event = Some(one_line(name.as_ref()));
        self
    }

    /// The event id (`id:`), sent back as `Last-Event-ID` on reconnect. Line
    /// breaks are removed.
    pub fn id(mut self, id: impl AsRef<str>) -> Self {
        self.id = Some(one_line(id.as_ref()));
        self
    }

    /// How long the client waits before reconnecting, in whole milliseconds.
    pub fn retry(mut self, delay: Duration) -> Self {
        self.retry = Some(delay);
        self
    }
}

impl fmt::Display for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(comment) = &self.comment {
            writeln!(formatter, ": {comment}")?;
        }
        if let Some(event) = &self.event {
            writeln!(formatter, "event: {event}")?;
        }
        if let Some(id) = &self.id {
            writeln!(formatter, "id: {id}")?;
        }
        if let Some(retry) = self.retry {
            writeln!(formatter, "retry: {}", retry.as_millis())?;
        }
        if let Some(data) = &self.data {
            // A lone `\r` and `\r\n` end a line as well as `\n`.
            let normalized = data.replace("\r\n", "\n").replace('\r', "\n");
            for line in normalized.split('\n') {
                writeln!(formatter, "data: {line}")?;
            }
        }
        writeln!(formatter)
    }
}

/// A response that streams [`Event`]s (`text/event-stream`). The connection
/// stays open until the stream ends or the client leaves.
///
/// Proxies may buffer or time out idle streams: send a
/// [`keep_alive`](Self::keep_alive). [`Compress`] leaves streams alone, and
/// `header_read_timeout` does not apply to an open stream.
pub struct Sse<S> {
    events: S,
    keep_alive: Option<Duration>,
}

impl<S> Sse<S> {
    pub fn new(events: S) -> Self {
        Self {
            events,
            keep_alive: None,
        }
    }

    /// Sends a `: keep-alive` comment whenever no event was sent for
    /// `interval`.
    ///
    /// # Panics
    ///
    /// Panics if `interval` is zero.
    pub fn keep_alive(mut self, interval: Duration) -> Self {
        assert!(
            !interval.is_zero(),
            "the keep-alive interval must not be zero"
        );
        self.keep_alive = Some(interval);
        self
    }
}

/// Events as bytes, with optional heartbeat comments in the gaps.
struct EventBytes<S> {
    events: Pin<Box<S>>,
    keep_alive: Option<Duration>,
    ticker: Option<tokio::time::Interval>,
}

impl<S: Stream<Item = Event>> Stream for EventBytes<S> {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Bytes>> {
        let this = &mut *self;
        if let Poll::Ready(item) = this.events.as_mut().poll_next(context) {
            if let Some(ticker) = &mut this.ticker {
                ticker.reset();
            }
            return Poll::Ready(item.map(|event| Bytes::from(event.to_string())));
        }
        if let Some(period) = this.keep_alive {
            let ticker = this.ticker.get_or_insert_with(|| {
                let mut ticker =
                    tokio::time::interval_at(tokio::time::Instant::now() + period, period);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                ticker
            });
            if ticker.poll_tick(context).is_ready() {
                return Poll::Ready(Some(Bytes::from_static(b": keep-alive\n\n")));
            }
        }
        Poll::Pending
    }
}

impl<S> IntoResponse for Sse<S>
where
    S: Stream<Item = Event> + Send + 'static,
{
    fn into_response(self) -> HttpResponse {
        let stream = EventBytes {
            events: Box::pin(self.events),
            keep_alive: self.keep_alive,
            ticker: None,
        };
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(ResponseBody::stream(stream))
            .expect("a valid event-stream response")
    }
}

/// Documented as a plain `200`: the schema of a stream is not expressible.
impl<S> ResponseMetadata for Sse<S> where S: Stream<Item = Event> + Send + 'static {}
