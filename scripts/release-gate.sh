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
# The dependency audit is part of the gate: a missing tool fails it.
cargo deny --version >/dev/null 2>&1 || { echo "release-gate: cargo-deny is required (cargo install cargo-deny --locked)" >&2; exit 1; }
cargo deny check
# The send_timeout socket-fill tests are ignored on Windows (its loopback never
# blocks a write), so they must run on Linux: natively, or in Docker elsewhere.
if [ "$(uname -s)" = "Linux" ]; then
  cargo test --features test-util --test send_timeout
else
  MSYS_NO_PATHCONV=1 docker run --rm -v "$PWD:/src" -v oasrs-target:/cache/target -v oasrs-cargo:/usr/local/cargo/registry     -e CARGO_TARGET_DIR=/cache/target -w /src rust:1.88 cargo test --features test-util --test send_timeout
fi
# Not `! cargo ... | grep`: under `set -e` a `!` pipeline never stops the script,
# and it would also hide a failing `cargo package`.
package_files="$(cargo package --list -p oas-rs)"
if grep -Ei 'wip|\.py$|\.log$' <<<"$package_files"; then
  echo "release-gate: unexpected files in the package" >&2
  exit 1
fi
[ "$(grep -m1 '^version' Cargo.toml)" = "$(grep -m1 '^version' oas-rs-macros/Cargo.toml)" ]
echo "release-gate: PASSED on $(git rev-parse HEAD)"
