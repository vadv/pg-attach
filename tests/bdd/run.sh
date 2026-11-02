#!/bin/sh
set -eu

PG_VERSION=${PG_VERSION:-17}
SOURCE_PG_VERSION=${SOURCE_PG_VERSION:-$PG_VERSION}
TARGET_PG_VERSION=${TARGET_PG_VERSION:-$PG_VERSION}
root=$(CDPATH= cd "$(dirname "$0")/../.." && pwd)

if [ "$SOURCE_PG_VERSION" != "$TARGET_PG_VERSION" ]; then
  for major in 16 17 18; do
    docker build --build-arg PG_VERSION="$major" \
      -t "pg-attach-pg${major}:dev" "$root"
  done
  docker build -f "$root/tests/bdd/Dockerfile.cross" \
    --build-arg SOURCE_PG_VERSION="$SOURCE_PG_VERSION" \
    --build-arg TARGET_PG_VERSION="$TARGET_PG_VERSION" \
    -t "pg-attach-bdd-pg${SOURCE_PG_VERSION}-pg${TARGET_PG_VERSION}:dev" "$root"
  image="pg-attach-bdd-pg${SOURCE_PG_VERSION}-pg${TARGET_PG_VERSION}:dev"
  matrix="$SOURCE_PG_VERSION-$TARGET_PG_VERSION"
else
  docker build --build-arg PG_VERSION="$PG_VERSION" \
    -t "pg-attach-pg${PG_VERSION}:dev" "$root"
  docker build -f "$root/tests/bdd/Dockerfile" \
    --build-arg PG_VERSION="$PG_VERSION" \
    -t "pg-attach-bdd-pg${PG_VERSION}:dev" "$root"
  image="pg-attach-bdd-pg${PG_VERSION}:dev"
  matrix="$PG_VERSION"
fi

log_dir="$root/target/bdd-logs/$matrix"
mkdir -p "$log_dir"
output=$(mktemp)
trap 'rm -f "$output"' EXIT HUP INT TERM
if docker run --rm -e PG_ATTACH_LOG_DIR=/logs \
  -v "$log_dir:/logs" "$image" >"$output" 2>&1; then
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
