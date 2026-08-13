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
| RED | `cargo test --test acceptance` | The new conformance tests failed for dynamic `HEAD` fallback and name-based UUID inference, proving both regressions before the implementation fix. |
| GREEN | `cargo test --workspace --all-targets` | 10 acceptance tests passed; router microbench executable also completed. |
| GREEN | `cargo test --doc --workspace` | Doc-test target completed with no failures. |
| GREEN | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Completed with zero warnings. |
| Docker smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -Vus 1 -Version matrix-smoke5` | All 12 P0 cases ran against raw/framework (24 measurements) with zero measured errors; the report remains `INCONCLUSIVE` because it is below the release minimum. |
| Collector smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases plaintext -Version smoke-stats` | Expanded p50/p95/p99 report and CPU/RSS artifact paths were generated; the run is correctly `INCONCLUSIVE` because it was too short to produce stable samples. |
| Extended plaintext | `./benchmarks/run-benchmark.ps1 -Iterations 1000000 -Runs 7 -WarmupSeconds 30 -Vus 32 -Cases plaintext -Version plaintext-official2` | 14 measurements and zero measured errors were collected; raw CV was 8.39%, throughput overhead was 4.55%, CPU delta was 4.04%, and negative timing samples were observed, so the gate is `INCONCLUSIVE`/invalid rather than PASS. |

## Guarantees covered by tests

| # | Guarantee | Test | Result |
|---|---|---|---|
| 1 | Static, dynamic path and typed query dispatch work | `tests/acceptance.rs::app_registers_static_dynamic_and_query_routes` | PASS |
| 2 | HEAD, 405/Allow, OPTIONS, 404 and OpenAPI/Swagger endpoints work | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 3 | JSON body and typed header extractors reject/parse at the boundary | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 4 | Registered summary/tag metadata appears in an OpenAPI 3.1 document | `tests/acceptance.rs::openapi_describes_registered_operations` | PASS |
| 5 | Router precedence, percent-decoding, duplicate detection, malformed templates, and 10k static routes are covered | `tests/acceptance.rs` | PASS |
| 6 | OpenAPI path schemas follow extractor types, JSON request bodies and headers are represented, and optional headers are not required | `tests/acceptance.rs::openapi_uses_extractor_types_and_response_statuses` | PASS |
| 7 | OpenAPI DTO/query schema derivation has pass and compile-fail coverage | `cargo test --test compile` | PASS |
| 8 | Release profile and raw/framework Docker images build from the same locked dependencies | `cargo build --release --locked`, `benchmarks/Dockerfile` | PASS |

## Known gaps

The current implementation is a v0.1 foundation, not evidence that the full 1% release contract has passed. PostgreSQL connectivity is implemented and smoke-tested with 16 persistent prepared connections, but the full 7-run/1M-request matrix across all VU levels on a dedicated host was not run. The static plaintext in-process microbench now reports zero extra allocations versus its raw comparator; dynamic capture/query allocation profiling, fuzzing, streaming/cancellation coverage, and dedicated-host CI gating remain follow-ups. The benchmark script intentionally reports `INCONCLUSIVE` until all requested gates and confidence intervals are available.
