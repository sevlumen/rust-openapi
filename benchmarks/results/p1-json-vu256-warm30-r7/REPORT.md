# Benchmark report p1-json-vu256-warm30-r7

Status: INCONCLUSIVE

This run collected 28 raw/framework measurements. Results are
classified only after the minimum run/request counts, raw-baseline CV, paired
95% bootstrap percentile bound for the paired median overhead, exact cgroup
CPU before/after samples, and RSS samples are available.

| Test | Raw RPS | OAS RPS | Overhead | p50 raw/oas (ms) | p95 raw/oas (ms) | p99 raw/oas (ms) | Raw CV | CPU ns/request raw/oas | CPU Δ | RSS peak raw/oas (MiB) | Result |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| json-100-users (VU 256) | 86322.6 | 85941.96 | 0.441% | 2.8483/2.8698 | 5.298/5.3506 | 6.5831/6.6659 | 3.479% | 45998.69/46242.67 | 0.53% | 9/9.7 | INCONCLUSIVE |
| users-static (VU 256) | 129522.99 | 125602.12 | 3.027% | 1.8998/1.9523 | 3.4603/3.5821 | 4.3084/4.4301 | 3.303% | 30557.68/31592.3 | 3.386% | 7.5/7.6 | INCONCLUSIVE |

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
