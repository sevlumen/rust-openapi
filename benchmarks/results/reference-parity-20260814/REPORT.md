# Benchmark report reference-parity-20260814

Benchmark head: `4accada`; the measured server source is unchanged from
`81a1f95640c564b0a2afb3ee9c85d69d489dc75b`. The later commit contains only
the harness/adapter documentation change used to produce this report.

Status: INCONCLUSIVE

This run collected 20 raw/framework measurements. Results are
classified only after the minimum run/request counts, raw-baseline CV, paired
95% bootstrap percentile bound for the paired median overhead, exact cgroup
CPU before/after samples, and RSS samples are available.

| Test | Raw RPS | OAS RPS | Overhead | p50 raw/oas (ms) | p95 raw/oas (ms) | p99 raw/oas (ms) | Raw CV | CPU ns/request raw/oas | CPU Δ | RSS peak raw/oas (MiB) | Result |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| json-100-users (VU 32) | 87740.85 | 88167.87 | -0.487% | 0.354/0.3542 | 0.5956/0.5853 | 0.7335/0.7126 | 0% | 45307.07/45102.56 | -0.451% | 2.6/3.1 | INCONCLUSIVE |
| json-100-users (VU 64) | 91305.54 | 85612.54 | 6.235% | 0.6835/0.7241 | 1.143/1.2296 | 1.4089/1.5393 | 0% | 43504.92/46304.33 | 6.435% | 3.8/3.6 | INCONCLUSIVE |
| json-100-users (VU 128) | 90314.52 | 90169.3 | 0.161% | 1.376/1.3834 | 2.3479/2.3454 | 2.9312/2.8958 | 0% | 43922.32/44061.41 | 0.317% | 5.2/5.5 | INCONCLUSIVE |
| json-100-users (VU 256) | 90120.93 | 87797.42 | 2.578% | 2.7292/2.8098 | 5.0178/5.1295 | 6.2348/6.36 | 0% | 44018.88/45173 | 2.622% | 8.2/8.2 | INCONCLUSIVE |
| json-100-users (VU 512) | 89163.69 | 84606.14 | 5.111% | 5.125/5.3861 | 10.4757/11.1176 | 12.8787/13.7909 | 0% | 44395.04/46810.12 | 5.44% | 13.9/14.6 | INCONCLUSIVE |
| users-static (VU 32) | 125729.47 | 122724.19 | 2.39% | 0.2439/0.2497 | 0.4178/0.4297 | 0.5307/0.5467 | 0% | 31439.22/32241.1 | 2.551% | 2/2.4 | INCONCLUSIVE |
| users-static (VU 64) | 128623.3 | 124305.43 | 3.357% | 0.4799/0.4984 | 0.8183/0.8368 | 1.0247/1.0368 | 0% | 30761.12/31818.97 | 3.439% | 2.7/2.9 | INCONCLUSIVE |
| users-static (VU 128) | 118470.31 | 117912.73 | 0.471% | 1.0285/1.044 | 1.8482/1.8126 | 2.4/2.2783 | 0% | 33337.13/33561.84 | 0.674% | 4/4 | INCONCLUSIVE |
| users-static (VU 256) | 124482.88 | 113928.37 | 8.479% | 1.9682/2.1449 | 3.6008/3.9594 | 4.4891/4.9652 | 0% | 31855.91/34709.33 | 8.957% | 6.5/6.4 | INCONCLUSIVE |
| users-static (VU 512) | 122588.35 | 115798.29 | 5.539% | 3.6934/4.1037 | 7.2516/7.7178 | 8.9471/9.5288 | 0% | 32213.2/34216.19 | 6.218% | 10.6/11.7 | INCONCLUSIVE |

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
