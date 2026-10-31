# pg_attach

PoC PostgreSQL extension that freezes and transfers a table between two PostgreSQL 16–18 instances as heap, TOAST and existing btree pages. It avoids decoding/rebuilding rows and indexes. Both sides must use the same major version. PostgreSQL 19 is out of scope until its stable release.

## Scope

- Same PostgreSQL major, architecture, `BLCKSZ`, encoding and `data_checksums` on both sides.
- Plain logged heap tables and ordinary, non-partial, non-exclusion btree indexes.
- An existing target is fully overwritten and must have identical attributes and btree keys; it may be non-empty and its name may differ.
- If the target is absent, the extension creates its table/columns, supported constraints and btree indexes from a compact binary schema descriptor. The target name may differ from the source name. Automatic creation currently rejects identity columns and foreign/exclusion constraints; create such a target explicitly first.
- ACLs, ownership, grants, roles and other privileges are deliberately out of scope and are not transferred.
- Import runs `VACUUM (FREEZE)` on the source and validates frozen heap/TOAST tuples under lock. No VM copying; run `VACUUM (ANALYZE)` on the target after import.
- No partitions, foreign tables, unlogged tables, non-btree indexes, or cross-version upgrades.

Both instances need the same extension build installed. No background worker or `shared_preload_libraries` entry is needed. The source only serves a `COPY` stream; the target pulls it through libpq. The source role must own the table (or have equivalent VACUUM privileges) and have permission to run `CHECKPOINT` (normally a superuser or a role with `pg_checkpoint`). Heap and TOAST are validated under `ACCESS EXCLUSIVE` before checkpoint using their visibility maps. Only pages without `ALL_FROZEN` need a validation read; all data pages are then read for streaming.

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

The Gherkin scenarios cover heap pages, external TOAST, NULL, btree lookup, empty tables, dropped-column layout, full overwrite, auto-created destination constraints/defaults, repeated import with multiple indexes, incompatible layouts, invalid/missing/offline sources, non-default tablespaces and non-superuser source owners. Docker builds also run Rust unit tests and strict Clippy lints.

For native debugging tools and backtrace commands, see [DEBUG.md](DEBUG.md).

## SQL API

On both databases:

```sql
CREATE EXTENSION pg_attach;
```

Create a compatible target table yourself, or let the extension create a missing target from the source schema, then on the target:

```sql
SELECT pg_attach_import(
  'host=source dbname=postgres user=postgres password=...',
  'public.events',
  'public.events_copy',
  true                         -- generate WAL
);
VACUUM (ANALYZE) public.events_copy;
```

`pg_attach_import` performs `VACUUM (FREEZE)` in source autocommit, then takes `ACCESS EXCLUSIVE` on that source connection and validates heap/TOAST tuples. If a writer changed the table between vacuum and lock, it rolls back and retries up to three times. `FlushRelationBuffers` makes vacuum's dirty heap/TOAST pages visible to the direct validation read without reading them through the buffer manager. Only after validation does a separate source connection run `CHECKPOINT`; the locked connection then performs direct SMGR reads and the COPY stream.

The source export function is internal to the transfer; use `pg_attach_import` rather than calling it standalone.

`VM_ALL_FROZEN` is used on PostgreSQL 16–18 for both heap and TOAST. Validation reads visibility-map metadata through PostgreSQL's buffer manager, reusing one pinned VM buffer per relation; when the bit is set, it does **not** read or scan the corresponding data page. Missing or clear bits fall back to direct page reads and tuple checks. If all bits are set, validation reads zero heap/TOAST pages and the transfer reads each data page only once, during streaming. This excludes VACUUM's own work and does not remove streaming tuple/TOAST rewriting. VM does **not** prove that dirty pages have reached disk, so `FlushRelationBuffers` and `CHECKPOINT` remain. With `log_min_messages = debug1`, per-relation validation logs report VM skips and data-page reads.

Legacy raw frozen xmin from pre-9.4 `pg_upgrade` is intentionally unsupported: streaming rejects it even if VM is marked all-frozen, and the target transaction rolls back.

The target takes `AccessExclusiveLock` and imports in one transaction. Existing target storage is replaced with new relfilenumbers; a missing target is created in the same transaction, so failure rolls back both creation and import. This is full overwrite, not a merge. Existing targets still require identical attributes and btree definitions; unexpected extra indexes are rejected rather than silently dropped.

For a missing target, `pg_attach_export_schema` returns a compact binary descriptor over the same COPY transport. It contains type/default/constraint/index metadata, not a `pg_dump` invocation or PL/pgSQL. Target-side Rust quotes the requested destination name independently of the source name.

## WAL and durability

Use `generate_wal = true` for a normal permanent table, physical replicas, PITR, or any setup where WAL must reconstruct the imported pages. It emits full-page WAL for heap, TOAST and indexes.

`false` uses direct storage writes plus `fsync` and avoids page WAL. It is only a local no-replica/no-PITR PoC mode: WAL-based recovery or physical replicas cannot reconstruct those pages. It does not make an unsupported unsafe import generally safe.

## Implementation notes

`src/page_shim.c` contains PostgreSQL C macro access, relation-buffer flushing, frozen-tuple validation, direct SMGR reads, TOAST pointer rewriting, page checksum/LSN handling, SMGR writes and WAL/fsync calls. `src/pg_attach/wire.rs` is a bounded big-endian packet/schema codec with unit tests. `src/pg_attach/exporter.rs` emits schema metadata and one COPY bytea value per packet; `src/pg_attach/importer.rs` optionally creates the target, validates the manifest and installs pages sequentially. No staging files are created.

See `docs/roadmap.md` for the PG16–PG18 matrix, future index/case DoD and C-boundary plan. The implemented Rust/Cucumber BDD suite lives in `tests/bdd`; its scenarios use plain SQL docstrings and two postmasters in one test container. See `docs/bdd-epic.md` for the original planning epic.
