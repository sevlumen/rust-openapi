use bytes::Bytes;
use futures_core::Stream;
use http_body::{Body, Frame, SizeHint};
use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

/// The fixed or streaming response body used by the Hyper adapter.
pub enum ResponseBody {
    Full(Option<Bytes>),
    Stream(Pin<Box<dyn Stream<Item = Bytes> + Send + 'static>>),
}

impl ResponseBody {
    pub(crate) fn full(bytes: Bytes) -> Self {
        Self::Full(Some(bytes))
    }

    pub(crate) fn stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Bytes> + Send + 'static,
    {
        Self::Stream(Box::pin(stream))
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            Self::Full(body) => Poll::Ready(body.take().map(|bytes| Ok(Frame::data(bytes)))),
            Self::Stream(stream) => match stream.as_mut().poll_next(context) {
                Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Full(body) => body.is_none(),
            Self::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(body) => {
                let mut hint = SizeHint::new();
                let length = body.as_ref().map_or(0, Bytes::len);
                hint.set_exact(length as u64);
                hint
            }
            Self::Stream(_) => SizeHint::default(),
        }
    }
}
