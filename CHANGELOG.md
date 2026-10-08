# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.1.0] - 2026-10-08

First public release.

### Features
- Typed routing on Hyper and Tokio with startup-generated OpenAPI 3.1 metadata
  and an opt-in Swagger UI (pinned `swagger-ui-dist@5.17.14` with Subresource
  Integrity hashes).
- Extractors (`Path`, `Query`, `Header`, `Json`, `State`, `Params`) and response
  types with RFC 9457-style `ApiError`.
- `#[derive(ApiSchema)]` for structs and unit-variant enums, with
  `#[serde(rename)]` / `#[serde(rename_all)]` support. Built-in schemas cover
  `String`, `bool`, integers, `f32`/`f64`, `Uuid`, `Option`, `Vec`, `Box`,
  string-keyed maps and `serde_json::Value`.

### Quality
- Inline-future safety invariants are enforced with `assert!`, documented with
  `SAFETY` comments, and checked by Miri in CI.
- CI runs fmt, clippy (default and all features), tests, doctests, examples,
  rustdoc with warnings denied, and `cargo-deny`.
- `scripts/verify-docker.sh` runs the same gates locally in Docker.
- MIT license, `CONTRIBUTING.md`, crate metadata and a minimum supported Rust
  version of 1.88.
