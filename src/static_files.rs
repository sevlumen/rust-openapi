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
/// whole file is sent, with `Accept-Ranges: none`), precompressed variants,
/// directory listings. A directory serves its index file (`index.html` by
/// default); a directory requested without a trailing slash is redirected
/// (`308`) to the slashed URL so relative links in the page work.
///
/// Safety: `..` segments, backslashes, NUL, drive-letter colons and hidden
/// files (names starting with `.`, unless
/// [`allow_dotfiles`](Self::allow_dotfiles)) are refused after percent-decoding,
/// and the resolved path must stay inside the directory and not name anything
/// hidden, so neither a symlink pointing out of it or at a dotfile, nor a
/// Windows 8.3 short name, gets through. Paths are compared by the file
/// system's own rules (`/assets/APP.CSS` works on Windows), and a file swapped
/// between the check and the read is a limitation shared by every file server.
///
/// **Deployment model.** The checks run on the resolved path before the file is
/// opened, so a symlink or file that someone replaces in between is not caught
/// (a race, not a flaw in the checks). Serve a directory that only trusted
/// processes can write, as a read-only deployment artifact; do not point
/// `ServeDir` at a directory that untrusted users can upload to or link into.
///
/// **Order matters.** The layer runs before routing, so a file under the
/// prefix wins over an application route with the same path (mount it under
/// a prefix of its own, ideally scoped with `app.layer_for("/assets", ..)` so
/// other requests skip it), and layers registered after it (authentication,
/// rate limits, `Cors`) do not run for served files: register `Cors` first if
/// fonts are fetched cross-origin.
#[derive(Clone)]
#[must_use = "middleware does nothing until it is registered with `App::layer`"]
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

    async fn resolve(&self, segments: &[String]) -> Option<Resolved> {
        let mut path = self.root.clone();
        path.extend(segments);
        let mut path = self.contained(path).await?;
        let mut metadata = tokio::fs::metadata(&path).await.ok()?;
        let mut via_directory = false;
        if metadata.is_dir() {
            via_directory = true;
            path = self.contained(path.join(self.index.as_ref()?)).await?;
            metadata = tokio::fs::metadata(&path).await.ok()?;
        }
        metadata.is_file().then_some(Resolved {
            path,
            metadata,
            via_directory,
        })
    }

    /// Canonicalizes `path` and requires it to be inside the root and, unless
    /// dotfiles are allowed, to name nothing hidden. The check runs on the
    /// resolved path so a symlink, hard link or Windows 8.3 short name
    /// (`SECRET~1`) cannot reach a hidden file the request did not name.
    async fn contained(&self, path: PathBuf) -> Option<PathBuf> {
        let canonical = tokio::fs::canonicalize(path).await.ok()?;
        let relative = canonical.strip_prefix(&self.root).ok()?;
        if !self.dotfiles
            && relative
                .components()
                .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
        {
            return None;
        }
        Some(canonical)
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
        "webmanifest" => "application/manifest+json",
        "xhtml" => "application/xhtml+xml",
        "bmp" => "image/bmp",
        "eot" => "application/vnd.ms-fontobject",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "jsonld" => "application/ld+json",
        "rss" | "atom" => "application/xml",
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

struct Resolved {
    path: PathBuf,
    metadata: std::fs::Metadata,
    /// The request named a directory and its index file was chosen.
    via_directory: bool,
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
    redirect_to: Option<String>,
) -> Option<HttpResponse> {
    let Resolved {
        path,
        metadata,
        via_directory,
    } = config.resolve(&segments).await?;
    if via_directory && let Some(location) = redirect_to {
        // `/dir` -> `/dir/`: relative links in the index page need the slash.
        return Response::builder()
            .status(StatusCode::PERMANENT_REDIRECT)
            .header(header::LOCATION, location)
            .header(header::CONTENT_LENGTH, "0")
            .body(ResponseBody::full(Bytes::new()))
            .ok();
    }
    // The file is opened once and described by that handle, so a file replaced
    // while serving cannot give one file's length with another's contents.
    let mut file = tokio::fs::File::open(&path).await.ok()?;
    let metadata = match file.metadata().await {
        Ok(opened) if opened.is_file() => opened,
        _ => metadata,
    };
    let mut length = metadata.len();
    let modified = metadata.modified().ok();
    // Size and the modification time to the nanosecond the file system keeps,
    // so a same-size rewrite within one second still changes the validator.
    // (`Last-Modified` and `If-Modified-Since` stay at HTTP-date precision,
    // which is why `If-None-Match` takes precedence.)
    let make_etag = |length: u64| {
        modified.map(|modified| {
            let nanos = modified
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            format!("W/\"{length:x}-{nanos:x}\"")
        })
    };
    let mut etag = make_etag(length);
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, content_type(&path))
        .header(header::ACCEPT_RANGES, "none")
        .header("x-content-type-options", "nosniff");
    if let Some(cache_control) = &config.cache_control {
        builder = builder.header(header::CACHE_CONTROL, cache_control.clone());
    }
    if let Some(modified) = modified {
        builder = builder.header(header::LAST_MODIFIED, httpdate::fmt_http_date(modified));
    }
    if let (Some(etag), Some(modified)) = (&etag, modified)
        && not_modified(&validators, etag, modified)
    {
        // A 304 repeats the validators (and cache headers) the 200 would carry.
        return builder
            .header(header::ETAG, etag.as_str())
            .status(StatusCode::NOT_MODIFIED)
            .body(ResponseBody::full(Bytes::new()))
            .ok();
    }
    let body = if head {
        ResponseBody::full(Bytes::new())
    } else if length <= SMALL_FILE {
        // Length and validator describe the bytes actually read (never more
        // than the size the handle reported, even if the file grew since).
        let mut bytes = Vec::with_capacity(length as usize);
        tokio::io::AsyncReadExt::read_to_end(
            &mut tokio::io::AsyncReadExt::take(&mut file, length),
            &mut bytes,
        )
        .await
        .ok()?;
        length = bytes.len() as u64;
        etag = make_etag(length);
        ResponseBody::full(Bytes::from(bytes))
    } else {
        ResponseBody::stream(FileChunks {
            file,
            remaining: length,
        })
    };
    if let Some(etag) = &etag {
        builder = builder.header(header::ETAG, etag.as_str());
    }
    builder
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, length)
        .body(body)
        .ok()
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
        // Where a directory request without a trailing slash is sent.
        // Built from the normalized segments, never the raw path: `//host/dir`
        // must not become the protocol-relative `//host/dir/`.
        let path = request.uri().path();
        let redirect_to = (!path.ends_with('/')).then(|| {
            let mut location = String::from("/");
            for part in PathParts::new(path) {
                location.push_str(part.value);
                location.push('/');
            }
            if let Some(query) = request.uri().query() {
                location.push('?');
                location.push_str(query);
            }
            location
        });
        let config = Arc::clone(&self.config);
        Box::pin(async move {
            match serve(&config, segments, validators, head, redirect_to).await {
                Some(response) => response,
                None => next.run(request).await,
            }
        })
    }
}
