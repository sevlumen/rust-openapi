# Contributing to oas-rs

Thanks for helping. Bug reports, docs fixes and focused pull requests are welcome.

## Before you open a pull request

There is no hosted CI, so the local gates are the check. Docker is the
easiest way to match the pinned toolchain (Rust 1.88):

```bash
bash scripts/verify-docker.sh
```

Without Docker, run the commands listed under "Verification" in the README.
A plain `cargo test --features "uuid test-util swagger"` also works after
cloning.

## Cleaning up

`scripts/verify-docker.sh` keeps its build cache in the Docker volumes
`oasrs-target` and `oasrs-cargo`, and cargo fills `target/`. Together they grow
to many GB across feature combinations. When you finish a task or branch, run:

```bash
bash scripts/clean.sh
```

It removes `target/` and those two volumes (everything is rebuilt on demand).

## Guidelines

- Keep changes focused; add a test for every behavior change.
- While the crate is `0.x`, a breaking change to the public API or to HTTP
  semantics needs a minor version bump and a `### Changed` entry in the
  changelog; open an issue first. Run `cargo semver-checks` against the
  latest published release when you can.
- Every `unsafe` block needs a `// SAFETY:` comment, and changes to
  `src/handler.rs` must keep `cargo +nightly miri test --lib inline_future` green.
- Performance-sensitive changes should include `cargo bench --bench router`
  numbers before and after.
- Update `CHANGELOG.md` for user-visible changes. The support policy (versions,
  MSRV, what counts as public API) is in the README.

By contributing you agree that your work is licensed under the MIT License.
