use futures_core::Stream;

use crate::*;

/// A one-item stream over an already buffered body.
struct OnceBody(Option<Bytes>);

impl Stream for OnceBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take().filter(|bytes| !bytes.is_empty()).map(Ok))
    }
}

fn map_error(error: multer::Error) -> ApiError {
    match error {
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Payload Too Large",
                error.to_string(),
            )
        }
        other => ApiError::bad_request(format!("invalid multipart body: {other}")),
    }
}

/// A `multipart/form-data` request body. Extract it in a handler and read the
/// parts with [`next_field`](Self::next_field). The body is buffered up to the
/// route's size limit (see `App::body_limit`).
pub struct Multipart {
    inner: multer::Multipart<'static>,
}

impl Multipart {
    /// The next part, or `None` after the last one.
    pub async fn next_field(&mut self) -> Result<Option<Field>, ApiError> {
        self.inner
            .next_field()
            .await
            .map(|field| field.map(|inner| Field { inner }))
            .map_err(map_error)
    }
}

/// One part of a multipart body.
pub struct Field {
    inner: multer::Field<'static>,
}

impl Field {
    pub fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    /// The client-supplied file name, returned verbatim (never used by the
    /// framework).
    pub fn file_name(&self) -> Option<&str> {
        self.inner.file_name()
    }

    pub fn content_type(&self) -> Option<&str> {
        self.inner.content_type().map(|mime| mime.as_ref())
    }

    pub async fn bytes(self) -> Result<Bytes, ApiError> {
        self.inner.bytes().await.map_err(map_error)
    }

    pub async fn text(self) -> Result<String, ApiError> {
        self.inner.text().await.map_err(map_error)
    }
}

impl<S: Send + Sync + 'static> FromRequest<S> for Multipart {
    const NEEDS_BODY: bool = true;

    fn openapi_request() -> OpenApiRequest {
        OpenApiRequest {
            request_body: Some(json!({
                "required": true,
                "content": {
                    "multipart/form-data": {
                        "schema": {
                            "type": "object",
                            "additionalProperties": { "type": "string", "format": "binary" }
                        }
                    }
                }
            })),
            ..OpenApiRequest::default()
        }
    }

    fn from_request(
        request: &mut Request<Bytes>,
        _params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let boundary = multer::parse_boundary(content_type).map_err(|error| match error {
            multer::Error::NoBoundary => {
                ApiError::bad_request("multipart Content-Type has no boundary")
            }
            _ => ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Media Type",
                "expected multipart/form-data",
            ),
        })?;
        let body = std::mem::take(request.body_mut());
        Ok(Multipart {
            inner: multer::Multipart::new(OnceBody(Some(body)), boundary),
        })
    }
}
