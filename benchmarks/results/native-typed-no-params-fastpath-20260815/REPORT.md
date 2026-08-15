# Native focused benchmark: `TypedNoParams` fast path

## Hypothesis

Typed routes whose extractor set never reads path captures do not need the
`CaptureProvider`/`Params` dispatch layer. Compiling them as `TypedNoParams`
should avoid the dynamic capture provider and the empty `Params` value while
preserving the same `Handler::call` and extractor semantics.

## Workload

```text
GET /trace/abc123
header: x-trace-id: abc123
handler: Header<BenchTrace> -> "OK"
release profile, native Windows process, no Docker
2,000,000 oneshot requests/run
5 runs per variant
```

## Results

| Variant | Runs (ns/op) | Median | Allocations/op | Bytes/op |
| --- | --- | ---: | ---: | ---: |
| baseline `Typed` | 532.71, 537.08, 546.71, 558.39, 542.65 | 542.65 | 7 | 1341 |
| candidate `TypedNoParams` | 532.12, 527.36, 540.69, 537.49, 541.44 | 537.49 | 7 | 1341 |

Candidate median is approximately **0.95% lower ns/request** than baseline.
Allocations and allocated bytes are unchanged. This is a focused native
diagnostic result, not the official 1% acceptance gate: runs were sequential
within each variant on Windows and no CPU affinity/governor control was used.

## Correctness

The candidate passed:

```text
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git diff --check
```

The regression test covers a dynamic route with a header extractor and asserts
that it is compiled as `TypedNoParams` while still returning `200 OK`.

## Decision

Promote the candidate as a safe, scoped fast path. Keep the Linux dedicated
host matrix as the authoritative release decision; this report does not prove
the framework-wide <=1% contract or permit freezing v0.1.
