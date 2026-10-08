use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use futures_core::Stream;
use tokio::io::{AsyncRead, ReadBuf};

use crate::*;

/// Files up to this size are read into memory in one go; larger ones are
/// streamed in chunks.
const SMALL_FILE: u64 = 64 * 1024;
const CHUNK: usize = 64 * 1024;

struct Config {
    /// The URL prefix, as path segments (empty for `/`).
    prefix: Vec<String>,
    /// The canonical directory being served.
    root: PathBuf,
    index: Option<String>,
    cache_control: Option<HeaderValue>,
    dotfiles: bool,
}

/// Serves files from a directory under a URL prefix, as a middleware: a
/// `GET` or `HEAD` whose path is under the prefix and names a file is
/// answered here, anything else (other methods, missing files, other paths)
/// goes on to the application, so routes and static files can share a prefix.
///
/// ```no_run
/// # use oas_rs::{App, ServeDir};
/// let mut app = App::new();
/// app.layer(ServeDir::new("/assets", "./public").cache_control("public, max-age=3600"));
/// ```
///
/// Responses carry `Content-Type` (by extension), `Content-Length`, a weak
/// `ETag` and `Last-Modified`, and `If-None-Match` / `If-Modified-Since` get
/// `304`. Large files are streamed. Not supported: `Range` requests (the
/// whole file is sent), precompressed variants, directory listings. A
/// directory serves its index file (`index.html` by default).
///
/// Safety: `..` segments, backslashes, NUL, drive-letter colons and hidden
/// files (names starting with `.`, unless
/// [`allow_dotfiles`](Self::allow_dotfiles)) are refused after percent-decoding,
/// and the resolved path must stay inside the directory, so a symlink pointing
/// out of it is not followed.
#[derive(Clone)]
pub struct ServeDir {
    config: Arc<Config>,
}

impl ServeDir {
    /// Serves `directory` under `prefix` (`/` serves it at the root).
    ///
    /// # Panics
    ///
    /// Panics if `directory` does not exist or is not a directory: a typo in
    /// a path should fail at start-up, not answer `404` forever.
    pub fn new(prefix: &str, directory: impl AsRef<Path>) -> Self {
        let directory = directory.as_ref();
        let root = std::fs::canonicalize(directory)
            .unwrap_or_else(|error| panic!("ServeDir: cannot use {directory:?}: {error}"));
        assert!(root.is_dir(), "ServeDir: {directory:?} is not a directory");
        Self {
            config: Arc::new(Config {
                prefix: PathParts::new(prefix)
                    .map(|part| part.value.to_owned())
                    .collect(),
                root,
                index: Some("index.html".to_owned()),
                cache_control: None,
                dotfiles: false,
            }),
        }
    }

    fn edit(mut self, change: impl FnOnce(&mut Config)) -> Self {
        let config = &mut self.config;
        // The config is shared only after `new`, so cloning here is cheap.
        let mut owned = Config {
            prefix: config.prefix.clone(),
            root: config.root.clone(),
            index: config.index.clone(),
            cache_control: config.cache_control.clone(),
            dotfiles: config.dotfiles,
        };
        change(&mut owned);
        self.config = Arc::new(owned);
        self
    }

    /// The file a directory request serves (default `index.html`).
    pub fn index_file(self, name: &str) -> Self {
        let name = name.to_owned();
        self.edit(|config| config.index = Some(name))
    }

    /// A `Cache-Control` value to send with every file.
    ///
    /// # Panics
    ///
    /// Panics if `value` is not a valid header value.
    pub fn cache_control(self, value: &str) -> Self {
        let value = HeaderValue::from_str(value).expect("a valid Cache-Control value");
        self.edit(|config| config.cache_control = Some(value))
    }

    /// Serve files and directories whose name starts with `.` (off by
    /// default: `.env`, `.git` and friends stay private).
    pub fn allow_dotfiles(self, allow: bool) -> Self {
        self.edit(|config| config.dotfiles = allow)
    }
}

impl Config {
    /// The percent-decoded path segments below the prefix, or `None` when the
    /// request is outside the prefix or names something unsafe.
    fn relative(&self, path: &str) -> Option<Vec<String>> {
        let mut parts = PathParts::new(path);
        for expected in &self.prefix {
            if parts.next()?.value != expected {
                return None;
            }
        }
        let mut segments = Vec::new();
        for raw in parts {
            let segment = percent_decode(raw.value).ok()?;
            let unsafe_segment = segment == "."
                || segment == ".."
                || segment.contains(['/', '\\', '\0', ':'])
                || segment.chars().any(char::is_control)
                || (!self.dotfiles && segment.starts_with('.'));
            if unsafe_segment {
                // Not servable; let the application answer (normally 404).
                return None;
            }
            segments.push(segment);
        }
        Some(segments)
    }

    async fn resolve(&self, segments: &[String]) -> Option<(PathBuf, std::fs::Metadata)> {
        let mut path = self.root.clone();
        path.extend(segments);
        let mut path = self.contained(path).await?;
        let mut metadata = tokio::fs::metadata(&path).await.ok()?;
        if metadata.is_dir() {
            path = self.contained(path.join(self.index.as_ref()?)).await?;
            metadata = tokio::fs::metadata(&path).await.ok()?;
        }
        metadata.is_file().then_some((path, metadata))
    }

    /// Canonicalizes `path` and requires it to be inside the root.
    async fn contained(&self, path: PathBuf) -> Option<PathBuf> {
        let canonical = tokio::fs::canonicalize(path).await.ok()?;
        canonical.starts_with(&self.root).then_some(canonical)
    }
}

fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

/// Streams a file in chunks, ending after `remaining` bytes.
struct FileChunks {
    file: tokio::fs::File,
    remaining: u64,
}

impl Stream for FileChunks {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Bytes>> {
        if self.remaining == 0 {
            return Poll::Ready(None);
        }
        let size = CHUNK.min(self.remaining as usize);
        let mut buffer = vec![0u8; size];
        let mut read_buffer = ReadBuf::new(&mut buffer);
        match Pin::new(&mut self.file).poll_read(context, &mut read_buffer) {
            Poll::Ready(Ok(())) => {
                let filled = read_buffer.filled().len();
                if filled == 0 {
                    // The file shrank: end the body early (the length mismatch
                    // makes the connection close, which is what we want).
                    self.remaining = 0;
                    return Poll::Ready(None);
                }
                buffer.truncate(filled);
                self.remaining -= filled as u64;
                Poll::Ready(Some(Bytes::from(buffer)))
            }
            Poll::Ready(Err(_)) => {
                self.remaining = 0;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

struct Validators {
    if_none_match: Option<String>,
    if_modified_since: Option<String>,
}

fn weak_tag(tag: &str) -> &str {
    let tag = tag.trim();
    tag.strip_prefix("W/").unwrap_or(tag).trim()
}

fn not_modified(validators: &Validators, etag: &str, modified: std::time::SystemTime) -> bool {
    if let Some(list) = &validators.if_none_match {
        return list
            .split(',')
            .any(|candidate| candidate.trim() == "*" || weak_tag(candidate) == weak_tag(etag));
    }
    if let Some(since) = validators
        .if_modified_since
        .as_deref()
        .and_then(|value| httpdate::parse_http_date(value).ok())
    {
        // HTTP dates have whole-second precision.
        let seconds = |time: std::time::SystemTime| {
            time.duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        };
        return seconds(modified) <= seconds(since);
    }
    false
}

async fn serve(
    config: &Config,
    segments: Vec<String>,
    validators: Validators,
    head: bool,
) -> Option<HttpResponse> {
    let (path, metadata) = config.resolve(&segments).await?;
    let length = metadata.len();
    let modified = metadata.modified().ok();
    let etag = modified.map(|modified| {
        let seconds = modified
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        format!("W/\"{length:x}-{seconds:x}\"")
    });
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, content_type(&path))
        .header("x-content-type-options", "nosniff");
    if let Some(cache_control) = &config.cache_control {
        builder = builder.header(header::CACHE_CONTROL, cache_control.clone());
    }
    if let Some(etag) = &etag {
        builder = builder.header(header::ETAG, etag.as_str());
    }
    if let Some(modified) = modified {
        builder = builder.header(header::LAST_MODIFIED, httpdate::fmt_http_date(modified));
    }
    if let (Some(etag), Some(modified)) = (&etag, modified)
        && not_modified(&validators, etag, modified)
    {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(ResponseBody::full(Bytes::new()))
            .ok();
    }
    builder = builder
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, length);
    let body = if head {
        ResponseBody::full(Bytes::new())
    } else if length <= SMALL_FILE {
        ResponseBody::full(Bytes::from(tokio::fs::read(&path).await.ok()?))
    } else {
        let file = tokio::fs::File::open(&path).await.ok()?;
        ResponseBody::stream(FileChunks {
            file,
            remaining: length,
        })
    };
    builder.body(body).ok()
}

impl Middleware for ServeDir {
    fn handle(&self, request: Request<RequestBody>, next: Next) -> BoxFuture<HttpResponse> {
        let head = request.method() == Method::HEAD;
        if !head && request.method() != Method::GET {
            return next.run(request);
        }
        let Some(segments) = self.config.relative(request.uri().path()) else {
            return next.run(request);
        };
        let text = |name: header::HeaderName| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let validators = Validators {
            if_none_match: text(header::IF_NONE_MATCH),
            if_modified_since: text(header::IF_MODIFIED_SINCE),
        };
        let config = Arc::clone(&self.config);
        Box::pin(async move {
            match serve(&config, segments, validators, head).await {
                Some(response) => response,
                None => next.run(request).await,
            }
        })
    }
}
