#!/usr/bin/env bash
# The release gate, run locally (there is no hosted CI). It fails on the first
# problem and ends by printing the commit it validated: publish only that SHA.
# Needs a clean tree so the result describes the commit, not local edits.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -z "$(git status --porcelain)" ] || { echo "release-gate: the working tree is not clean" >&2; exit 1; }
FEATURES="uuid test-util swagger multipart tls http2 compression compression-brotli static-files websocket"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo +1.88 check --workspace --all-targets --all-features
cargo test --workspace --all-targets
cargo test --workspace --all-targets --features "$FEATURES"
cargo test --doc --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo +nightly miri test --lib inline_future
OAS_SOAK_SECS="${OAS_SOAK_SECS:-60}" cargo test --release --features "test-util http2 websocket" --test soak -- --ignored --nocapture
if cargo deny --version >/dev/null 2>&1; then cargo deny check; else echo "release-gate: cargo-deny not installed, skipped"; fi
! cargo package --list -p oas-rs | grep -Ei 'wip|\.py$|\.log$'
[ "$(grep -m1 '^version' Cargo.toml)" = "$(grep -m1 '^version' oas-rs-macros/Cargo.toml)" ]
echo "release-gate: PASSED on $(git rev-parse HEAD)"
