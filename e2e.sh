#!/bin/sh
set -eu

PG_VERSION=${PG_VERSION:-17}
if [ "$PG_VERSION" = 19 ]; then
    PG_IMAGE_TAG=${PG_IMAGE_TAG:-19beta1-bookworm}
else
    PG_IMAGE_TAG=${PG_IMAGE_TAG:-${PG_VERSION}-bookworm}
fi
export PG_VERSION PG_IMAGE_TAG

compose="docker compose"
trap '$compose down -v >/dev/null 2>&1 || true' EXIT
$compose down -v >/dev/null 2>&1 || true
$compose up -d --build source target

for service in source target; do
    i=0
    until $compose exec -T "$service" psql -U postgres -d postgres -v ON_ERROR_STOP=1 \
        -c 'CREATE EXTENSION IF NOT EXISTS pg_attach;' >/dev/null 2>&1; do
        i=$((i + 1))
        if [ "$i" -gt 60 ]; then
            echo "$service did not become ready" >&2
            exit 1
        fi
        sleep 1
    done
done

$compose exec -T source psql -U postgres -d postgres -v ON_ERROR_STOP=1 <<'SQL'
DROP TABLE IF EXISTS public.events;
CREATE TABLE public.events (
    id integer PRIMARY KEY,
    payload text NOT NULL,
    note text DEFAULT 'missing' CHECK (note IS NULL OR note IN ('even', 'odd', 'missing'))
);
INSERT INTO public.events
SELECT i, repeat('payload-' || i, 1000), CASE WHEN i % 2 = 0 THEN 'even' END
FROM generate_series(1, 2000) AS i;
VACUUM (FREEZE, ANALYZE) public.events;
SQL

$compose exec -T target psql -U postgres -d postgres -v ON_ERROR_STOP=1 <<'SQL'
DROP TABLE IF EXISTS public.events_copy;
CREATE TABLE public.events_copy (
    id integer PRIMARY KEY,
    payload text NOT NULL,
    note text
);
SELECT pg_attach_import(
    'host=source port=5432 dbname=postgres user=postgres password=postgres',
    'public.events',
    'public.events_copy',
    true
);
VACUUM (FREEZE, ANALYZE) public.events_copy;
SQL

source_stats=$($compose exec -T source psql -U postgres -d postgres -At \
    -c 'SELECT count(*), sum(length(payload)), sum(length(note)) FROM public.events;')
target_stats=$($compose exec -T target psql -U postgres -d postgres -At \
    -c 'SELECT count(*), sum(length(payload)), sum(length(note)) FROM public.events_copy;')
[ "$source_stats" = "$target_stats" ] || {
    echo "content mismatch: source=$source_stats target=$target_stats" >&2
    exit 1
}

lookup=$($compose exec -T target psql -U postgres -d postgres -Atq \
    -c 'SET enable_seqscan = off; SELECT id, length(payload) FROM public.events_copy WHERE id = 42;')
[ "$lookup" = '42|10000' ] || {
    echo "btree lookup mismatch: $lookup" >&2
    exit 1
}

# Existing destination is fully overwritten, even when it is non-empty.
$compose exec -T target psql -U postgres -d postgres -v ON_ERROR_STOP=1 <<'SQL'
INSERT INTO public.events_copy VALUES (0, 'stale', NULL);
SELECT pg_attach_import(
    'host=source port=5432 dbname=postgres user=postgres password=postgres',
    'public.events', 'public.events_copy', true
);
SQL
overwritten=$($compose exec -T target psql -U postgres -d postgres -At \
    -c 'SELECT count(*), count(*) FILTER (WHERE id = 0) FROM public.events_copy;')
[ "$overwritten" = '2000|0' ] || {
    echo "non-empty overwrite mismatch: $overwritten" >&2
    exit 1
}

# Missing destination is created from the source descriptor, with a different name.
$compose exec -T target psql -U postgres -d postgres -v ON_ERROR_STOP=1 <<'SQL'
DROP TABLE IF EXISTS public.archive_events;
SELECT pg_attach_import(
    'host=source port=5432 dbname=postgres user=postgres password=postgres',
    'public.events', 'public.archive_events', true
);
VACUUM (FREEZE, ANALYZE) public.archive_events;
SQL
auto_stats=$($compose exec -T target psql -U postgres -d postgres -At \
    -c 'SELECT count(*), sum(length(payload)), sum(length(note)) FROM public.archive_events;')
[ "$auto_stats" = "$source_stats" ] || {
    echo "auto-created destination mismatch: $auto_stats" >&2
    exit 1
}
auto_lookup=$($compose exec -T target psql -U postgres -d postgres -Atq \
    -c 'SET enable_seqscan = off; SELECT id, length(payload) FROM public.archive_events WHERE id = 42;')
[ "$auto_lookup" = '42|10000' ] || {
    echo "auto-created btree lookup mismatch: $auto_lookup" >&2
    exit 1
}
default_note=$($compose exec -T target psql -U postgres -d postgres -qAt \
    -c "INSERT INTO public.archive_events(id, payload) VALUES (2001, 'x') RETURNING note;")
[ "$default_note" = 'missing' ] || {
    echo "auto-created default/constraint mismatch: $default_note" >&2
    exit 1
}

$compose exec -T target psql -U postgres -d postgres -v ON_ERROR_STOP=1 \
    -c 'DROP TABLE IF EXISTS public.events_bad; CREATE TABLE public.events_bad(id integer PRIMARY KEY, payload bigint, note text);'
if $compose exec -T target psql -U postgres -d postgres -v ON_ERROR_STOP=1 \
    -c "SELECT pg_attach_import('host=source port=5432 dbname=postgres user=postgres password=postgres', 'public.events', 'public.events_bad', true);"; then
    echo 'schema mismatch was unexpectedly accepted' >&2
    exit 1
fi
remaining=$($compose exec -T target psql -U postgres -d postgres -At \
    -c 'SELECT count(*) FROM public.events_bad;')
[ "$remaining" = '0' ] || {
    echo 'failed import did not roll back cleanly' >&2
    exit 1
}

echo "pg_attach e2e passed: $target_stats"
