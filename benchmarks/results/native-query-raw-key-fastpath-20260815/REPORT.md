# Native focused benchmark: generated query raw-key fast path

## Hypothesis

The derive-generated direct query parser decoded every key before matching it
against field names. For ordinary query strings, generated field names are
already plain ASCII. Match those raw keys first, and retain the existing full
percent-decoding fallback for encoded or unknown keys.

## Workload

```text
GET /search?page=42&active=true
derived query: { page: u32, active: bool }
release profile, native Windows process, no Docker
2,000,000 oneshot requests/run
5 runs per variant
```

## Results

| Variant | Runs (ns/op) | Median | Allocations/op | Bytes/op |
| --- | --- | ---: | ---: | ---: |
| baseline (decode key first) | 373.75, 371.91, 374.99, 374.04, 374.67 | 374.04 | 3 | 683 |
| candidate (raw-key match) | 352.29, 355.39, 358.04, 352.67, 362.11 | 355.39 | 3 | 683 |

Candidate median is approximately **4.99% lower ns/request**. Allocation
counts and bytes are unchanged.

## Correctness

The candidate preserves the fallback path for encoded names and malformed
values. Regression coverage includes:

```text
%70age=42&active=true       -> 200
unknown=%ZZ&page=42&...     -> 400
```

The full test and lint gates passed before promotion.

## Decision

Promote the raw-key fast path. This is a native Windows microbenchmark result,
not the dedicated Linux release acceptance gate; it does not establish the
framework-wide <=1% contract or permit freezing v0.1.
