# oas-rs benchmark harness

This harness keeps the raw Hyper comparator and `oas-rs` binary in the same
release image, with identical memory limits and CPU affinity. The measured
scenario uses oha exact-request mode, a 30-second warm-up, and an explicit
zero-error/status threshold. Normal diagnostic/official runs retain one CSV row
per request; the reference profile uses oha's summary JSON instead so the load
generator's bind-mounted per-request CSV I/O cannot dominate a 1,000,000-request
run. The JSON adapter still checks the exact response count and expected status.

Run the full release matrix from PowerShell:

```powershell
./benchmarks/run-benchmark.ps1 -Official
```

`-Official` is a hard release-matrix guard. It requires every configured case,
VU 32/64/128/256/512, at least 7 paired runs, and at least 1,000,000 requests
per run; missing any tuple forces `INCONCLUSIVE`.

To reproduce the A/B workload from `D:\Code\demo` as closely as this harness
allows, run the two 100-user endpoints with the same VU sweep and request
count:

```powershell
./benchmarks/run-benchmark.ps1 -ReferenceProfile -AllowUndersizedHost -Version reference-compatible
```

This profile is diagnostic: it uses one run and no additional warm-up, so it
cannot be a release PASS. It keeps the reference paths and payload shape
(`/users` and `/users-static`) while retaining the raw-vs-oas paired
comparison. The reference k6 numbers must not be compared directly with the
oha raw-vs-oas overhead; match endpoint, payload, VU, request count, CPU
affinity, memory limit, and database phase first. Because oha has no
discard-response-bodies mode, its absolute RPS is tool-specific; use the
dedicated Linux k6 run for final comparison with `D:\Code\demo`.

For a quick Docker smoke test (not a release gate):

```powershell
./benchmarks/run-benchmark.ps1 -Iterations 10000 -Runs 1 -Vus 32
```

Use `-WarmupSeconds 1` only for a functional smoke of the matrix. Official
runs keep the default 30-second warm-up.

The script refuses an official run on hosts with fewer than 12 logical
processors unless `-AllowUndersizedHost` is explicitly supplied. Before each
pair it validates the API/PostgreSQL CPU affinity and 512 MiB memory limit. The
retained stats CSV includes Docker CPU%, RSS, PIDs, and cgroup CPU usage; the
report derives CPU nanoseconds/request from the cgroup usage delta. Incomplete
request counts, missing CPU/RSS samples, negative timings, baseline CV above
0.5%, or any measured error remain `INCONCLUSIVE`. Official runs stop at the
first negative timing sample and write a partial `INCONCLUSIVE` report; use
`-ContinueAfterInvalidTiming` only when collecting diagnostic artifacts after
that invalidation.

The default matrix includes reference-compatible `/users` and `/users-static`
plus the P0/P1 HTTP cases (`validation-success`,
`problem`, `raw-handler`, and `security`) and `postgres` (16 persistent
`tokio-postgres` clients with one prepared statement per connection). Each
measured pair records p50/p95/p99/p999, zero-error counters, and sampled API
CPU/RSS CSV data. The full plan requires 7 measured runs per case, randomized
raw/framework pair order, 1,000,000 requests, and a dedicated host. A report
is marked `INCONCLUSIVE` unless the raw baseline CV, upper 95% confidence
bound, CPU/RSS samples, and minimum run/request counts are available; a single
smoke run is never treated as a performance pass or failure.

The default container limits match the reference A/B benchmark at 512 MiB per
API. A/B runs do not start PostgreSQL; the `postgres` case starts it explicitly
with the reference DB-phase CPU set (`0-3`).
Each result directory archives `REPORT.md`, `environment.md`, `manifest.txt`,
version files, and the per-request CSV/JSON evidence.
