# Native focused benchmark — `9751eb4`

Diagnostic comparison after commit `9751eb4d45dd69d3e8619184af19a8f02fe909e2`.
The server and load generator ran directly on the Windows host; Docker was not
used.

## Method

- release binary: `target/release/oas-bench-server.exe`
- load generator: `oha 1.15.0`
- request count: `1,000,000` per measurement
- connections: `256`
- measured pairs: 3 raw Hyper runs and 3 oas-rs runs per case
- order: raw then oas for each case/run
- warm-up: none
- response validation: every run reported exactly `1,000,000` responses,
  `successRate=1`, and `200=1,000,000`

## Results

Overhead is `(raw median RPS / oas-rs median RPS - 1) * 100`.

| Case | Raw median RPS | oas-rs median RPS | Overhead |
| --- | ---: | ---: | ---: |
| `/raw-handler` | 147,361.01 | 149,190.39 | -1.226% |
| `/users-static` | 140,580.53 | 136,942.89 | 2.656% |
| `/users/123456` | 143,992.12 | 142,587.40 | 0.985% |

Per-run RPS is retained in the adjacent oha JSON files. The result is
diagnostic only: three runs, no warm-up, Windows scheduling noise, no fixed
CPU affinity/governor, and no cgroup CPU/RSS measurement. It is not evidence
for the final Linux `<=1%` release gate.

