# oas-rs v0.1 TDD evidence

Source plan: `C:\Users\Q\Downloads\2026-08-13-oas-rust-framework-acceptance-benchmark-plan.md`.

## Journeys

- As an API developer, I can register static, dynamic, query, JSON-body and typed-header handlers.
- As an API client, I receive HTTP method semantics, typed success responses and problem details for invalid inputs.
- As an API developer, I can expose generated OpenAPI JSON and a Swagger smoke page without re-registering route metadata.
- As a release engineer, I can compare a raw Hyper server with the framework through a reproducible Docker/k6 harness.

## RED/GREEN evidence

| Stage | Command | Evidence |
|---|---|---|
| RED | `cargo test --test acceptance` | Initial run compiled dependencies, then failed because `src/lib.rs` was absent. |
| GREEN | `cargo test --workspace --all-targets` | 3 acceptance tests passed; router microbench executable also completed. |
| GREEN | `cargo test --doc --workspace` | Doc-test target completed with no failures. |
| GREEN | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Completed with zero warnings. |
| Docker smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -Vus 1 -Version smoke3` | Both raw and oas containers ran, measured errors were zero; report remains `INCONCLUSIVE` by design. |

## Guarantees covered by tests

| # | Guarantee | Test | Result |
|---|---|---|---|
| 1 | Static, dynamic path and typed query dispatch work | `tests/acceptance.rs::app_registers_static_dynamic_and_query_routes` | PASS |
| 2 | HEAD, 405/Allow, OPTIONS, 404 and OpenAPI/Swagger endpoints work | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 3 | JSON body and typed header extractors reject/parse at the boundary | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 4 | Registered summary/tag metadata appears in an OpenAPI 3.1 document | `tests/acceptance.rs::openapi_describes_registered_operations` | PASS |
| 5 | Release profile and raw/framework Docker images build from the same locked dependencies | `cargo build --release --locked`, `benchmarks/Dockerfile` | PASS |

## Known gaps

The current implementation is a v0.1 foundation, not evidence that the full 1% release contract has passed. PostgreSQL, allocation counters, CPU/RSS capture, 7-run paired aggregation, 10k-route stress, macro compile-fail tests, and CI dedicated-host gating remain follow-ups. The benchmark script intentionally reports `INCONCLUSIVE` until those gates and confidence intervals are available.
