use oas_rs::{App, Json};
use serde::Serialize;

#[derive(Clone, Serialize)]
struct User {
    id: u64,
    name: &'static str,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let implementation = std::env::var("OAS_IMPLEMENTATION").unwrap_or_else(|_| "oas".to_owned());
    let address = std::env::var("OAS_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    if implementation == "raw" {
        return raw_server(&address).await;
    }
    let static_json = Json(User { id: 1, name: "Alice" });
    let mut app = App::new().title("oas-rs benchmark").version("0.1.0");
    app.get("/plaintext", || async { "OK" });
    app.get("/json-static", move || {
        let value = static_json.clone();
        async move { value }
    });
    app.openapi("/openapi.json").swagger("/swagger");
    app.listen(&address).await
}

async fn raw_server(address: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use bytes::Bytes;
    use http::{Request, Response, StatusCode};
    use http_body_util::Full;
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;

    let listener = tokio::net::TcpListener::bind(address).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = hyper::service::service_fn(|request: Request<Incoming>| async move {
                let response = if request.uri().path() == "/plaintext" {
                    Response::builder().status(StatusCode::OK).body(Full::new(Bytes::from_static(b"OK"))).unwrap()
                } else {
                    Response::builder().status(StatusCode::NOT_FOUND).body(Full::new(Bytes::from_static(b"Not Found"))).unwrap()
                };
                Ok::<_, std::convert::Infallible>(response)
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await;
        });
    }
}
