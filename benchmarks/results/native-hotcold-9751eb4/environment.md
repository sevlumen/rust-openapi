# Environment

- commit: `9751eb4d45dd69d3e8619184af19a8f02fe909e2`
- platform: Windows 10 Pro, build `26200`
- CPU: AMD Ryzen 7 5700X 8-Core Processor
- CPU topology: 8 cores / 16 logical processors
- Rust: `rustc 1.96.0 (ac68faa20 2026-05-25)`
- target: `x86_64-pc-windows-msvc`
- oha: `1.15.0`
- server: native Windows process, loopback `127.0.0.1:18080`
- Docker: not used
- profile: Cargo `release` (`opt-level=3`, fat LTO, one codegen unit)
- CPU affinity/governor: not pinned or fixed

Command shape:

```text
oha -n 1m -c 256 --no-tui --no-color --output-format json <URL>
```

