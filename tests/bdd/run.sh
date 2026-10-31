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
output=$(mktemp)
trap 'rm -f "$output"' EXIT HUP INT TERM
if docker run --rm -e PG_ATTACH_LOG_DIR=/logs \
  -v "$log_dir:/logs" "pg-attach-bdd-pg${PG_VERSION}:dev" >"$output" 2>&1; then
  status=0
else
  status=$?
fi
cat "$output"
[ "$status" -eq 0 ] || exit "$status"
if grep -Eq '[1-9][0-9]* skipped' "$output"; then
  echo 'BDD scenarios were skipped; treat the run as failed' >&2
  exit 1
fi
