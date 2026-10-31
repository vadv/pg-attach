# Cucumber BDD

Изолированный набор проверяет `pg_attach` на двух независимых postmaster внутри
одного Docker-контейнера. Каждый сценарий создаёт новые `PGDATA`, порты и сокеты;
данные и DDL задаются plain SQL docstring'ами в feature-файлах.

## Запуск

```sh
make bdd PG_VERSION=16
make bdd PG_VERSION=17
make bdd PG_VERSION=18
```

`make bdd` собирает расширение со strict Clippy/tests, затем отдельный BDD image.
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
- `@toast`, `@index`: специализированные physical checks.

## Структура

- `main.rs` — последовательный fail-fast Cucumber runner;
- `world.rs` — состояние сценария и подстановка портов;
- `postgres.rs` — initdb/pg_ctl, SQL clients, cancellation и логи;
- `steps.rs` — общие Given/When/Then с timeout;
- `features/` — одинаковые feature-файлы для PG16–PG18;
- `flake.nix` — версионированные dev shells.

Postmaster logs сохраняются в `target/bdd-logs/<major>/`. GitHub Actions
загружает их как артефакты после каждого запуска; job имеет общий timeout 20 минут.
