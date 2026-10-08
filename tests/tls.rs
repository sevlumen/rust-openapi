use oas_rs::{TlsConfig, TlsError};

pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn identity() -> Identity {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    Identity {
        cert_pem: certified.cert.pem(),
        key_pem: certified.signing_key.serialize_pem(),
    }
}

#[test]
fn loads_a_pkcs8_pem_pair() {
    let id = identity();
    TlsConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
}

#[test]
fn loads_a_pem_pair_from_files() {
    let id = identity();
    let dir = std::env::temp_dir().join(format!("oas-rs-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert, &id.cert_pem).unwrap();
    std::fs::write(&key, &id.key_pem).unwrap();
    TlsConfig::from_pem_files(&cert, &key).unwrap();
    let missing = TlsConfig::from_pem_files(dir.join("nope.pem"), &key);
    assert!(missing.is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rejects_a_mismatched_key_and_certificate() {
    let (a, b) = (identity(), identity());
    let error: TlsError = TlsConfig::from_pem(a.cert_pem.as_bytes(), b.key_pem.as_bytes())
        .err()
        .expect("mismatched pair must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn rejects_empty_unparsable_and_key_only_input() {
    let id = identity();
    assert!(TlsConfig::from_pem(b"", id.key_pem.as_bytes()).is_err()); // no certificate
    assert!(TlsConfig::from_pem(id.cert_pem.as_bytes(), b"").is_err()); // no key
    assert!(TlsConfig::from_pem(b"not pem at all", b"also not pem").is_err());
    assert!(TlsConfig::from_pem(id.key_pem.as_bytes(), id.key_pem.as_bytes()).is_err()); // key where a cert is expected
}
