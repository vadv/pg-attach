# Cucumber BDD

Изолированный набор проверяет `pg_attach` на двух независимых postmaster внутри
одного Docker-контейнера. Каждый сценарий создаёт новые `PGDATA`, порты и сокеты;
данные и DDL задаются plain SQL docstring'ами в feature-файлах.

## Запуск

```sh
make bdd PG_VERSION=16
make bdd PG_VERSION=17
make bdd PG_VERSION=18
SOURCE_PG_VERSION=16 TARGET_PG_VERSION=17 ./tests/bdd/run.sh
SOURCE_PG_VERSION=16 TARGET_PG_VERSION=18 ./tests/bdd/run.sh
SOURCE_PG_VERSION=17 TARGET_PG_VERSION=18 ./tests/bdd/run.sh
```

`make bdd` собирает расширение со strict Clippy/tests, затем отдельный BDD image.
Cross-major runner собирает все три варианта расширения и `Dockerfile.cross`,
чтобы positive и negative scenarios имели все нужные major. Каждый postmaster
использует `/usr/lib/postgresql/<major>/bin` и свою сборку `pg_attach`.
`PG_ATTACH_FEATURES` внутри образа можно заменить для запуска выбранного каталога
или feature-файла. Основной crate не зависит от Cucumber.

Для нативной разработки доступны shells:

```sh
nix develop ./tests/bdd#pg16
nix develop ./tests/bdd#pg17
nix develop ./tests/bdd#pg18
```

Полный изолированный BDD flow всё равно выполняется через Docker.

## Покрытие и теги

- `@core`, `@smoke`: heap, TOAST, NULL, btree, empty/dropped columns, overwrite;
- `@destination`: auto-create, constraints/defaults, несколько индексов;
- `@validation`: schema/frozen/relation/index errors, rollback и retry;
- `@wal`: WAL/no-WAL;
- `@slow`: checksums, disconnect/cancellation и timeout;
- `@toast`, `@index`: специализированные physical checks;
- `@visibility_map`: нулевые validation data reads, heap/TOAST без VM, DML,
  tuple locks, несколько VM-страниц, checksums/WAL и dead/aborted versions;
- `@freeze_retry`: детерминированный committed writer через dblink между vacuum
  и lock; точное число retries, исчерпание лимита, rollback и повторный импорт;
- `@legacy_frozen`: отказ для raw frozen xmin без современных hint bits,
  с VM и без неё, overwrite/auto-create rollback;
- `@stream_cleanup`: ранний LIMIT, executor error после header, отмена между
  SRF-вызовами; late-page legacy error откатывает overwrite/auto-create при WAL
  on/off. Проверяется отсутствие SIGABRT/recovery, не только текст SQL-ошибки.
- `@safety-*`: независимо запускаемые отказы для fast defaults, OID-bearing
  types, collation drift, lz4 и испорченной source-страницы;
- `@cross-major`: все пары 16→17, 16→18, 17→18 с checksums/WAL on/off,
  `verify_heapam`, `bt_index_check(..., heapallindexed=true)` и явные negative
  scenarios с проверкой сохранившихся строк и счётчиков VACUUM.

## Структура

- `main.rs` — последовательный fail-fast Cucumber runner;
- `world.rs` — состояние сценария и подстановка портов;
- `postgres.rs` — versioned initdb/pg_ctl, SQL clients, cancellation и логи;
- `steps.rs` — общие Given/When/Then с timeout и независимыми major/checksum;
- `features/` — одинаковые feature-файлы для PG16–PG18;
- `cross-features/` — сценарии для mixed-major образа;
- `fixtures.c` — только BDD: legacy tuple header и прямое повреждение страницы
  на диске. Собирается отдельным Docker stage и **не входит** в production API;
- `flake.nix` — версионированные dev shells.

Postmaster logs сохраняются в `target/bdd-logs/<major>/` или
`target/bdd-logs/<source>-<target>/`. GitHub Actions загружает их после каждого
запуска; same-major job имеет timeout 20 минут, cross-major — 40 минут.
