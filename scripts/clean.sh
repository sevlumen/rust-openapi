#!/usr/bin/env bash
# Remove the build artifacts and caches that this repository's tooling creates.
#
#   scripts/clean.sh
#
# What it removes (all of it is regenerated on demand, nothing is source):
#   - target/                      cargo build output on this machine
#   - Docker volume oasrs-target   build cache used by scripts/verify-docker.sh
#   - Docker volume oasrs-cargo    registry cache used by scripts/verify-docker.sh
#
# These grow quickly (the Docker target volume reached 12 GB across feature
# combinations), so run this after finishing a task or branch. The next
# scripts/verify-docker.sh run rebuilds them (a few minutes, plus a download).
set -uo pipefail
cd "$(dirname "$0")/.."

echo "Removing target/ ..."
rm -rf target

if command -v docker >/dev/null 2>&1; then
  for volume in oasrs-target oasrs-cargo; do
    if docker volume inspect "$volume" >/dev/null 2>&1; then
      if docker volume rm "$volume" >/dev/null 2>&1; then
        echo "Removed Docker volume $volume"
      else
        echo "Could not remove Docker volume $volume (is a container using it?)" >&2
      fi
    fi
  done
else
  echo "Docker not found: skipped the Docker volumes."
fi
echo "Clean."
