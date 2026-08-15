# Native microbench: borrow CaptureSet range access

Date: 2026-08-15
Host: Windows 11 x64, native process (no Docker)
Command: `cargo bench --bench router`
Workload: `OAS_BENCH_ITERATIONS=1000000`, release bench profile

`CaptureSet::range` now borrows `&self` instead of taking the 72-byte
`CaptureSet` by value. This changes only an accessor; packed layout and trie
backtracking are unchanged.

| Variant | Params ns/op | allocations/op | bytes/op |
| --- | ---: | ---: | ---: |
| baseline: `range(self, ...)` | 563.03 | 8 | 963 |
| candidate: `range(&self, ...)` | 534.52; 526.44 | 8 | 963 |

Candidate median is approximately 5.8% below the paired baseline. The result
is still Windows diagnostic evidence, not the Linux 1% acceptance gate. The
zero-allocation path budgets remain unchanged.

Correctness/layout gates:

- `cargo test --all-targets`: pass
- `cargo clippy --all-targets -- -D warnings`: pass
- `cargo fmt --all`: pass
- `RoutePlan=32`, `Params=80`, `DynamicRouteNode=88`

No dynamic trie algorithm or unsafe storage was changed in this variant.
