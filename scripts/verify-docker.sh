#!/usr/bin/env bash
# Run the local gates inside Docker (pinned toolchain). There is no hosted CI.
# Usage: scripts/verify-docker.sh   (from the repo root, before committing)
set -euo pipefail

RUST_VERSION="${RUST_VERSION:-1.88}"

MSYS_NO_PATHCONV=1 docker run --rm \
  -v "$PWD:/src" \
  -v oasrs-target:/cache/target \
  -v oasrs-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/cache/target \
  -w /src "rust:${RUST_VERSION}" bash -euxc '
    rustup component add rustfmt clippy
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo clippy --workspace --all-targets --features tls -- -D warnings
    cargo test --features tls --test tls
    cargo test --workspace --all-targets --features "uuid test-util swagger multipart tls http2 compression compression-brotli static-files websocket"
    cargo test --doc --workspace
    cargo build --workspace --examples --features "uuid swagger tls http2"
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
  '
echo "verify-docker: all gates passed"
echo "(the Docker cache volumes oasrs-target and oasrs-cargo can reach many GB; run scripts/clean.sh when you are done)"
