# Native microbench: precomputed header name

Date: 2026-08-15
Host: Windows 11 x64, native process (no Docker)
Benchmark: `cargo bench --bench router`
Workload: `OAS_BENCH_ITERATIONS=1000000`, release bench profile

This is a focused A/B diagnostic, not the official 1% acceptance matrix.
Both sides used the same `is_head` dispatch refactor; the only A/B difference
was `Header<T>` lookup through `T::NAME` versus the new compile-time
`T::HEADER_NAME`.

| Variant | Header ns/op | Raw comparator ns/op | Allocations/op | Bytes/op |
| --- | ---: | ---: | ---: | ---: |
| baseline: `get(T::NAME)` | 546.13 | 519.23 | 7 / 9 | 1334 / 1360 |
| candidate: `get(&T::HEADER_NAME)` | 518.52 | 517.43 | 7 / 9 | 1334 / 1360 |

Candidate delta: **-5.06%** for the typed header case. Allocation counts and
bytes are unchanged. A second candidate run measured `516.38 ns/op`; Windows
microbench noise remains significant, so this evidence is diagnostic only.

Correctness gates after the candidate:

- `cargo test --all-targets`: pass
- `cargo clippy --all-targets -- -D warnings`: pass
- `cargo fmt --all`: pass

This report does not establish Linux throughput, CPU/request, latency, RSS, or
release-freeze acceptance.
