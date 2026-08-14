# Benchmark report p0-zero-fastpath-focused-rerun

Status: INCONCLUSIVE

This run collected 40 raw/framework measurements. Results are
classified only after the minimum run/request counts, raw-baseline CV, paired
95% bootstrap percentile bound for the paired median overhead, exact cgroup
CPU before/after samples, and RSS samples are available.

| Test | Raw RPS | OAS RPS | Overhead | p50 raw/oas (ms) | p95 raw/oas (ms) | p99 raw/oas (ms) | Raw CV | CPU ns/request raw/oas | CPU Δ | RSS peak raw/oas (MiB) | Result |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| json-100-users (VU 256) | 86778.29 | 84515.4 | 2.608% | 2.8407/2.8987 | 5.21/5.4229 | 6.429/6.806 | 4.124% | 45717.67/47008.22 | 2.823% | 8.1/8.7 | INCONCLUSIVE |
| users-static (VU 256) | 123356.74 | 119799.56 | 2.884% | 1.9824/2.0482 | 3.6254/3.7322 | 4.4886/4.5801 | 1.239% | 32181.89/33080.34 | 2.792% | 6.4/7 | INCONCLUSIVE |
| plaintext (VU 256) | 216104.87 | 212930.79 | 1.469% | 0.5701/0.5807 | 4.7113/4.7164 | 5.1/8.3847 | 1.209% | 16670.47/17191.15 | 3.123% | 6.1/7 | INCONCLUSIVE |
| raw-handler (VU 256) | 218158.44 | 215732.58 | 1.112% | 0.5705/0.5736 | 4.6841/4.6995 | 5.1995/8.6092 | 1.436% | 16727.05/16994.22 | 1.597% | 6.3/6.8 | INCONCLUSIVE |

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
