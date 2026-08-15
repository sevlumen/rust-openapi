# Native focused benchmark — `7b59316`

Status: `INCONCLUSIVE` (diagnostic only)

This run was executed directly on the Windows host with the release binary,
not through Docker. It is not a release acceptance run: there is one raw/OAS
pair per endpoint, no CPU affinity, no exact CPU/RSS gate, and no Linux
monotonic-clock evidence.

Command shape:

```text
oha -n 1m -c 256 --no-tui --output-format json http://127.0.0.1:18083/<path>
```

| Endpoint | Raw RPS | OAS RPS | Throughput overhead | Requests each | Errors |
|---|---:|---:|---:|---:|---:|
| `/raw-handler` | 143,610.18 | 146,670.62 | -2.13% | 1,000,000 | 0 |
| `/users-static` | 138,667.79 | 140,437.70 | -1.28% | 1,000,000 | 0 |

Negative overhead means OAS measured faster in this single pair. Treat these
figures as a smoke/diagnostic result only; they do not establish the <=1%
acceptance contract.
