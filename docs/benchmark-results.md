# pg_attach benchmark results

Primary measurement: GitHub Actions `ubuntu-latest` runner (workflow
`benchmark`, run `41289820691`, commit `e98764d`), Linux kernel cgroup v2
`io.stat`/`cpu.stat` — true blackbox counters. Docker Desktop numbers are
kept below for comparison; note that its 2 GB "saturation" observed earlier
was a `mawk printf %d` artifact of the harness, not the kernel (fixed in
`benchmark/bench-inner.sh`, counters are now parsed in pure bash).

Every measurement is a fresh container (one postmaster for local methods,
two for network/COPY), deterministic dataset, 3 repeats, median reported.
Wall/CPU/WAL are measured inside the method window; a `CHECKPOINT` +
writeback settlement runs after the window so written bytes are counted
fully. Source reads are warm-cache (data was just generated in the same
container), so `rbytes` is near zero for every method — reads are not a
differentiator here.

Metrics: `wall_s` (method duration), `cpu_s` (cgroup `cpu.stat` usage delta,
both instances), `wbytes` (cgroup `io.stat` write bytes), `lsn_bytes` (WAL
delta, both instances). `ANALYZE` is inside the window for every method.

## Datasets

- `events`: `id bigint PK`, `payload text` ~10 KB/row (external TOAST),
  `note text` (every 3rd NULL), secondary btree. m = 100k rows (~1.2 GB
  on disk).
- `metrics`: `id bigint PK`, `ts timestamptz`, `val float8`, `tag text`,
  btree on ts — narrow rows, no TOAST. m = 100k rows.

The attach window includes its own `VACUUM (FREEZE)` preparation
(end-to-end honest); the COPY window includes `CREATE INDEX` x2 + `ANALYZE`.

## events m (TOAST-heavy, ~1.2 GB) — Linux runner, blackbox

| method | wall_s | cpu_s | wbytes | lsn_bytes |
| --- | ---: | ---: | ---: | ---: |
| attach-local-nowal | 11.1 | 6.7 | 3.62 GB | 1.10 GB |
| attach-local-wal | 13.9 | 7.9 | 4.73 GB | 2.19 GB |
| attach-network | 16.4 | 7.9 | 5.81 GB | 2.19 GB |
| copy-binary | 27.7 | 31.4 | 4.53 GB | 1.14 GB |
| copy-text | 29.4 | 33.8 | 4.56 GB | 1.14 GB |
| dump-text | 29.0 | 33.3 | 4.54 GB | 1.14 GB |
| dump-fc (-j4) | 68.7 | 77.1 | 5.46 GB | 1.14 GB |

attach-local-nowal vs the field: **2.5x faster wall than copy-binary
(4.7x CPU), 6.2x faster than dump-fc (11.5x CPU), 1.25–1.5x fewer bytes
written**. The WAL volume is equal to COPY paths (1.1 GB) — that is the
VACUUM FREEZE WAL of the preparation step, not the transfer itself.

## metrics m (narrow, no TOAST) — Linux runner, blackbox

| method | wall_s | cpu_s | wbytes | lsn_bytes |
| --- | ---: | ---: | ---: | ---: |
| attach-local-nowal | 0.14 | 0.20 | 23 MB | 6.3 MB |
| attach-local-wal | 0.15 | 0.21 | 34 MB | 16 MB |
| attach-network | 0.15 | 0.22 | 51 MB | 16 MB |
| copy-binary | 0.17 | 0.30 | 36 MB | 8.2 MB |
| copy-text | 0.22 | 0.36 | 36 MB | 8.2 MB |
| dump-text | 0.27 | 0.43 | 36 MB | 8.3 MB |
| dump-fc (-j4) | 0.41 | 0.56 | 37 MB | 8.3 MB |

On a small narrow table the absolute gaps shrink (overhead dominates),
but attach still wins every column; CPU advantage holds at ~1.5x vs COPY
and ~2.6x vs dump-fc.

## Reference: Docker Desktop local run (same harness, PG17)

Events m wall: attach-local-nowal 3.7 s, attach-local-wal 4.4 s,
attach-network 5.1 s, copy-binary 13.0 s, copy-text 14.0 s, dump-text
14.1 s, dump-fc 38.3 s. Ratios match the Linux runner (2.5–3.8x vs COPY,
10x vs dump-fc); absolute times differ by hardware.

## Running on demand

The workflow is `workflow_dispatch` only (after this branch merges to
main): `gh workflow run benchmark`. Locally: `SIZES="m" REPEATS=3
PG_VERSION=17 ./benchmark/run.sh`. Raw CSV of the reference run:
`target/benchmark-ci-final.csv` (also attached to run `41289820691`).

## Known limits

- `rbytes` is ~0 everywhere: the dataset is generated in the same
  container right before the method, so source reads are page-cache hits.
- `dump-fc -j4` cannot parallelize a single-table restore; its cost is
  dominated by custom-format compression.
- Sizes beyond m (l = ~10 GB) were run locally for wall/CPU only; the
  Linux runner disk budget fits m comfortably.
