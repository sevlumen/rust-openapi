use std::io::Write;

use flate2::{Compression, write::GzEncoder};

use crate::*;

/// Bodies above this size are compressed on the blocking thread pool so a
/// large response does not stall the async worker.
const BLOCKING_THRESHOLD: usize = 16 * 1024;

/// Gzip (and, with the `compression-brotli` feature, brotli) response
/// compression.
///
/// Compresses buffered text-like responses (`text/*`, JSON, XML, JavaScript,
/// SVG) of at least [`min_size`](Self::min_size) bytes for clients whose
/// `Accept-Encoding` allows an encoding (the highest quality wins, brotli on
/// a tie), and sets `Content-Encoding`, an adjusted
/// `Content-Length` and a weak `ETag` (a `HEAD` mirrors this, minus the
/// length). Streaming responses and bodiless
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
    #[cfg(feature = "compression-brotli")]
    brotli_quality: u32,
}

impl Compress {
    /// Default minimum size 1024 bytes, level 6.
    pub fn new() -> Self {
        Self {
            min_size: 1024,
            level: 6,
            #[cfg(feature = "compression-brotli")]
            brotli_quality: 4,
        }
    }

    /// Bodies smaller than this are sent as they are. Whatever is set here,
    /// bodies under 32 bytes are never compressed (a gzip stream has more
    /// overhead than that, so they cannot shrink); this keeps `HEAD`, whose
    /// body is gone before this layer runs and which therefore decides from
    /// the length alone, in agreement with `GET`. A larger body that does not
    /// shrink (already-compressed or random-looking text) is still sent as
    /// it is by `GET`, and a `HEAD` for it announces the encoding anyway.
    pub fn min_size(mut self, bytes: usize) -> Self {
        self.min_size = bytes;
        self
    }

    /// Brotli quality, 0 (fastest) to 11 (smallest, slow). The default 4 is a
    /// good trade for responses compressed on the fly.
    ///
    /// # Panics
    ///
    /// Panics if `quality` is above 11.
    #[cfg(feature = "compression-brotli")]
    pub fn brotli_quality(mut self, quality: u32) -> Self {
        assert!(quality <= 11, "the brotli quality must be 0 to 11");
        self.brotli_quality = quality;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Encoding {
    Gzip,
    #[cfg(feature = "compression-brotli")]
    Brotli,
}

impl Encoding {
    fn token(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            #[cfg(feature = "compression-brotli")]
            Self::Brotli => "br",
        }
    }
}

/// Picks the encoding `Accept-Encoding` prefers among the ones this build can
/// produce: the highest quality above 0 wins and brotli wins a tie. An
/// explicit entry beats `*`; a quality of 0 forbids.
fn negotiate(value: &str) -> Option<Encoding> {
    let mut entries: Vec<(String, f32)> = Vec::new();
    for entry in value.split(',') {
        let mut parts = entry.split(';');
        let coding = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
        let quality = parts
            .find_map(|param| {
                let (name, value) = param.split_once('=')?;
                name.trim()
                    .eq_ignore_ascii_case("q")
                    .then(|| value.trim().parse::<f32>().ok())
                    .flatten()
            })
            .unwrap_or(1.0);
        entries.push((coding, quality));
    }
    let quality_of = |names: &[&str]| -> Option<f32> {
        entries
            .iter()
            .rev()
            .find(|(coding, _)| names.contains(&coding.as_str()))
            .or_else(|| entries.iter().rev().find(|(coding, _)| coding == "*"))
            .map(|(_, quality)| *quality)
    };
    let mut best: Option<(Encoding, f32)> = None;
    let mut consider = |encoding: Encoding, quality: Option<f32>| {
        if let Some(quality) = quality.filter(|quality| *quality > 0.0)
            && best.is_none_or(|(_, current)| quality > current)
        {
            best = Some((encoding, quality));
        }
    };
    // Brotli first so that it wins ties (`>` keeps the earlier candidate).
    #[cfg(feature = "compression-brotli")]
    consider(Encoding::Brotli, quality_of(&["br"]));
    consider(Encoding::Gzip, quality_of(&["gzip", "x-gzip"]));
    best.map(|(encoding, _)| encoding)
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

#[cfg(feature = "compression-brotli")]
fn brotli_compress(bytes: &[u8], quality: u32) -> Vec<u8> {
    let mut writer = brotli::CompressorWriter::new(Vec::new(), 4096, quality, 22);
    // Writing to a Vec cannot fail.
    let _ = writer.write_all(bytes);
    writer.into_inner()
}

/// Below this a gzip stream is never smaller than its input.
const MIN_COMPRESSIBLE: usize = 32;

fn gzip(bytes: &[u8], level: u32) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
    // Writing to a Vec cannot fail.
    let _ = encoder.write_all(bytes);
    encoder.finish().unwrap_or_default()
}

/// A compressed representation is not byte-identical to the original, so a
/// strong validator becomes weak.
fn weaken_etag(headers: &mut http::HeaderMap) {
    if let Some(etag) = headers.get(header::ETAG).cloned()
        && !etag.as_bytes().starts_with(b"W/")
    {
        let mut weak = b"W/".to_vec();
        weak.extend_from_slice(etag.as_bytes());
        if let Ok(weak) = HeaderValue::from_bytes(&weak) {
            headers.insert(header::ETAG, weak);
        }
    }
}

impl Middleware for Compress {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        // Several header lines mean one comma-separated list.
        let accept_encoding = request
            .headers()
            .get_all(header::ACCEPT_ENCODING)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>()
            .join(",");
        let encoding = negotiate(&accept_encoding);
        let is_head = request.method() == Method::HEAD;
        let (min_size, level) = (self.min_size, self.level);
        #[cfg(feature = "compression-brotli")]
        let brotli_quality = self.brotli_quality;
        Box::pin(async move {
            let mut response = next.run(request).await;
            let status = response.status();
            let encoded = response.headers().contains_key(header::CONTENT_ENCODING);
            let is_compressible = !encoded
                && response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(compressible);
            if !is_compressible {
                return response;
            }
            // The representation depends on `Accept-Encoding` even when this
            // response is not compressed (HEAD, 304, a client without gzip).
            add_vary(response.headers_mut());
            let no_transform = response
                .headers()
                .get_all(header::CACHE_CONTROL)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .any(|directive| directive.trim().eq_ignore_ascii_case("no-transform"));
            let Some(encoding) = encoding else {
                return response;
            };
            if no_transform
                || status.is_informational()
                || status == StatusCode::NO_CONTENT
                || status == StatusCode::NOT_MODIFIED
                || status == StatusCode::PARTIAL_CONTENT
                || response.headers().contains_key(header::CONTENT_RANGE)
            {
                return response;
            }
            if is_head {
                // The router already dropped the body, so the compressed size
                // is unknown: describe the GET (encoding, weak validator) and
                // omit the length, which a HEAD response may do.
                let large_enough = response
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<usize>().ok())
                    .is_some_and(|length| length >= min_size.max(MIN_COMPRESSIBLE));
                if large_enough {
                    let headers = response.headers_mut();
                    headers.remove(header::CONTENT_LENGTH);
                    headers.insert(
                        header::CONTENT_ENCODING,
                        HeaderValue::from_static(encoding.token()),
                    );
                    weaken_etag(headers);
                }
                return response;
            }
            let ResponseBody::Full(slot) = response.body_mut() else {
                return response;
            };
            let Some(body) = slot
                .as_ref()
                .filter(|body| body.len() >= min_size.max(MIN_COMPRESSIBLE))
            else {
                return response;
            };
            let original = body.clone();
            let compress = move |bytes: &[u8]| match encoding {
                Encoding::Gzip => gzip(bytes, level),
                #[cfg(feature = "compression-brotli")]
                Encoding::Brotli => brotli_compress(bytes, brotli_quality),
            };
            let compressed = if original.len() > BLOCKING_THRESHOLD {
                let source = original.clone();
                match tokio::task::spawn_blocking(move || compress(&source)).await {
                    Ok(bytes) => bytes,
                    Err(_) => return response,
                }
            } else {
                compress(&original)
            };
            if compressed.is_empty() || compressed.len() >= original.len() {
                return response;
            }
            let length = compressed.len();
            *response.body_mut() = ResponseBody::full(Bytes::from(compressed));
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_ENCODING,
                HeaderValue::from_static(encoding.token()),
            );
            if headers.contains_key(header::CONTENT_LENGTH) {
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            }
            weaken_etag(headers);
            response
        })
    }
}
