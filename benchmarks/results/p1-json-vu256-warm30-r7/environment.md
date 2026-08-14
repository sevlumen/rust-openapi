# Benchmark environment

- Git commit: 331164d591a755b3bf940dcca85dd947214cf29d
- CPU: AMD Ryzen 7 5700X 8-Core Processor             
- Physical cores: 8
- Logical processors: 16
- Max clock MHz: 3401
- Host: Gigabyte Technology Co., Ltd. X570 GAMING X
- RAM bytes: 34276425728
- OS: Microsoft Windows 11 Pro 10.0.26200 build 26200
- API CPU affinity:  (default 0-3)
- PostgreSQL CPU affinity:  (default 0-3; matches the reference DB phase)
- Load-generator CPU affinity:  (default 4-7; matches the reference A/B benchmark)
- API/PostgreSQL memory limit: 512m / 512m
- Host logical processors observed: 16
- Official topology guard: at least 12 logical processors unless -AllowUndersizedHost was explicitly used
- Rust compiler:

rustc 1.96.0 (ac68faa20 2026-05-25)
binary: rustc
commit-hash: ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96
commit-date: 2026-05-25
host: x86_64-pc-windows-msvc
release: 1.96.0
LLVM version: 22.1.2

- Docker Compose: Docker Compose version v5.1.0
- Benchmark build image: rust:1.88.0-bookworm
- PostgreSQL image: postgres:16.4-bookworm
- oha image: ghcr.io/hatoo/oha:1.15.0 (digest pinned in docker-compose.yml)
- Release profile: opt-level=3, lto=fat, codegen-units=1, panic=abort, strip=true
- Dependencies are locked by Cargo.lock.

Docker version details are retained in docker-version.txt.
