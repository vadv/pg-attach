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

1. **`pg_stat_io`** (основной, PG16–18): `reads/read_bytes`,
   `writes/write_bytes`, `extends/extend_bytes` по `backend_type` и `context`
   (`normal`/`bulkread`/`bulkwrite`). Считает именно I/O PostgreSQL, включая
   direct-чтения pg_attach через smgr. Snapshot до/после + `pg_stat_reset()`.
   Ограничение: не видит I/O мимо SMGR — проверить на runtime, что smgr-чтения
   pg_attach попадают в счётчик.
2. **WAL-байты**: `pg_current_wal_lsn()` до/после метода. Точно и дёшево.
   Разница no-WAL против WAL-режима — ключевая демонстрация.
3. **CPU**: cgroup v2 `/sys/fs/cgroup/cpu.stat` (`usage_usec`,
   `user_usec`/`system_usec`) внутри контейнера — покрывает все процессы
   инстанса. Дельта до/после метода. Запасной вариант: `docker stats
   --no-stream` (менее точный), `/proc/<pid>/stat` по дереву postmaster.
4. **Wall time**: `clock_timestamp()` обёрткой вокруг метода.
5. **Дополнительно** (по желанию): `track_io_timing=on` для времени ожидания
   I/O; `EXPLAIN (ANALYZE, BUFFERS)` на фазе `CREATE INDEX` COPY-пути, чтобы
   показать CPU-стоимость построения индекса отдельно.

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
