# Native diagnostic: compiled buffered body limit

Date: 2026-08-15
Host: Windows 11 x64, native process (no Docker)
Command: `cargo bench --bench router`
Workload: `OAS_BENCH_ITERATIONS=1000000`, release bench profile

The change moves the buffered-body limit from a runtime global constant into
each `RoutePlan` as a packed `u32`. `AppBuilder::max_body_size()` updates
buffered plans at build/registration time. Raw `Incoming` routes keep a zero
limit field and remain streaming.

Layout gate:

```text
HandlerFuture=80 InlineFuture=80 Params=80 HandlerKind=24 RoutePlan=32
```

Representative final run:

| Case | ns/op | allocations/op | bytes/op |
| --- | ---: | ---: | ---: |
| plaintext | 306.85 | 3.0000 | 666 |
| static-text | 314.38 | 3.0000 | 663 |
| static-json-fast | 273.74 | 3.0000 | 664 |
| path-integer | 355.44 | 3.0000 | 669 |
| params | 545.95 | 8.0000 | 963 |

Windows timing is diagnostic only; this run does not establish the official
1% acceptance result.

Correctness gates:

- `cargo test --all-targets`: pass (20 library, 25 acceptance, compile and fuzz smoke)
- `cargo clippy --all-targets -- -D warnings`: pass
- `cargo fmt --all`: pass
- configured `4` byte limit returns `413 Payload Too Large` before JSON collection
