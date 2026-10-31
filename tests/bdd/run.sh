#!/bin/sh
set -eu

PG_VERSION=${PG_VERSION:-17}
root=$(CDPATH= cd "$(dirname "$0")/../.." && pwd)

docker build --build-arg PG_VERSION="$PG_VERSION" \
  -t "pg-attach-pg${PG_VERSION}:dev" "$root"
docker build -f "$root/tests/bdd/Dockerfile" \
  --build-arg PG_VERSION="$PG_VERSION" \
  -t "pg-attach-bdd-pg${PG_VERSION}:dev" "$root"
exec docker run --rm "pg-attach-bdd-pg${PG_VERSION}:dev"
