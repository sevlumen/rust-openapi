//! An HTTPS server using a throwaway self-signed certificate.
//!
//! ```text
//! cargo run --example tls --features tls
//! curl -k https://localhost:8443/
//! ```
//!
//! In production load a real certificate and key with
//! `TlsConfig::from_pem_files("cert.pem", "key.pem")`.

use std::time::Duration;

use oas_rs::{App, TlsConfig};

async fn hello() -> &'static str {
    "hello over TLS\n"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let tls = TlsConfig::from_pem(
        certified.cert.pem().as_bytes(),
        certified.signing_key.serialize_pem().as_bytes(),
    )?;

    let mut app = App::new();
    app.get("/", hello);
    let runtime = app.build()?.handshake_timeout(Duration::from_secs(10));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8443").await?;
    println!("listening on https://localhost:8443 (self-signed certificate: use curl -k)");
    runtime
        .serve_tls(listener, tls, async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await
}
