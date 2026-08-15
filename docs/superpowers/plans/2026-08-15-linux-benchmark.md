# Linux Benchmark Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep a release-profile Rust microbenchmark in `oas-rs` and build a standalone Docker-based Linux HTTP acceptance lab comparing raw Hyper with `oas-rs`.

**Architecture:** The core repository changes are limited to benchmark coverage/documentation. The acceptance lab is a separate sibling project at `/home/quang/code/oas-rs-perf`; it contains one small server binary selected by `BENCH_IMPL=raw|oas`, a Compose file, pinned `oha` runner, environment capture, and a report validator. The lab consumes the core crate through a local path for this run and records the exact `oas-rs` revision.

**Tech Stack:** Rust 2024, Hyper 1.x, Tokio, `oas-rs`, Docker Compose, `oha`, POSIX shell, JSON report artifacts.

## Global Constraints

- The core `oas-rs` repository must not gain a Docker Compose load-test laboratory, PostgreSQL service, `oha` runner, or committed HTTP result archives.
- The core microbenchmark runs with `cargo bench --bench router --features uuid,test-util,swagger` and uses the existing release profile.
- Official HTTP acceptance runs require Linux on a dedicated host; Windows and Docker Desktop are diagnostic only.
- Raw Hyper and `oas-rs` expose the same `GET /users/123456` endpoint and return status 200 with a small JSON body.
- Acceptance reports must include RPS, error rate, p50/p95/p99 latency, CPU/request, RSS, request count, concurrency, keep-alive, warm-up, revision, and environment metadata.
- No production framework code changes are required unless a failing acceptance smoke test identifies an actual framework defect.

---

### Task 1: Align and document the in-repository microbenchmark

**Files:**
- Modify: `README.md`
- Inspect only: `benches/router.rs`, `Cargo.toml`

**Interfaces:**
- Produces: one documented command for the full release-profile microbenchmark and a list of the existing measured cases.

- [ ] **Step 1: Verify the current benchmark command and feature gate**

Run:

```bash
cargo bench --bench router --features uuid,test-util,swagger --no-run
```

Expected: the benchmark target compiles successfully; if compilation fails, stop and investigate the reported cause before changing documentation.

- [ ] **Step 2: Update the README benchmark section**

Document the exact feature-enabled command and the current categories: static/dynamic routing, typed extractors, response construction, allocation counters, and fixed/dynamic route scaling including 404/405/OPTIONS.

- [ ] **Step 3: Verify the documentation edit**

Run:

```bash
git diff --check
rg -n "cargo bench --bench router|route scaling|allocation|OPTIONS" README.md
```

Expected: no whitespace errors and the README contains the feature-enabled command and all documented categories.

- [ ] **Step 4: Record the core change**

```bash
git add README.md
git commit -m "docs: document Linux microbenchmark contract"
```

If the environment rejects Git index writes, retain the change and report that commit creation was blocked by filesystem permissions.

### Task 2: Create a failing acceptance-report validation test

**Files:**
- Create: `/home/quang/code/oas-rs-perf/scripts/tests/test_report_validator.sh`
- Create: `/home/quang/code/oas-rs-perf/scripts/validate-report.sh`

**Interfaces:**
- Consumes: report files with `status`, `implementation`, `case`, `rps`, `error_rate`, `p50_ms`, `p95_ms`, `p99_ms`, `cpu_per_request`, and `rss_bytes` fields.
- Produces: exit code 0 only for a complete `ACCEPT` report and nonzero for missing, invalid, or threshold-failing data.

- [ ] **Step 1: Write the failing shell test**

The test must create one incomplete report and assert that the not-yet-created validator exits nonzero:

```sh
#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf '{"status":"ACCEPT","runs":[]}' >"$tmp/REPORT.json"
if "$root/scripts/validate-report.sh" "$tmp/REPORT.json"; then
  echo "validator accepted an incomplete report" >&2
  exit 1
fi
```

- [ ] **Step 2: Run the test and verify the expected failure**

Run:

```bash
sh /home/quang/code/oas-rs-perf/scripts/tests/test_report_validator.sh
```

Expected: FAIL because `validate-report.sh` does not yet exist or rejects the incomplete report.

- [ ] **Step 3: Implement the minimal validator contract**

The validator must parse JSON with the system's available JSON tool (`jq` in the lab image), require `status=ACCEPT`, require paired `raw-hyper` and `oas-rs` rows for every required case, reject nonzero errors, and enforce configured throughput/latency overhead thresholds.

- [ ] **Step 4: Run the test and verify it passes**

Run the same shell test. Expected: PASS with the incomplete report rejected.

- [ ] **Step 5: Add valid and threshold-failing fixtures**

Extend the test with one complete passing report and one report whose `oas-rs` p95 exceeds the configured threshold. Assert pass and fail respectively.

- [ ] **Step 6: Record the validator change**

```bash
cd /home/quang/code/oas-rs-perf
git add scripts/validate-report.sh scripts/tests/test_report_validator.sh
git commit -m "test: validate HTTP acceptance reports"
```

### Task 3: Implement parity servers and Docker Compose topology

**Files:**
- Create: `/home/quang/code/oas-rs-perf/Cargo.toml`
- Create: `/home/quang/code/oas-rs-perf/src/main.rs`
- Create: `/home/quang/code/oas-rs-perf/Dockerfile`
- Create: `/home/quang/code/oas-rs-perf/docker-compose.yml`
- Create: `/home/quang/code/oas-rs-perf/.dockerignore`

**Interfaces:**
- Consumes: `BENCH_IMPL`, `BIND_ADDR`, and `OAS_RS_REVISION` environment variables.
- Produces: HTTP servers named `raw-hyper` and `oas-rs`, both listening on port 8080 and serving `GET /users/123456` with status 200, `GET /missing` with 404, and `POST /users/123456` with 405.

- [ ] **Step 1: Add server smoke-test expectations before implementation**

Create `/home/quang/code/oas-rs-perf/scripts/tests/test_servers.sh` that starts each Compose service, uses `curl --fail-with-body` for the 200 endpoint, checks `/missing` returns 404, checks POST returns 405, and exits nonzero if either implementation differs.

- [ ] **Step 2: Run the smoke test to verify it fails**

Run:

```bash
sh /home/quang/code/oas-rs-perf/scripts/tests/test_servers.sh
```

Expected: FAIL because the Compose services and server image do not yet exist.

- [ ] **Step 3: Implement the raw Hyper server**

Use Hyper HTTP/1 with Tokio TCP accept, parse only the benchmark endpoint contract, return a fixed `application/json` body, and keep connection handling enabled for HTTP keep-alive.

- [ ] **Step 4: Implement the `oas-rs` server**

Build an `oas_rs::App`, register `GET /users/{id}` with a typed `Path<u64>` handler returning the same JSON body, call `App::build`, and serve the bound listener with `AppRuntime::serve_listener`.

- [ ] **Step 5: Add reproducible image and Compose definitions**

Use a pinned Rust builder image, build in release mode with `lto=fat` from the core crate path, run a minimal runtime image, expose port 8080, and define separate services with identical CPU/memory settings. Keep `oha` separate from the server image.

- [ ] **Step 6: Run the smoke test and verify parity**

Run:

```bash
docker compose -f /home/quang/code/oas-rs-perf/docker-compose.yml config
sh /home/quang/code/oas-rs-perf/scripts/tests/test_servers.sh
```

Expected: Compose config succeeds and both services pass the identical status/body checks.

- [ ] **Step 7: Record the server/container change**

```bash
cd /home/quang/code/oas-rs-perf
git add Cargo.toml src Dockerfile docker-compose.yml .dockerignore scripts/tests/test_servers.sh
git commit -m "feat: add raw Hyper and oas-rs benchmark servers"
```

### Task 4: Add environment capture, paired `oha` matrix, and report generation

**Files:**
- Create: `/home/quang/code/oas-rs-perf/scripts/collect-environment.sh`
- Create: `/home/quang/code/oas-rs-perf/scripts/run-matrix.sh`
- Create: `/home/quang/code/oas-rs-perf/scripts/render-report.sh`
- Create: `/home/quang/code/oas-rs-perf/README.md`
- Create: `/home/quang/code/oas-rs-perf/.gitignore`

**Interfaces:**
- Consumes: `--official`, `--requests`, `--connections`, `--warmup-requests`, `--runs`, and `--output` options.
- Produces: `environment.md`, paired `raw-hyper-*.json`/`oas-rs-*.json` summaries, `REPORT.md`, `REPORT.json`, and status `ACCEPT`, `NOT ACCEPTED`, or `INCONCLUSIVE`.

- [ ] **Step 1: Add a runner dry-run test**

Create `/home/quang/code/oas-rs-perf/scripts/tests/test_runner_options.sh` that invokes `run-matrix.sh --help`, verifies the option names, and invokes `run-matrix.sh --official --dry-run` on a non-Linux host expecting `INCONCLUSIVE` rather than an acceptance claim.

- [ ] **Step 2: Run the option test and verify it fails**

Run:

```bash
sh /home/quang/code/oas-rs-perf/scripts/tests/test_runner_options.sh
```

Expected: FAIL because the runner and help output do not yet exist.

- [ ] **Step 3: Implement environment capture**

Record kernel, distribution, CPU model, logical CPU count, Docker version, Compose version, `oha --version`, revision, request count, concurrency, warm-up, keep-alive, and case list. In official mode, reject non-Linux hosts and Docker Desktop indicators.

- [ ] **Step 4: Implement the paired matrix runner**

For each implementation and each configured case, warm the endpoint, run pinned `oha` JSON summaries, save the raw files, and run the same load shape against both services. Default the matrix to one lightweight diagnostic run; official mode must require at least 7 paired runs, at least 1,000,000 requests per run, and VU levels 32/64/128/256/512.

- [ ] **Step 5: Implement report rendering**

Calculate median RPS and p50/p95/p99 values per implementation, error rate, throughput delta, latency overhead, and status. Emit both Markdown and JSON without embedding raw large artifacts.

- [ ] **Step 6: Run the option test and verify it passes**

Run the same test script. Expected: PASS, with non-Linux official mode reported as `INCONCLUSIVE`.

- [ ] **Step 7: Document local and official commands**

Document:

```bash
./scripts/run-matrix.sh --requests 10000 --connections 32 --warmup-requests 1000 --runs 1 --output results/diagnostic
./scripts/run-matrix.sh --official --output results/official
./scripts/validate-report.sh results/official/REPORT.json
```

- [ ] **Step 8: Record the runner/report change**

```bash
cd /home/quang/code/oas-rs-perf
git add scripts README.md .gitignore
git commit -m "feat: add paired Linux HTTP acceptance matrix"
```

### Task 5: Run full verification and produce the handoff report

**Files:**
- Create: `/home/quang/code/oas-rs-perf/results/20260815-diagnostic/REPORT.md`
- Create: `/home/quang/code/oas-rs-perf/results/20260815-diagnostic/environment.md`
- Do not add raw result JSON to the core `oas-rs` repository.

**Interfaces:**
- Consumes: core Rust verification commands, microbenchmark output, Docker Compose smoke tests, and one diagnostic or official HTTP matrix.
- Produces: an evidence-backed final report with exact commands and status.

- [ ] **Step 1: Run core verification**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo test --doc --workspace
cargo build --workspace --examples
cargo bench --bench router --features uuid,test-util,swagger
```

- [ ] **Step 2: Run lab verification**

```bash
docker compose -f /home/quang/code/oas-rs-perf/docker-compose.yml config
sh /home/quang/code/oas-rs-perf/scripts/tests/test_report_validator.sh
sh /home/quang/code/oas-rs-perf/scripts/tests/test_servers.sh
sh /home/quang/code/oas-rs-perf/scripts/tests/test_runner_options.sh
```

- [ ] **Step 3: Run the available HTTP matrix**

On dedicated Linux, run official mode. Otherwise run diagnostic mode and mark the result `INCONCLUSIVE`; never label it an official acceptance result.

- [ ] **Step 4: Inspect all evidence**

Check exit codes, expected status counts, report thresholds, environment manifest, Git diffs, and absence of lab files from the core repository.

- [ ] **Step 5: Hand off the result**

Report modified files, exact commands, benchmark output, environment, acceptance status, and any blocked step such as unavailable Linux host, unavailable `oha`, or read-only Git metadata.
