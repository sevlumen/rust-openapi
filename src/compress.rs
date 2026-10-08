use std::io::Write;

use flate2::{Compression, write::GzEncoder};

use crate::*;

/// Bodies above this size are compressed on the blocking thread pool so a
/// large response does not stall the async worker.
const BLOCKING_THRESHOLD: usize = 64 * 1024;

/// Gzip response compression.
///
/// Compresses buffered text-like responses (`text/*`, JSON, XML, JavaScript,
/// SVG) of at least [`min_size`](Self::min_size) bytes for clients whose
/// `Accept-Encoding` allows gzip, and sets `Content-Encoding`, an adjusted
/// `Content-Length` and a weak `ETag`. Streaming responses, `HEAD` and bodiless
/// statuses, responses that already have a `Content-Encoding`, and other
/// media types (images, archives) pass through unchanged. A response whose
/// type could have been compressed always carries `Vary: Accept-Encoding`, so
/// caches keep both variants apart.
///
/// Register it before layers whose responses you want compressed, and keep
/// it away from endpoints that carry secrets next to attacker-chosen input
/// over TLS (BREACH).
#[derive(Clone)]
pub struct Compress {
    min_size: usize,
    level: u32,
}

impl Compress {
    /// Default minimum size 1024 bytes, level 6.
    pub fn new() -> Self {
        Self {
            min_size: 1024,
            level: 6,
        }
    }

    /// Bodies smaller than this are sent as they are.
    pub fn min_size(mut self, bytes: usize) -> Self {
        self.min_size = bytes;
        self
    }

    /// Gzip level, 0 (store) to 9 (smallest).
    ///
    /// # Panics
    ///
    /// Panics if `level` is above 9.
    pub fn level(mut self, level: u32) -> Self {
        assert!(level <= 9, "the gzip level must be 0 to 9");
        self.level = level;
        self
    }
}

impl Default for Compress {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `Accept-Encoding` allows gzip: an explicit `gzip` entry wins over
/// `*`, and a quality of 0 forbids.
fn accepts_gzip(value: &str) -> bool {
    let mut gzip = None;
    let mut any = None;
    for entry in value.split(',') {
        let mut parts = entry.split(';');
        let coding = parts.next().unwrap_or_default().trim();
        let quality = parts
            .find_map(|param| {
                let (name, value) = param.split_once('=')?;
                name.trim()
                    .eq_ignore_ascii_case("q")
                    .then(|| value.trim().parse::<f32>().ok())
                    .flatten()
            })
            .unwrap_or(1.0);
        if coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("x-gzip") {
            gzip = Some(quality);
        } else if coding == "*" {
            any = Some(quality);
        }
    }
    gzip.or(any).is_some_and(|quality| quality > 0.0)
}

fn compressible(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type.starts_with("text/")
        || matches!(
            media_type.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "image/svg+xml"
        )
        || media_type.ends_with("+json")
        || media_type.ends_with("+xml")
}

fn add_vary(headers: &mut http::HeaderMap) {
    let present = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| {
            let token = token.trim();
            token == "*" || token.eq_ignore_ascii_case("accept-encoding")
        });
    if !present {
        headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
}

fn gzip(bytes: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
    // Writing to a Vec cannot fail.
    let _ = encoder.write_all(bytes);
    encoder.finish().unwrap_or_default()
}

impl Middleware for Compress {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let accepts = request
            .headers()
            .get_all(header::ACCEPT_ENCODING)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(accepts_gzip);
        let is_head = request.method() == Method::HEAD;
        let (min_size, level) = (self.min_size, self.level);
        Box::pin(async move {
            let mut response = next.run(request).await;
            let status = response.status();
            if is_head
                || status.is_informational()
                || status == StatusCode::NO_CONTENT
                || status == StatusCode::NOT_MODIFIED
                || status == StatusCode::PARTIAL_CONTENT
                || response.headers().contains_key(header::CONTENT_ENCODING)
                || response.headers().contains_key(header::CONTENT_RANGE)
            {
                return response;
            }
            let is_compressible = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(compressible);
            if !is_compressible {
                return response;
            }
            add_vary(response.headers_mut());
            if !accepts {
                return response;
            }
            let ResponseBody::Full(slot) = response.body_mut() else {
                return response;
            };
            let Some(body) = slot.as_ref().filter(|body| body.len() >= min_size.max(1)) else {
                return response;
            };
            let original = body.clone();
            let compressed = if original.len() > BLOCKING_THRESHOLD {
                let source = original.clone();
                match tokio::task::spawn_blocking(move || gzip(&source, level)).await {
                    Ok(bytes) => bytes,
                    Err(_) => return response,
                }
            } else {
                gzip(&original, level)
            };
            if compressed.is_empty() || compressed.len() >= original.len() {
                return response;
            }
            let length = compressed.len();
            *response.body_mut() = ResponseBody::full(Bytes::from(compressed));
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            if headers.contains_key(header::CONTENT_LENGTH) {
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            }
            if let Some(etag) = headers.get(header::ETAG).cloned()
                && !etag.as_bytes().starts_with(b"W/")
            {
                let mut weak = b"W/".to_vec();
                weak.extend_from_slice(etag.as_bytes());
                if let Ok(weak) = HeaderValue::from_bytes(&weak) {
                    headers.insert(header::ETAG, weak);
                }
            }
            response
        })
    }
}
