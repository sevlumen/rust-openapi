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
| GREEN | `cargo test --workspace --all-targets` | 15 acceptance tests passed; router microbench executable also completed. |
| GREEN | `cargo test --workspace --all-targets` | Startup OpenAPI cache, shared benchmark DTO serialization, examples, and deterministic fuzz smoke all passed alongside the existing suite. |
| GREEN | `cargo test --doc --workspace` | Doc-test target completed with no failures. |
| GREEN | `cargo build --workspace --examples` | `hello` and `users-api` reference examples compiled. |
| GREEN | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Completed with zero warnings. |
| Allocation hot paths | `OAS_BENCH_ITERATIONS=1000000 cargo bench --bench router -- --nocapture` | Plaintext, integer/UUID path, typed query, and typed header all passed executable zero-extra-allocation assertions; each measured 0 extra allocations and 0 extra bytes/op versus a request-shape-matched raw comparator. |
| OpenAPI hot path | `OAS_BENCH_ITERATIONS=1000000 cargo bench --bench router -- --nocapture` | OpenAPI/Swagger enabled and disabled apps both measured 4 allocations/op; the latest single release sample showed -0.49% enabled-vs-disabled time delta, which is supporting evidence rather than a statistical gate. |
| Static route-count sweep | `cargo bench --bench router -- --nocapture` | In-process static lookup sweep covered 1, 10, 100, 1,000, and 10,000 routes with stable 4 allocations/op on the framework path; this is not an end-to-end release gate. |
| Docker smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -Vus 1 -Version matrix-smoke5` | All 12 P0 cases ran against raw/framework (24 measurements) with zero measured errors; the report remains `INCONCLUSIVE` because it is below the release minimum. |
| Collector smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases plaintext -Version smoke-stats` | Expanded p50/p95/p99 report and CPU/RSS artifact paths were generated; the run is correctly `INCONCLUSIVE` because it was too short to produce stable samples. |
| Extended plaintext | `./benchmarks/run-benchmark.ps1 -Iterations 1000000 -Runs 7 -WarmupSeconds 30 -Vus 32 -Cases plaintext -Version plaintext-official2` | 14 measurements and zero measured errors were collected; raw CV was 8.39%, throughput overhead was 4.55%, CPU delta was 4.04%, and negative timing samples were observed, so the gate is `INCONCLUSIVE`/invalid rather than PASS. |
| Body-equivalence smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -WarmupSeconds 1 -Vus 1 -Version smoke-body-equivalence` | All 12 cases × raw/framework completed (24 measurements), including PostgreSQL, with status and expected-body checks passing and zero custom measured errors. |
| HTTP transport | `cargo test --test acceptance` | Keep-alive, connection-close, interrupted request body, and streaming response tests pass. |
| P1 smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases validation-success,problem,raw-handler,security -Version smoke-p1` | 8 raw/framework measurements completed with status/body checks passing and zero measured errors; result remains `INCONCLUSIVE` by design. |
| CPU/topology smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10000 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases plaintext -Version smoke-topology-cpu` | Docker affinity/memory assertions passed; cgroup CPU usage and RSS samples were captured, with exact request counts and an `INCONCLUSIVE` report because it was below release minimums. |

## Guarantees covered by tests

| # | Guarantee | Test | Result |
|---|---|---|---|
| 1 | Static, dynamic path and typed query dispatch work | `tests/acceptance.rs::app_registers_static_dynamic_and_query_routes` | PASS |
| 2 | HEAD, 405/Allow, OPTIONS, 404 and OpenAPI/Swagger endpoints work | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 3 | JSON body and typed header extractors reject/parse at the boundary | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 4 | Docker/k6 checks compare expected response bodies as well as status codes for all benchmark cases | `benchmarks/k6.js`, `smoke-body-equivalence` | PASS |
| 5 | Keep-alive, connection-close, body cancellation, and lazy stream responses conform | `tests/acceptance.rs` | PASS |
| 6 | Typed path, query, and header fast paths add no measured allocations or bytes versus request-shape-matched raw comparators | `benches/router.rs` | PASS |
| 7 | Duplicate operation IDs are rejected and emitted in OpenAPI | `tests/acceptance.rs` | PASS |
| 8 | Registered summary/tag metadata appears in an OpenAPI 3.1 document | `tests/acceptance.rs::openapi_describes_registered_operations` | PASS |
| 9 | Router precedence, percent-decoding, duplicate detection, malformed templates, and 10k static routes are covered | `tests/acceptance.rs`, `tests/fuzz_smoke.rs` | PASS |
| 10 | OpenAPI path schemas follow extractor types, JSON request bodies and headers are represented, and optional headers are not required | `tests/acceptance.rs::openapi_uses_extractor_types_and_response_statuses` | PASS |
| 11 | OpenAPI DTO/query schema derivation has pass and compile-fail coverage | `cargo test --test compile` | PASS |
| 12 | OpenAPI documents are prepared before listener dispatch rather than regenerated on normal requests | `src/lib.rs::openapi_document_is_prepared_once_for_server_dispatch` | PASS |
| 13 | Release profile and raw/framework Docker images build from the same locked dependencies | `cargo build --release --locked`, `benchmarks/Dockerfile` | PASS |

## Known gaps

The current implementation is a v0.1 foundation, not evidence that the full 1% release contract has passed. PostgreSQL connectivity is implemented and smoke-tested with 16 persistent prepared connections, typed path/query/header allocation gates pass in-process, and the Docker harness now validates affinity, memory limits, exact request counts, cgroup CPU usage, and RSS artifacts. The full 7-run/1M-request matrix across all VU levels on a dedicated host was not run; the earlier extended plaintext evidence was invalidated by raw CV 8.39% and a negative timing sample. Microbench timing samples are supporting evidence, not statistical release gates. The benchmark script intentionally reports `INCONCLUSIVE` until all requested gates and confidence intervals are available.
