# Native benchmark native-current

Diagnostic native Windows run; this is not the dedicated Linux release gate.
Status: `INCONCLUSIVE` for release acceptance; the server was built from a
modified working tree based on `b5be453`.

| Case | Raw median RPS | OAS median RPS | Throughput delta |
| --- | ---: | ---: | ---: |
| users | 67577.66 | 68358.58 | -1.142% |
| users-static | 77209.37 | 79858.29 | -3.317% |

All measured requests must have the expected HTTP status and success rate 1.0; otherwise the script fails.
