# pg_attach

PoC PostgreSQL extension that transfers a frozen table between two PostgreSQL 16–18 instances as heap, TOAST and existing btree pages. It avoids decoding/rebuilding rows and indexes. Both sides must use the same major version. PostgreSQL 19 is out of scope until its stable release.

## Scope

- Same PostgreSQL major, architecture, `BLCKSZ`, encoding and `data_checksums` on both sides.
- Plain logged heap tables and ordinary, non-partial, non-exclusion btree indexes.
- An existing target is fully overwritten and must have identical attributes and btree keys; it may be non-empty and its name may differ.
- If the target is absent, the extension creates its table/columns, supported constraints and btree indexes from a compact binary schema descriptor. The target name may differ from the source name. Automatic creation currently rejects identity columns and foreign/exclusion constraints; create such a target explicitly first.
- ACLs, ownership, grants, roles and other privileges are deliberately out of scope and are not transferred.
- Heap and TOAST tuples must be frozen. No VM copying; run `VACUUM (FREEZE, ANALYZE)` on the target after import.
- No partitions, foreign tables, unlogged tables, non-btree indexes, or cross-version upgrades.

Both instances need the same extension build installed. No background worker or `shared_preload_libraries` entry is needed. The source only serves a `COPY` stream; the target pulls it through libpq.

## Build and test

For a local build, put the desired PostgreSQL `pg_config` in `PATH` (or set `PG_CONFIG`) and select its major version:

```sh
make build PG_VERSION=18
make test PG_VERSION=18
make lint PG_VERSION=18
```

The Makefile exports the resolved `pg_config` path to pgrx and rejects a version mismatch. Supported versions are PostgreSQL 16, 17 and 18.

The BDD suite requires Docker and starts two isolated postmasters inside one test container:

```sh
make bdd PG_VERSION=17
```

The Gherkin scenarios cover heap pages, external TOAST, NULL, btree lookup, empty tables, dropped-column layout, full overwrite, auto-created destination constraints/defaults and repeated import with multiple indexes. Docker builds also run Rust unit tests and strict Clippy lints.

For native debugging tools and backtrace commands, see [DEBUG.md](DEBUG.md).

## SQL API

On both databases:

```sql
CREATE EXTENSION pg_attach;
```

On the source, before transferring:

```sql
VACUUM (FREEZE, ANALYZE) public.events;
```

Create a compatible target table yourself, or let the extension create a missing target from the source schema, then on the target:

```sql
SELECT pg_attach_import(
  'host=source dbname=postgres user=postgres password=...',
  'public.events',
  'public.events_copy',
  true                         -- generate WAL
);
VACUUM (FREEZE, ANALYZE) public.events_copy;
```

The source export function is normally called internally, but can be inspected independently:

```sql
SELECT pg_attach_export_stream(
  'public.events', 'public.events_copy',
  '12345'::oid,                  -- target TOAST OID
  false                          -- target data_checksums
);
```

The source takes `ShareLock` on the table, TOAST relation and indexes for the statement, blocking ordinary writes. The target takes `AccessExclusiveLock` and imports in one transaction. Existing target storage is replaced with new relfilenumbers; a missing target is created in the same transaction, so failure rolls back both creation and import. This is full overwrite, not a merge. Existing targets still require identical attributes and btree definitions; unexpected extra indexes are rejected rather than silently dropped.

For a missing target, `pg_attach_export_schema` returns a compact binary descriptor over the same COPY transport. It contains type/default/constraint/index metadata, not a `pg_dump` invocation or PL/pgSQL. Target-side Rust quotes the requested destination name independently of the source name.

## WAL and durability

Use `generate_wal = true` for a normal permanent table, physical replicas, PITR, or any setup where WAL must reconstruct the imported pages. It emits full-page WAL for heap, TOAST and indexes.

`false` uses direct storage writes plus `fsync` and avoids page WAL. It is only a local no-replica/no-PITR PoC mode: WAL-based recovery or physical replicas cannot reconstruct those pages. It does not make an unsupported unsafe import generally safe.

## Implementation notes

`src/page_shim.c` contains PostgreSQL C macro access, frozen-tuple validation, TOAST pointer rewriting, page checksum/LSN handling, SMGR writes and WAL/fsync calls. `src/pg_attach/wire.rs` is a bounded big-endian packet/schema codec with unit tests. `src/pg_attach/exporter.rs` emits schema metadata and one COPY bytea value per packet; `src/pg_attach/importer.rs` optionally creates the target, validates the manifest and installs pages sequentially. No staging files are created.

See `docs/roadmap.md` for the PG16–PG18 matrix, future index/case DoD and C-boundary plan. The implemented Rust/Cucumber BDD suite lives in `tests/bdd`; its scenarios use plain SQL docstrings and two postmasters in one test container. See `docs/bdd-epic.md` for the original planning epic.
