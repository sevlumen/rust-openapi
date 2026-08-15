# Native focused benchmark: bodyless fast path

Status: **INCONCLUSIVE — diagnostic only**

This run measures commit `526a3615d7dceff25bd6db8f07e99ac7c9026ece`, which
avoids constructing `Request<Bytes>` for `Zero`, `Static`, and `Builtin` routes
in the native server dispatch path. It is a direct Windows loopback run; Docker
was not used.

Each implementation/endpoint measurement used 1,000,000 requests and 256
connections. There were three paired runs, with raw/OAS order varied between
runs. Every measurement returned exactly 1,000,000 HTTP 200 responses.

| Endpoint | Raw median RPS | OAS median RPS | OAS-vs-raw overhead |
| --- | ---: | ---: | ---: |
| `/raw-handler` | 147,486.61 | 148,745.84 | -0.85% |
| `/users-static` | 139,016.70 | 139,810.22 | -0.57% |

Individual RPS values:

| Run | `/raw-handler` raw | `/raw-handler` OAS | `/users-static` raw | `/users-static` OAS |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 146,114.44 | 147,951.90 | 138,671.44 | 138,853.83 |
| 2 | 147,486.61 | 148,745.84 | 139,016.70 | 139,810.22 |
| 3 | 148,013.11 | 150,323.77 | 139,268.66 | 141,096.78 |

The result is useful evidence that the focused run has no visible regression,
but it is not a release acceptance result: Windows native timing was used,
there was no CPU affinity/governor control, no warmup, no exact cgroup CPU/RSS
measurement, and the raw baseline was not collected in the same experiment
before this change. Dedicated Linux acceptance remains required.

## Exact load command

```powershell
oha -n 1000000 -c 256 --no-tui --no-color --output-format json `
  http://127.0.0.1:18080/raw-handler

oha -n 1000000 -c 256 --no-tui --no-color --output-format json `
  http://127.0.0.1:18080/users-static
```

The server was started separately for each measurement with
`OAS_IMPLEMENTATION=raw` or `OAS_IMPLEMENTATION=oas` and
`OAS_LISTEN=127.0.0.1:18080`.
