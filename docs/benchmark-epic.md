# Benchmark epic: pg_attach против COPY

Статус: план. Эксперимент повторяем, полностью в Docker, PostgreSQL 16–18.

## Вопрос

Насколько pg_attach выгоднее COPY при переносе таблицы между базами/инстансами:
сколько страниц читает и пишет PostgreSQL, сколько WAL генерирует, сколько CPU
тратит (включая построение индексов), и сколько это занимает wall time.

## Методы переноса (сценарии)

| Код | Метод | Что включает |
| --- | --- | --- |
| `attach-local-nowal` | `pg_attach_import_local(..., false)` | авто-VACUUM FREEZE, lock, checkpoint, перенос готовых btree, без WAL |
| `attach-local-wal` | `pg_attach_import_local(..., true)` | то же + WAL-запись страниц |
| `attach-network` | `pg_attach_import` через libpq | два инстанса, поток COPY binary по сети |
| `copy-text` | `\copy` text + `CREATE INDEX` | классический путь, индексы строятся с нуля |
| `copy-binary` | `COPY ... FORMAT binary` + `CREATE INDEX` | то же, binary протокол |

Общий для честности: датасет с TOAST, PK + вторичный btree, `ANALYZE` после
каждого метода (входит в замер).

## Варианты замера

### Основной уровень — blackbox контейнера (симметричен для всех методов)

1. **Блочный I/O**: cgroup v2 `/sys/fs/cgroup/io.stat` (`rbytes`/`wbytes`/
   `rios`/`wios`) — всё, что контейнер реально прочитал/записал на диск;
   не зависит от пути PostgreSQL (bufmgr или прямые smgr). Дельта
   до/после метода. Проверено в Docker Desktop.
2. **CPU**: cgroup v2 `/sys/fs/cgroup/cpu.stat` (`usage_usec`,
   `user_usec`/`system_usec`) — все процессы инстанса. Дельта до/после.
3. **Wall time**: `clock_timestamp()` обёрткой вокруг метода.
4. **WAL-байты**: `pg_current_wal_lsn()` до/после метода (показывает
   no-WAL режим ≈ 0).

Протокол blackbox: каждый метод в свежем контейнере (cold page cache —
честно и повторяемо); финальный snapshot после `CHECKPOINT` + `sync` +
паузы на writeback. Warm-режим — повтор в том же контейнере.

Ограничения blackbox (зафиксированы):
- тёплый page cache прячет чтения из `rbytes` — поэтому cold-режим основной;
- тома с macOS-хоста (virtiofs) уходят мимо cgroup — данные держим внутри
  FS контейнера (наш образ так и делает);
- асинхронный writeback — финальный snapshot после `sync` + паузы.

### Разбивочный уровень — white-box (откуда именно байты)

5. **`pg_stat_io`** (для COPY-пути, PG16–18): `reads`,
   `writes`, `extends` по `backend_type` и `context`
   (`normal`/`bulkread`/`bulkwrite`). Байты: в PG18 есть прямые колонки
   `read_bytes`/`write_bytes`/`extend_bytes`; в PG16–17 — `count ×
   op_bytes`. Snapshot до/после + `pg_stat_reset()`.
   ⚠ Проверено по исходникам: счётчики живут в bufmgr, поэтому прямые
   `smgrread`/`smgrwrite` pg_attach в `pg_stat_io` не попадают.
6. **Манифест pg_attach**: страницы heap+TOAST+indexes × 8K —
   детерминированный источник для pg_attach-методов (логируется
   расширением).
7. **Дополнительно** (по желанию): `track_io_timing=on` для времени ожидания
   I/O; `EXPLAIN (ANALYZE, BUFFERS)` на фазе `CREATE INDEX` COPY-пути, чтобы
   показать CPU-стоимость построения индекса отдельно.

## Источники метрик по методам

| Метрика | Источник (все методы одинаково) |
| --- | --- |
| Чтения/записи диска | cgroup `io.stat` rbytes/wbytes (blackbox) |
| Логические страницы | COPY: `pg_stat_io`; pg_attach: манифест × 8K (разбивка) |
| WAL | LSN до/после |
| CPU | cgroup `cpu.stat` |
| Wall | `clock_timestamp()` |

## Датасет

- Детерминированный (фиксированный seed), никаких `random()` без seed.
- Три размера heap: ~100 MB, ~1 GB, ~5 GB.
- Схема: `id bigint PRIMARY KEY`, `payload text` (TOAST, ~10 KB на строку),
  `note text` + secondary btree по `(note)`.
- Генерация один раз на source; замеряется только перенос.

## Протокол прогона

1. Отдельная пара инстансов на метод (или полный сброс между методами:
   drop + recreate, `pg_stat_reset()`, `CHECKPOINT`).
2. Warm-cache сравнение: одинаковые условия для всех методов. Cold-вариант —
   рестарт контейнера перед методом (drop caches внутри контейнера недоступен).
3. 3 повтора на метод; медиана + min/max.
4. Отдельно замеряется стоимость авто-VACUUM FREEZE (входит в метод
   pg_attach, но показывается своей строкой).
5. Результат: markdown-отчёт + CSV в `docs/benchmarks/`.

## Критерии готовности

- [ ] `benchmark/run.sh` воспроизводит прогон одной командой в Docker.
- [ ] Отчёт: таблица метод × {read_bytes, write_bytes, extends, WAL bytes,
      CPU sec (в т.ч. CREATE INDEX), wall sec}.
- [ ] Проверено на PG17; матрица PG16/18 — по требованию.
- [ ] Вызывающий аргумент pg_attach (например, «в N раз меньше CPU на
      индексы») подтверждён числом из отчёта.
