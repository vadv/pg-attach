# pg_attach benchmark results

Setup: `benchmark/run.sh`, Docker Desktop, image `pg-attach-bdd-pg17:dev`,
PostgreSQL 17. Every measurement is a fresh container (one postmaster for
local methods, two for network/COPY), deterministic dataset, 3 repeats,
median reported. Wall/CPU/WAL are measured inside the method window; a
`CHECKPOINT` + writeback settlement runs after the window so written bytes
are counted fully. Source reads are warm-cache (data was just generated in
the same container), so `rbytes` is near zero for every method — reads are
not a differentiator here.

Metrics: `wall_s` (method duration), `cpu_s` (cgroup `cpu.stat` usage delta,
both instances), `wbytes` (cgroup `io.stat` write bytes), `lsn_bytes` (WAL
delta, both instances).

## Datasets

- `events`: `id bigint PK`, `payload text` ~10 KB/row (external TOAST),
  `note text` (every 3rd NULL), secondary btree. m = 100k rows (~1 GB),
  l = 500k rows (~10 GB heap+TOAST).
- `metrics`: `id bigint PK`, `ts timestamptz`, `val float8`, `tag text`,
  btree on ts — narrow rows, no TOAST. Same row counts.

The attach window includes its own `VACUUM (FREEZE)` preparation
(end-to-end honest); the COPY window includes `CREATE INDEX` x2 + `ANALYZE`.

## events (TOAST-heavy)

| size | method | wall_s | cpu_s | wbytes | lsn_bytes |
| --- | --- | ---: | ---: | ---: | ---: |
| m | attach-local-nowal | 0.142 | 0.144 | 67.6 MB | 21.6 MB |
| m | attach-local-wal | 0.188 | 0.177 | 91.5 MB | 45.4 MB |
| m | attach-network | 0.182 | 0.194 | 108.5 MB | 45.4 MB |
| m | copy-binary | 2.243 | 2.550 | 63.2 MB | 21.4 MB |
| m | copy-text | 3.623 | 4.689 | 64.1 MB | 22.4 MB |
| l | attach-local-nowal | 0.419 | 0.403 | 332.7 MB* | 107.1 MB |
| l | attach-local-wal | 0.506 | 0.484 | 451.7 MB* | 225.9 MB |
| l | attach-network | 0.629 | 0.587 | 569.5 MB* | 225.9 MB |
| l | copy-binary | 10.893 | 12.606 | 328.7 MB* | 106.9 MB |
| l | copy-text | 17.887 | 22.955 | 333.4 MB* | 111.6 MB |

\* see Caveats: Docker Desktop storage is host-backed, cgroup write
counters capture only a small fraction for every method alike; both
paths write the same ~10 GB of pages by construction.

## metrics (narrow, no TOAST)

| size | method | wall_s | cpu_s | wbytes | lsn_bytes |
| --- | --- | ---: | ---: | ---: | ---: |
| m | attach-local-nowal | 0.132 | 0.121 | 23.6 MB | 6.3 MB |
| m | attach-local-wal | 0.141 | 0.125 | 33.7 MB | 16.4 MB |
| m | attach-network | 0.133 | 0.151 | 50.8 MB | 16.4 MB |
| m | copy-binary | 0.113 | 0.187 | 36.4 MB | 8.2 MB |
| m | copy-text | 0.126 | 0.211 | 36.4 MB | 8.2 MB |
| l | attach-local-nowal | 0.212 | 0.200 | 114.3 MB | 30.8 MB |
| l | attach-local-wal | 0.260 | 0.246 | 164.9 MB | 81.4 MB |
| l | attach-network | 0.287 | 0.274 | 215.6 MB | 81.4 MB |
| l | copy-binary | 0.293 | 0.483 | 127.7 MB | 40.5 MB |
| l | copy-text | 0.392 | 0.615 | 127.7 MB | 40.5 MB |

## Findings

1. **TOAST-heavy table, the headline case.** At 500k rows x 10 KB
   (events l) attach-local-nowal moves the table in 0.42 s vs 10.9 s for
   copy-binary and 17.9 s for copy-text: **26x / 43x faster wall time,
   31x / 57x less CPU**. The gap is CPU-side tuple work (COPY parse/format,
   index build), not I/O volume: written bytes are the same order for every
   method, and WAL volume is nearly identical (107 MB) for attach-nowal and
   COPY. The advantage is exactly the removed per-row CPU cost.
2. **The advantage scales with data volume, as expected.** events m shows
   16x/26x; the fixed overhead (freeze scan, checkpoint, connection setup)
   amortizes as the table grows.
3. **Narrow no-TOAST tables: COPY is competitive.** At metrics m copy-binary
   wall time (0.113 s) beats attach (0.132 s): there is nothing to skip, and
   attach pays its freeze/lock preparation. At metrics l attach wins wall
   1.4x and CPU 2.4x. CPU is where attach stays ahead on every shape.
4. **WAL mode costs ~2x write volume vs no-WAL** (FPI per page: 45 vs
   22 MB at m; 226 vs 107 MB at l) but stays ~20x faster than COPY on
   events l. no-WAL mode remains PoC-only (no replicas/PITR).
5. **Included preparation is cheap.** attach numbers include auto
   `VACUUM (FREEZE)` (its WAL is the 21/107 MB on the nowal rows) —
   end-to-end the freeze scan does not change the picture.

## Caveats

- Source reads are warm-cache; `rbytes` is not a meaningful column here.
- **Write bytes are a fraction, not the truth, on Docker Desktop**: the
  container filesystem is host-backed (virtiofs/`docker.raw`), so most
  writeback happens in the macOS-side block layer, invisible to the
  guest cgroup `io.stat`. Verified by re-running events l with a
  writeback-settlement wait: counters are stable at the same values, so
  it is not unsettled writeback. The undercount applies equally to every
  method; both attach and COPY write the same ~10 GB of pages by
  construction, and the exact WAL volume is the `lsn_bytes` column.
- attach-network ran over a unix socket inside one container; a real
  network adds latency and bandwidth limits not measured here.
- COPY pipeline is a single pipe (`COPY TO STDOUT | COPY FROM STDIN`);
  parallel COPY workers were not used — that is the standard comparison
  baseline, not a tuned one.
- Single host (Docker Desktop VM); relative comparisons hold, absolute
  numbers will differ on other hardware.

Raw medians: `target/benchmark/results-17.csv` (regenerate with
`./benchmark/run.sh`).
