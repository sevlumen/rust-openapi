# Contributing to oas-rs

Thanks for helping. Bug reports, docs fixes and focused pull requests are welcome.

## Before you open a pull request

Run the same gates as CI. Docker is the easiest way to match the pinned
toolchain (Rust 1.88):

```bash
bash scripts/verify-docker.sh
```

Without Docker, run the commands listed under "Verification" in the README.
The `.cargo/config.toml` in this repository declares the `quangt` registry, so
a plain `cargo test` works after cloning.

## Guidelines

- Keep changes focused; add a test for every behavior change.
- Public API and HTTP semantics are frozen for the `0.1` line. Open an issue
  before proposing a breaking change.
- Every `unsafe` block needs a `// SAFETY:` comment, and changes to
  `src/handler.rs` must keep the Miri job green.
- Performance-sensitive changes should include `cargo bench --bench router`
  numbers before and after.
- Update `CHANGELOG.md` for user-visible changes.

By contributing you agree that your work is licensed under the MIT License.
