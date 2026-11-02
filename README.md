# pg_attach

Transfer a PostgreSQL table to another instance as ready-made data pages —
heap, external TOAST, visibility maps and existing btree indexes — in a single
SQL call. No row decoding, no `COPY`, no index rebuild: on a ~1.2 GB
TOAST-heavy table the transfer is **2.5× faster** than `COPY ... FORMAT binary`
with **4.7× less CPU**, and **6.2× faster** than `pg_dump -Fc`
([benchmark results](docs/benchmark-results.md)).

Supported: PostgreSQL 16, 17 and 18; same-major transfers and upward upgrades
16→17, 16→18, 17→18. Downgrades and PostgreSQL 19 are rejected.

## What you can do

- **Clone a table to another instance** — the target pulls a page stream from
  the source over libpq.
- **Clone a table inside the same database** — same protocol, no network hop.
- **Move a table during a major upgrade** — transfer 16→17/16→18/17→18 as
  pages, keeping frozen tuples and warm visibility maps, instead of a full
  dump/reload.
- The target table may already exist (identical physical layout; fully
  overwritten, may be non-empty) or be created automatically from the source
  schema descriptor, including columns, defaults, CHECK constraints and btree
  indexes.
- Both sides need the same pg_attach version installed. No background worker,
  no `shared_preload_libraries`.

## SQL API

Install on both databases:

```sql
CREATE EXTENSION pg_attach;
```

### Transfer from another instance

Run on the **target** database:

```sql
SELECT pg_attach_import(
  'host=source dbname=postgres user=postgres password=...',
  'public.events',          -- source table
  'public.events_copy',     -- target table (may differ, may not exist yet)
  true                      -- generate WAL
);
VACUUM (ANALYZE) public.events_copy;
```

Parameters:

- connection string — libpq DSN of the source;
- `generate_wal = true` — normal durable import with full-page WAL for heap,
  TOAST and indexes; use it whenever physical replicas or PITR must be able
  to reconstruct the table. `false` writes pages directly with `fsync` and no
  page WAL — a local no-replica/no-PITR PoC mode;
- a missing target is created in the same transaction and rolled back with
  the import on any failure.

### Transfer inside one database

```sql
SELECT pg_attach_import_local(
  'public.events',
  'public.events_archive',
  false
);
```

Same freeze/validate/checkpoint protocol, executed entirely in the local
postmaster: no libpq connection is opened.

### Internal functions

`pg_attach_server_info()` (compatibility handshake, called automatically),
`pg_attach_source_metadata`, `pg_attach_source_is_frozen`,
`pg_attach_lock_source`, `pg_attach_export_schema`,
`pg_attach_export_stream` are transfer building blocks; call
`pg_attach_import` / `pg_attach_import_local` instead of using them directly.

## How a transfer runs

1. `VACUUM (FREEZE)` runs on the source in autocommit.
2. The source table is locked `ACCESS EXCLUSIVE` and every heap/TOAST tuple
   is validated as frozen. A writer that raced between vacuum and lock makes
   the transfer roll back and retry (up to 3 times). Visibility-map-aware:
   pages already marked `ALL_FROZEN` are not re-read.
3. `FlushRelationBuffers` plus `CHECKPOINT` (second connection) put the
   frozen pages on disk.
4. Direct SMGR reads stream heap, TOAST, btree, VM and FSM pages through one
   COPY stream to the target.
5. The target installs pages in a single transaction under
   `AccessExclusiveLock`, rewriting TOAST pointers, checksums and LSNs; any
   failure rolls back both creation and import.

Writers to the source table are blocked for the read window (freeze → end of
stream). This is full overwrite, not a merge.

## Requirements and limits

- Matching architecture, `BLCKSZ`, alignment, endianness, encoding, TOAST
  geometry, checksum algorithm and `data_checksums` mode on both sides.
- Plain logged heap tables and ordinary valid non-partial, non-expression,
  non-exclusion btree indexes with `pg_catalog` opclasses only.
- Only whitelisted built-in types without catalog OIDs in their values, and
  arrays of those types. Enum, `reg*`, OID/oidvector, aclitem,
  composite/range/domain and extension-defined types are rejected.
- Fast defaults/missing values and virtual generated columns are rejected.
  Collation identity (name, provider, locale, deterministic flag, encoding,
  stored/actual versions) must match. lz4 values are rejected when the target
  lacks lz4. Automatic creation rejects identity columns and serial/nextval
  defaults; with a pre-created target, owned sequence state (`last_value`,
  `is_called`) is advanced on the target while the source lock is held. A
  pre-created target column whose default calls `nextval()` on a sequence the
  column does not own is rejected — own it (`ALTER SEQUENCE ... OWNED BY`) or
  advance it manually.
- Foreign and exclusion constraints are unsupported in every mode.
- No partitions, foreign tables, unlogged tables, non-btree indexes,
  downgrades or differing checksum modes.
- ACLs, ownership, grants and roles are deliberately **not** transferred.
- The source role must own the table (or hold equivalent VACUUM privileges)
  and may run `CHECKPOINT` (superuser or `pg_checkpoint`).
- After a transfer, run `VACUUM (ANALYZE)` on the target for planner
  statistics; visibility map and free space map arrive warm, so index-only
  scans and free-space reuse start immediately.

The extension version is the only wire/schema compatibility version; any
compatibility-affecting change requires a version bump. At connect time the
target verifies the extension version, allowed major pair, `BLCKSZ`,
alignment, endianness, encoding, checksum mode, TOAST chunk size and
identifier length before touching anything.

## Build and test

For a local build, put the desired PostgreSQL `pg_config` in `PATH` (or set
`PG_CONFIG`) and select its major version:

```sh
make build PG_VERSION=18
make test PG_VERSION=18
make lint PG_VERSION=18
```

The Makefile exports the resolved `pg_config` path to pgrx and rejects a
version mismatch.

The BDD suite requires Docker and starts two isolated postmasters inside one
test container:

```sh
make bdd PG_VERSION=17
SOURCE_PG_VERSION=16 TARGET_PG_VERSION=17 ./tests/bdd/run.sh
```

Scenarios cover heap pages, external TOAST, NULL, btree lookups and
index-only scans, empty and dropped-column layouts, overwrite and automatic
target creation, validation/rollback, cross-major transfers, and explicit
unsafe-type/default/generated/collation/compression/checksum failures. Every
successful pair is checked with `verify_heapam` and
`bt_index_check(..., heapallindexed=true)` across checksum and WAL modes.

For native debugging tools and backtrace commands, see [DEBUG.md](DEBUG.md).

## Implementation notes

`src/page_shim.c` holds the PostgreSQL C boundary: relation-buffer flushing,
frozen-tuple validation, direct SMGR reads with source checksum/layout
verification, TOAST pointer and compression checks, btree metapage
validation, page checksum/LSN handling, SMGR writes and WAL/fsync calls.
`src/pg_attach/wire.rs` is a bounded big-endian packet/schema codec;
`exporter.rs` emits schema and physical metadata plus one COPY bytea value
per packet; `importer.rs` validates the manifest and installs pages
sequentially. No staging files or tuple conversion are used.

- [docs/benchmark-results.md](docs/benchmark-results.md) — measured
  pg_attach vs COPY/pg_dump wall/CPU/write/WAL numbers;
- [docs/roadmap.md](docs/roadmap.md) — compatibility matrix, must-have
  priorities and future index work;
- [docs/pg-attach-internals.md](docs/pg-attach-internals.md) — deep dive;
- [tests/bdd](tests/bdd) — Gherkin suite with two independent postmasters
  in one container.
