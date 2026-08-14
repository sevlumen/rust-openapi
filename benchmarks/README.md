# oas-rs benchmark harness

This harness keeps the raw Hyper comparator and `oas-rs` binary in the same
release image, with identical memory limits and CPU affinity. The measured
scenario uses oha exact-request mode, a 30-second warm-up, and an explicit
zero-error/status threshold. oha writes one CSV row per request; the adapter
normalizes those rows into the harness JSON metrics schema.

Run the full release matrix from PowerShell:

```powershell
./benchmarks/run-benchmark.ps1
```

For a quick Docker smoke test (not a release gate):

```powershell
./benchmarks/run-benchmark.ps1 -Iterations 10000 -Runs 1 -Vus 32
```

Use `-WarmupSeconds 1` only for a functional smoke of the matrix. Official
runs keep the default 30-second warm-up.

The script refuses an official run on hosts with fewer than 12 logical
processors unless `-AllowUndersizedHost` is explicitly supplied. Before each
pair it validates the API/PostgreSQL CPU affinity and 1 GiB memory limit. The
retained stats CSV includes Docker CPU%, RSS, PIDs, and cgroup CPU usage; the
report derives CPU nanoseconds/request from the cgroup usage delta. Incomplete
request counts, missing CPU/RSS samples, negative timings, baseline CV above
0.5%, or any measured error remain `INCONCLUSIVE`. Official runs stop at the
first negative timing sample and write a partial `INCONCLUSIVE` report; use
`-ContinueAfterInvalidTiming` only when collecting diagnostic artifacts after
that invalidation.

The default matrix includes the P0/P1 HTTP cases (`validation-success`,
`problem`, `raw-handler`, and `security`) and `postgres` (16 persistent
`tokio-postgres` clients with one prepared statement per connection). Each
measured pair records p50/p95/p99/p999, zero-error counters, and sampled API
CPU/RSS CSV data. The full plan requires 7 measured runs per case, randomized
raw/framework pair order, 1,000,000 requests, and a dedicated host. A report
is marked `INCONCLUSIVE` unless the raw baseline CV, upper 95% confidence
bound, CPU/RSS samples, and minimum run/request counts are available; a single
smoke run is never treated as a performance pass or failure.
