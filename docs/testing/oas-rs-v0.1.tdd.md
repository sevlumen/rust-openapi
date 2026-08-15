# oas-rs v0.1 TDD evidence

Source plan: `C:\Users\Q\Downloads\2026-08-13-oas-rust-framework-acceptance-benchmark-plan.md`.

## Journeys

- As an API developer, I can register static, dynamic, query, JSON-body and typed-header handlers.
- As an API client, I receive HTTP method semantics, typed success responses and problem details for invalid inputs.
- As an API developer, I can expose generated OpenAPI JSON and a Swagger smoke page without re-registering route metadata.
- As a release engineer, I can compare a raw Hyper server with the framework through a reproducible Docker/oha harness.

## RED/GREEN evidence

| Stage | Command | Evidence |
|---|---|---|
| RED | `cargo test --test acceptance` | The new conformance tests failed for dynamic `HEAD` fallback and name-based UUID inference, proving both regressions before the implementation fix. |
| GREEN | `cargo test --workspace --all-targets` | 26 acceptance tests passed; router microbench executable also completed. |
| GREEN | `cargo test --workspace --all-targets` | Startup OpenAPI cache, shared benchmark DTO serialization, examples, and deterministic fuzz smoke all passed alongside the existing suite. |
| GREEN | `cargo test --doc --workspace` | Doc-test target completed with no failures. |
| GREEN | `cargo build --workspace --examples` | `hello` and `users-api` reference examples compiled. |
| GREEN | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Completed with zero warnings. |
| Miri unsafe-future check | `cargo +nightly miri test --lib inline_future --` | Three tests passed on the current Windows worktree after using a test-only executor without Tokio IOCP; this covers pinned `!Unpin` polling and oversized heap fallback. |
| Allocation hot paths | `OAS_BENCH_ITERATIONS=1000000 cargo bench --bench router -- --nocapture` | Plaintext, integer/UUID path, typed query, and typed header all passed executable zero-extra-allocation assertions; each measured 0 extra allocations and 0 extra bytes/op versus a request-shape-matched raw comparator. |
| Generated multi-extractor path | `OAS_BENCH_CASE=multi-extractor OAS_BENCH_ITERATIONS=1000000 cargo bench --bench router -- --nocapture` | Five native release samples exercised the compile-time `Path + Query + Header` binder; median was 827.03 ns/op versus 728.65 ns/op for the request-shape-matched raw comparator, with 0 extra allocations and 0 extra bytes/op. This is diagnostic Windows evidence, not a release gate. |
| OpenAPI hot path | `OAS_BENCH_ITERATIONS=1000000 cargo bench --bench router -- --nocapture` | OpenAPI/Swagger enabled and disabled apps both measured 4 allocations/op; the latest single release sample showed -0.49% enabled-vs-disabled time delta, which is supporting evidence rather than a statistical gate. |
| Static route-count sweep | `cargo bench --bench router -- --nocapture` | In-process static lookup sweep covered 1, 10, 100, 1,000, and 10,000 routes with stable 4 allocations/op on the framework path; this is not an end-to-end release gate. |
| Dynamic route trie | `cargo test --lib dynamic_routes_use_a_compiled_method_trie` and `cargo bench --bench router -- --nocapture` | Dynamic route lookup uses a compiled method trie; the 1/10/100/1,000/10,000 route sweep remains bounded by path depth for hits, while misses are retained as scaling evidence. |
| Raw Incoming escape hatch | `cargo test --test acceptance raw_` | Raw handlers receive `Request<Incoming>` directly; a TCP request with a 1 MiB declared body returns without framework-side collection. |
| Docker smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -Vus 1 -Version matrix-smoke5` | All 12 P0 cases ran against raw/framework (24 measurements) with zero measured errors; the report remains `INCONCLUSIVE` because it is below the release minimum. |
| Official platform guard | `pwsh -File ./benchmarks/tests/test-matrix-guard.ps1` | The matrix guard rejects `-Official` on Windows/Docker Desktop and permits only diagnostic runs there; Linux remains the release environment. |
| Collector smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases plaintext -Version smoke-stats` | Expanded p50/p95/p99 report and CPU/RSS artifact paths were generated; the run is correctly `INCONCLUSIVE` because it was too short to produce stable samples. Official runs now stop at the first negative timing sample and retain a partial `INCONCLUSIVE` report. |
| Extended plaintext | `./benchmarks/run-benchmark.ps1 -Iterations 1000000 -Runs 7 -WarmupSeconds 30 -Vus 32 -Cases plaintext -Version plaintext-official2` | 14 measurements and zero measured errors were collected; raw CV was 8.39%, throughput overhead was 4.55%, CPU delta was 4.04%, and negative timing samples were observed, so the gate is `INCONCLUSIVE`/invalid rather than PASS. |
| Native direct focused current | `./benchmarks/run-native-benchmark.ps1 -Cases users,users-static -Runs 3 -Requests 1000000 -Connections 256 -WarmupRequests 10000 -Version native-current` | Direct Windows host run on the current worktree completed all 12 raw/OAS pairs with 100% expected status: `/users` OAS median 68,358.58 RPS vs raw 67,577.66 (`-1.142%`), `/users-static` OAS 79,858.29 vs raw 77,209.37 (`-3.317%`). Diagnostic only; no cgroup CPU/RSS authority. |
| Benchmark status smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases plaintext,problem,404,405,header,security -Version oha-status-smoke` | All 6 cases × raw/framework completed (12 measurements), with per-request expected-status checks passing and zero measured errors; response-body correctness remains covered by the Rust acceptance suite. |
| Final Docker smoke | `./benchmarks/run-benchmark.ps1 -Iterations 1000 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases plaintext,path-integer,raw-handler,404 -Version oha-final-smoke -AllowUndersizedHost` | 8 raw/framework measurements completed, all 8,000 requests returned their expected status, no negative timings were observed, and the result remains `INCONCLUSIVE` because it is below release minimums. |
| HTTP transport | `cargo test --test acceptance` | Keep-alive, connection-close, interrupted request body, and streaming response tests pass. |
| P1 smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases validation-success,problem,raw-handler,security -Version smoke-p1` | 8 raw/framework measurements completed with per-request status checks and zero measured errors; response-body correctness remains covered by the Rust acceptance suite and the result remains `INCONCLUSIVE` by design. |
| CPU/topology smoke | `./benchmarks/run-benchmark.ps1 -Iterations 10000 -Runs 1 -WarmupSeconds 1 -Vus 1 -Cases plaintext -Version smoke-topology-cpu` | Docker affinity/memory assertions passed; cgroup CPU usage and RSS samples were captured, with exact request counts and an `INCONCLUSIVE` report because it was below release minimums. |

## Guarantees covered by tests

| # | Guarantee | Test | Result |
|---|---|---|---|
| 1 | Static, dynamic path and typed query dispatch work | `tests/acceptance.rs::app_registers_static_dynamic_and_query_routes` | PASS |
| 2 | HEAD, 405/Allow, OPTIONS, 404 and OpenAPI/Swagger endpoints work | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 3 | JSON body and typed header extractors reject/parse at the boundary | `tests/acceptance.rs::http_semantics_and_typed_body_header_are_preserved` | PASS |
| 4 | Docker/oha checks compare expected status codes for all benchmark cases; response-body correctness remains covered by the Rust acceptance suite | `benchmarks/oha-adapter.ps1`, `oha-status-smoke` | PASS |
| 5 | Keep-alive, connection-close, body cancellation, and lazy stream responses conform | `tests/acceptance.rs` | PASS |
| 6 | Typed path, query, and header fast paths add no measured allocations or bytes versus request-shape-matched raw comparators | `benches/router.rs` | PASS |
| 7 | Duplicate operation IDs are rejected and emitted in OpenAPI | `tests/acceptance.rs` | PASS |
| 8 | Registered summary/tag metadata appears in an OpenAPI 3.1 document | `tests/acceptance.rs::openapi_describes_registered_operations` | PASS |
| 9 | Router precedence, percent-decoding, duplicate detection, malformed templates, and 10k static routes are covered | `tests/acceptance.rs`, `tests/fuzz_smoke.rs` | PASS |
| 10 | OpenAPI path schemas follow extractor types, JSON request bodies and headers are represented, and optional headers are not required | `tests/acceptance.rs::openapi_uses_extractor_types_and_response_statuses` | PASS |
| 11 | OpenAPI DTO/query schema derivation has pass and compile-fail coverage | `cargo test --test compile` | PASS |
| 12 | OpenAPI JSON and Swagger HTML are prepared before listener dispatch rather than regenerated on normal requests | `src/lib.rs::openapi_document_is_prepared_once_for_server_dispatch` | PASS |
| 13 | Release profile and raw/framework Docker images build from the same locked dependencies | `cargo build --release --locked`, `benchmarks/Dockerfile` | PASS |
| 14 | Dynamic routes are compiled into per-method trie nodes instead of a request-time route scan | `src/lib.rs::dynamic_routes_use_a_compiled_method_trie`, `benches/router.rs` | PASS |
| 15 | Raw handlers can receive Hyper `Incoming` without global request-body collection | `tests/acceptance.rs::raw_route_receives_incoming_without_collecting_request_body` | PASS |
| 16 | Custom optional extractors map missing input to `None`, invalid present input to `400`, and valid present input to `Some` | `tests/acceptance.rs::custom_optional_extractors_distinguish_missing_from_invalid` | PASS |
| 17 | Inline futures preserve pinned application futures and use a safe boxed fallback when oversized | `cargo +nightly miri test --lib inline_future --` | PASS |

## Known gaps

The current implementation is a v0.1 foundation, not evidence that the full 1% release contract has passed. PostgreSQL connectivity is implemented and smoke-tested with 16 persistent prepared connections, typed path/query/header allocation gates pass in-process, and the Docker harness now validates affinity, memory limits, exact request counts, cgroup CPU usage, and RSS artifacts. The full 7-run/1M-request matrix across all VU levels on a dedicated host was not run; the earlier extended plaintext evidence was invalidated by raw CV 8.39% and a negative timing sample. Microbench timing samples are supporting evidence, not statistical release gates. The benchmark script intentionally reports `INCONCLUSIVE` until all requested gates and confidence intervals are available.
