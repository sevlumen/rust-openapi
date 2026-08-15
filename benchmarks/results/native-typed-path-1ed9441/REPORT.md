# Native focused benchmark: normalized path reuse

Status: **INCONCLUSIVE — diagnostic only**

Commit `1ed9441` passes the normalized path length from route resolution to
bodyless typed dispatch, avoiding a second `normalize_request_path()` call.
The workload intentionally exercises the typed dynamic capture route
`/users/123456`; it is not the static/zero-handler workload.

This was a direct native Windows loopback run without Docker. Each
implementation used 1,000,000 requests and 256 connections. There were three
paired runs and every measurement returned exactly 1,000,000 HTTP 200
responses.

| Endpoint | Raw median RPS | OAS median RPS | OAS-vs-raw overhead |
| --- | ---: | ---: | ---: |
| `/users/123456` | 150,994.42 | 150,059.53 | 0.62% |

Individual RPS values:

| Run | Raw RPS | OAS RPS |
| ---: | ---: | ---: |
| 1 | 147,423.15 | 148,220.72 |
| 2 | 150,994.42 | 150,243.35 |
| 3 | 151,653.66 | 150,059.53 |

The result is a focused diagnostic signal, not release acceptance. It has no
CPU affinity/governor control, no warmup, no exact cgroup CPU/RSS accounting,
and no dedicated Linux monotonic-clock evidence. The raw implementation is a
separate Hyper comparator, so this ratio is not a proof that the framework
meets the final 1% contract.

## Exact load command

```powershell
oha -n 1000000 -c 256 --no-tui --no-color --output-format json `
  http://127.0.0.1:18080/users/123456
```

The server was started separately for each measurement with
`OAS_IMPLEMENTATION=raw` or `OAS_IMPLEMENTATION=oas` and
`OAS_LISTEN=127.0.0.1:18080`.
