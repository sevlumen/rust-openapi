# Native benchmark native-route-split-fd6648f

Diagnostic native Windows run; this is not the dedicated Linux release gate.

| Case | Raw median RPS | OAS median RPS | Throughput delta |
| --- | ---: | ---: | ---: |
| users | 116257.47 | 117855.73 | -1.356% |
| users-static | 138198.78 | 140083.72 | -1.346% |

All measured requests must have the expected HTTP status and success rate 1.0; otherwise the script fails.
