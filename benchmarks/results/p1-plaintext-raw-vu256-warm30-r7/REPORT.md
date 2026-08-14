# Benchmark report p1-plaintext-raw-vu256-warm30-r7

Status: INCONCLUSIVE

This run collected 28 raw/framework measurements. Results are
classified only after the minimum run/request counts, raw-baseline CV, paired
95% bootstrap percentile bound for the paired median overhead, exact cgroup
CPU before/after samples, and RSS samples are available.

| Test | Raw RPS | OAS RPS | Overhead | p50 raw/oas (ms) | p95 raw/oas (ms) | p99 raw/oas (ms) | Raw CV | CPU ns/request raw/oas | CPU Δ | RSS peak raw/oas (MiB) | Result |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| plaintext (VU 256) | 216748.32 | 215439.88 | 0.604% | 0.5656/0.5741 | 4.6974/4.7047 | 8.5673/8.4481 | 4.234% | 16639.28/16915.78 | 1.662% | 7.3/7.9 | INCONCLUSIVE |
| raw-handler (VU 256) | 206405.33 | 198803.98 | 3.683% | 0.5978/0.6263 | 4.7846/4.8223 | 8.566/8.6609 | 14.131% | 17659.7/18422.59 | 4.32% | 7.8/8.1 | INCONCLUSIVE |

Timing invalidation: no negative latency samples detected in completed rows.
Observed invalid timing fields: none.
Execution completed all requested case/VU/run loops.
Official matrix guard: not requested; use -Official for release acceptance.

The statistical gate uses median paired throughput overhead, the upper 95%
percentile bootstrap CI from 10,000 resamples (seed 8675309), p50/p95/p99
latency deltas, raw baseline CV, zero measured errors, exact measured request
counts, cgroup CPU usage nanoseconds/request, memory samples, and the requested
run/request minimums. Any negative timing sample or incomplete request count
invalidates the row. p999 is retained in the JSON artifacts for warning analysis.
Authoritative CPU/request values use cgroup usage_usec captured immediately
before and after each measured oha run; docker stats remains charting evidence.
Allocation metrics are reported by the in-process router benchmark.

Raw result files and the exact environment must be retained beside this file.
