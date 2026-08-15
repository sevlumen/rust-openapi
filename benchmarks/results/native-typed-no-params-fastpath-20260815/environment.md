# Environment

- Host: Windows 11, build 26200
- Execution: native Windows process; Docker not used
- Rust: `rustc 1.96.0`, `cargo 1.96.0`
- Profile: Cargo `bench` / release profile (`opt-level=3`, fat LTO, one codegen unit)
- Base source before candidate: `bf5e414` (`perf: borrow packed capture accessors`)
- Workload: 2,000,000 iterations per run, 5 runs per variant
- CPU affinity/governor: not pinned
- cgroup CPU/RSS accounting: not applicable
