#!/bin/sh
set -eu

PG_VERSION=${PG_VERSION:-17}
root=$(CDPATH= cd "$(dirname "$0")/../.." && pwd)

docker build --build-arg PG_VERSION="$PG_VERSION" \
  -t "pg-attach-pg${PG_VERSION}:dev" "$root"
docker build -f "$root/tests/bdd/Dockerfile" \
  --build-arg PG_VERSION="$PG_VERSION" \
  -t "pg-attach-bdd-pg${PG_VERSION}:dev" "$root"
log_dir="$root/target/bdd-logs/$PG_VERSION"
mkdir -p "$log_dir"
exec docker run --rm -e PG_ATTACH_LOG_DIR=/logs \
  -v "$log_dir:/logs" "pg-attach-bdd-pg${PG_VERSION}:dev"
