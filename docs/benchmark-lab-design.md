# Comparison laboratory: `oas-rs` against other servers

Status: implemented in the sibling project `oas-rs-perf` (`../oas-rs-perf`, first diagnostic run done,
outside this workspace, as `docs/benchmark-design.md` requires). This document
extends that design from "raw Hyper vs `oas-rs`" to a multi-framework comparison.

## Goal and authority

Measure where `oas-rs` stands against raw Hyper, axum, actix-web and three
servers from other ecosystems (Go `net/http`, Node Fastify, Python FastAPI),
under one HTTP contract. Results on a developer machine (Windows, Docker
Desktop) are **diagnostic**: they show relative position and catch regressions,
and are never quoted as numbers. Only a run on a dedicated Linux host, with the
environment manifest recorded, may be labelled official (`ACCEPT` /
`NOT ACCEPTED` / `INCONCLUSIVE`, as in `docs/benchmark-design.md`).

## The contract (every server implements it byte-for-byte equivalently)

| Endpoint | Behaviour |
|---|---|
| `GET /plaintext` | `200`, `text/plain`, body `Hello, World!` |
| `GET /json` | `200`, JSON `{"message":"Hello, World!"}` |
| `GET /users/{id}` (u32) | `200`, JSON `{"id":<id>,"name":"user-<id>"}` |
| `GET /search?q=<text>&limit=<u32>` | `200`, JSON `{"q":"<text>","limit":<n>}` |
| `POST /echo` (JSON `{"n":u64,"text":string}`) | `200`, the same JSON back |
| `GET /missing` | `404` |

Servers read `PORT` (and `THREADS`, default 4) from the environment and bind
`127.0.0.1`. A contract check runs before any measurement; a server that
answers wrongly is excluded from that run and reported as such.

## Fairness rules

- Each server gets the same number of worker threads (`THREADS`, default 4):
  Tokio workers, actix workers, `GOMAXPROCS`, Node `cluster` workers, uvicorn
  workers. Processes are pinned to a fixed CPU set and the load generator to
  another, disjoint set.
- Release builds with the same profile for the Rust servers (`opt-level=3`,
  fat LTO, `codegen-units=1`); HTTP/1.1 keep-alive; no logging, no middleware.
- Framework-typed handlers where the framework offers them (path, query and
  JSON extractors): that is what the comparison is about. `raw-hyper` parses by
  hand and is the baseline.
- Server order is shuffled in every repetition; each server is a fresh process
  for each repetition.

## Measurements

Load from `oha` (JSON output): requests/s, p50/p95/p99 latency, status-code and
error counts. From the server process: CPU time per request and peak RSS.
Profiles: `quick` (4 endpoints, 32 connections, 2 s warm-up, 5 s measure, 3
repetitions) and `full` (all endpoints, 1/32/256 connections, 10 s warm-up,
30 s measure, 5 repetitions). The median of the repetitions is reported with
its min-max range.

## Output

`results/<timestamp>/` holds the raw `oha` JSON of every run, `manifest.json`
(versions, CPU, OS, build profile, thread and pinning settings) and
`REPORT.md` / `REPORT.json`: per endpoint, a table of servers by requests/s with
the ratio to the `raw-hyper` baseline, latency percentiles, CPU per 1,000
requests and RSS. A run not on Linux, or without a manifest, is stamped
`DIAGNOSTIC - not citable` at the top of the report.

## Out of scope

HTTP/2 and TLS comparisons, databases, feature comparison, and any claim from a
non-Linux run. Docker mode (`docker-compose.yml`, per-container CPU limits) is
provided for the dedicated host; the native mode is for quick local checks.
