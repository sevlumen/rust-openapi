# Linux Benchmark Design for oas-rs

## Goal

Provide two complementary performance signals for `oas-rs`:

1. A fast, deterministic Rust microbenchmark that remains in the core repository and detects regressions in router, extractor, response, allocation, and route-scaling hot paths.
2. A release-oriented HTTP acceptance benchmark that runs as an independent Linux project and compares a raw Hyper server with an `oas-rs` server over real TCP/HTTP traffic.

The two layers have different authority: the microbenchmark is the developer regression detector; the dedicated-Linux HTTP benchmark is the only source allowed to produce an `ACCEPT`/`NOT ACCEPTED` performance decision.

## Scope and repository boundary

The `oas-rs` repository keeps:

- `benches/router.rs` and its Cargo benchmark declaration;
- benchmark documentation describing the microbenchmark contract;
- no Docker Compose load-test laboratory, `oha` runner, PostgreSQL service, or committed HTTP result archives.

The HTTP laboratory is a standalone sibling project, provisionally named `oas-rs-perf` and located beside this repository (for example, `../oas-rs-perf`) or in a separate remote repository. It must not be added as a workspace member or tracked subtree of `oas-rs`.

## Core microbenchmark

The existing release-profile benchmark remains the primary in-repository harness. It must cover these observable cases:

- static route latency;
- dynamic path extraction, including integer and UUID paths;
- materialized `Params` extraction;
- typed query extraction;
- typed header extraction;
- combined path/query/header extraction;
- buffered JSON request extraction;
- static text, static JSON, and `JsonBytes` response construction;
- 404, 405, and OPTIONS behavior;
- fixed-route scaling at 1, 10, 100, 1,000, and 10,000 routes;
- dynamic-route scaling for first, middle, last, miss, method-not-allowed, and OPTIONS positions;
- per-operation nanoseconds, allocation count, and allocated bytes where the harness can measure them.

The benchmark must continue to build with the repository's existing feature gate (`uuid`, `test-util`, and `swagger`) and run with the release profile through `cargo bench --bench router`. Benchmark output is diagnostic and comparative; it is not treated as a statistically stable release gate by itself.

## Independent HTTP acceptance laboratory

The standalone lab contains two server implementations exposing identical endpoint contracts:

- `raw-hyper`: the minimal raw Hyper/Tokio reference;
- `oas-rs`: the framework implementation using the checked-out or pinned `oas-rs` revision.

The lab uses Docker for reproducibility on a dedicated Linux host. Its components are:

- `Dockerfile` for the server image/build environment;
- `docker-compose.yml` for server topology and resource configuration;
- an `oha` load-generator container or pinned host binary;
- runner/report scripts that execute paired raw-vs-framework runs;
- machine-readable run artifacts kept outside the core `oas-rs` repository.

The HTTP workload must measure at least:

- requests per second;
- error rate and expected-status success rate;
- p50, p95, and p99 latency;
- CPU usage/request;
- resident memory (RSS);
- request-count, concurrency, keep-alive, and warm-up settings.

Allocation/request is optional for the HTTP layer because allocator instrumentation can perturb the server. If collected, it is reported as a diagnostic metric and cannot replace the HTTP acceptance measurements.

Each acceptance run records the commit/revision, Linux kernel and distribution, CPU model and logical processor count, Docker/runtime versions, `oha` version, workload parameters, and raw result-file names. The runner must reject official mode unless it is executing on Linux with the intended dedicated-host contract; Windows/Docker Desktop runs may be diagnostic only.

## Comparison and acceptance policy

Raw Hyper and `oas-rs` runs are paired by case and load level. The report calculates framework overhead for throughput and latency using the raw result as the baseline and preserves the underlying JSON summaries.

The acceptance policy is explicit and configurable in the independent lab. A release report must contain one of:

- `ACCEPT`: every required case has complete paired data, expected-status success is 100%, and all configured overhead thresholds pass;
- `NOT ACCEPTED`: a required case fails status/error/threshold checks;
- `INCONCLUSIVE`: the host/runtime is not eligible or the matrix is incomplete.

No result is considered official without a dedicated Linux execution and a recorded environment manifest.

## Verification

Core-repository verification:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo test --doc --workspace
cargo build --workspace --examples
cargo bench --bench router --features uuid,test-util,swagger
```

Acceptance-lab verification:

```text
docker compose config
docker compose build
docker compose up -d raw-hyper oas-rs
./scripts/run-matrix.sh --official --output results/20260815-official
./scripts/validate-report.sh results/20260815-official/REPORT.json
```

The final handoff must report the exact commands, commit/revision, environment, benchmark outputs, and whether the HTTP result was `ACCEPT`, `NOT ACCEPTED`, or `INCONCLUSIVE`.

## Out of scope

- PostgreSQL or application-specific persistence in the benchmark path;
- adding an HTTP load generator to the `oas-rs` Cargo workspace;
- claiming Linux acceptance from a Windows host or Docker Desktop;
- replacing the microbenchmark with only an end-to-end load test;
- committing large raw result archives to the core framework repository.
