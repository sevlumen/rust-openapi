# Native diagnostic: borrow JSON request body

Date: 2026-08-15
Host: Windows 11 x64, native process (no Docker)
Command: `cargo bench --bench router`
Workload: `OAS_BENCH_ITERATIONS=1000000`, release bench profile

`Json<T>::from_request` now passes a borrowed `&[u8]` to
`serde_json::from_slice` instead of cloning the request `Bytes` handle first.
The benchmark case was added as `case=json-request`.

| Variant | ns/op | allocations/op | bytes/op |
| --- | ---: | ---: | ---: |
| baseline, `request.body().clone()` | 583.13; 587.13 | 7 | 1344 |
| candidate, borrowed body | 576.14; 591.16 | 7 | 1344 |

The paired medians are effectively equal on this Windows host; this is not
claimed as a statistically significant throughput win. The candidate removes
an unnecessary `Bytes` refcount operation and preserves zero-allocation/byte
parity, so it remains the preferred borrow-first implementation.

Correctness gates:

- `cargo test --all-targets`: pass
- `cargo clippy --all-targets -- -D warnings`: pass after the final borrow fix
- `cargo fmt --all`: pass

This report is diagnostic and does not establish Linux release acceptance.
