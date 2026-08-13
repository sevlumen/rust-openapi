# oas-rs benchmark harness

This harness keeps the raw Hyper comparator and `oas-rs` binary in the same
release image, with identical memory limits and CPU affinity. The measured
scenario uses k6 `shared-iterations`, a 30-second warm-up, and an explicit
zero-error threshold.

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

The default matrix includes the P0/P1 HTTP cases (`validation-success`,
`problem`, `raw-handler`, and `security`) and `postgres` (16 persistent
`tokio-postgres` clients with one prepared statement per connection). Each
measured pair records p50/p95/p99/p999, zero-error counters, and sampled API
CPU/RSS CSV data. The full plan requires 7 measured runs per case, randomized
raw/framework pair order, 1,000,000 requests, and a dedicated host. A report
is marked `INCONCLUSIVE` unless the raw baseline CV, upper 95% confidence
bound, CPU/RSS samples, and minimum run/request counts are available; a single
smoke run is never treated as a performance pass or failure.
