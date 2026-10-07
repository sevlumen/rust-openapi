# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `ApiSchema` implementations for `f32`, `f64`, small integers, `Box`,
  string-keyed maps and `serde_json::Value`.
- `LICENSE-MIT`, `CONTRIBUTING.md`, and `scripts/verify-docker.sh`.
- Crate metadata (`repository`, `readme`, `keywords`, `categories`, `rust-version`).

### Changed
- Swagger UI assets carry Subresource Integrity hashes.
- CI runs `cargo-deny` (advisories, licenses, sources) via `deny.toml`.
- CI and `scripts/verify-docker.sh` also build the docs with warnings denied.
- Benchmark design notes moved to `docs/benchmark-design.md`; internal agent
  plan files removed.
- Swagger UI assets are pinned to `swagger-ui-dist@5.17.14`.
- Inline-future safety invariants are enforced with `assert!` and documented
  with `SAFETY` comments.
- Query values are parsed once instead of twice.
- Percent-decoding copies literal runs in bulk instead of byte by byte.

## [0.1.0]

- Initial release: typed routing on Hyper and Tokio with OpenAPI 3.1 generation.
