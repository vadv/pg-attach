# Epic: Rust Cucumber BDD для pg_attach

Статус: план, не реализовано. Этот документ описывает отдельный эпик и не
изменяет текущий SQL API расширения.

## Цель

Заменить shell-сценарии на проверяемые Rust/Cucumber-сценарии, которые полностью
контролируют два PostgreSQL в **одном тестовом Docker-контейнере**. Внутри
контейнера запускаются два независимых postmaster с разными data directory и
портами: `source` и `target`. Это не два compose-сервиса и не внешний PostgreSQL.

Референс реализации: `~/Projects/pg_doorman_vadv`:

- `tests/bdd/main.rs`, `world.rs` — Cucumber World и запуск сценариев;
- `tests/bdd/postgres_helper.rs` — управление `initdb`/`pg_ctl` и потоковые логи;
- `tests/bdd/features/connection.feature` — feature с docstring-конфигурацией;
- `Cargo.toml` — `cucumber = "0.21"` и отдельный `[[test]]` с `harness = false`.

Берём идею harness и управления процессами, но не переносим Nix, клиентские
тулчейны или GHCR-инфраструктуру без необходимости.

## Основные решения

1. `tests/bdd/main.rs` использует `cucumber = "0.21"`, `harness = false`,
   `fail_fast()` и последовательные сценарии, пока World владеет общей парой
   кластеров.
2. `AttachWorld` владеет `TempDir`, двумя `Child`/PID postmaster, портами,
   соединениями и последним результатом SQL. Drop останавливает оба postmaster
   и удаляет временные каталоги; ошибки остановки не скрывают исходный failure.
3. Docker-образ содержит серверные бинарники выбранной major, extension,
   `initdb`, `pg_ctl`, `psql` и test runner. Один сценарий может пересоздавать
   кластеры или только базу/схему — выбор фиксируется в Given-шаге.
4. Набор данных задаётся **plain SQL**, а не специальными шагами-генераторами.
   Gherkin отвечает за оркестрацию и проверки, SQL — за данные и DDL.
5. Низкоуровневый wire/heap/TOAST тестируется отдельными Rust unit/integration
   tests; Cucumber проверяет пользовательский сценарий и наблюдаемые последствия,
   а не дублирует каждый бинарный тест.

## Feature map для согласования

Это согласованный состав feature-групп. Детальные сценарии review'им в draft PR;
реализацию harness не начинаем до подтверждения этого списка.

### `transfer.feature` — core transfer

- heap, внешний TOAST, NULL и готовый btree переносятся вместе;
- пустая таблица переносится без страниц данных;
- dropped columns сохраняют физический attribute layout;
- повторный import в то же destination проходит как полный overwrite.

### `destination.feature` — destination modes

- существующая непустая destination полностью перезаписывается;
- отсутствующая destination создаётся под другим именем;
- auto-create восстанавливает NOT NULL, defaults, CHECK и обычные btree indexes;
- несколько индексов и повторный import в auto-created destination работают.

### `validation.feature` — validation and rollback

- различие source/target attributes вызывает ошибку и rollback;
- unfrozen source tuple отклоняется;
- unsupported relation и non-btree index отклоняются;
- отсутствующий source не создаёт и не изменяет destination;
- после любой ошибки target остаётся доступным для следующего import.

### `resilience.feature` — `@slow`, после стабилизации первых трёх групп

- import с `generate_wal = true` и в no-WAL PoC mode;
- checksums включены и выключены;
- обрыв source connection во время COPY не оставляет видимых partial data;
- отмена import на target не оставляет видимых partial data;
- зависший source/target прерывается timeout сценария и сохраняет логи.

ACL, ownership, grants и roles во всех feature остаются out of scope и не
переносятся. Матрица одинаковых feature — PostgreSQL 16–18.

## Этапы

### 1. Harness и жизненный цикл

- Добавить `tests/bdd/{main.rs,world.rs,postgres.rs,steps.rs}`.
- Реализовать запуск двух кластеров с изолированными `PGDATA`, Unix socket и
  портами, readiness через реальное подключение, а не только `pg_isready`.
- Собирать stdout/stderr обоих postmaster в артефакты при failure.
- Добавить теги `@smoke`, `@negative`, `@toast`, `@index`, `@wal`, `@slow`.

### 2. Общие SQL-шаги

Минимальный набор шагов:

- `Given the following SQL is executed on "source":` + docstring;
- `When the following SQL is executed on "target":` + docstring;
- `Then the following SQL on "target" returns "..."` + docstring;
- `Then the following SQL on "source" fails with "..."` + docstring.

Шаги не знают про `events`, число строк или TOAST: любые DDL/DML/проверки
находятся в feature. SQL выполняется через один PostgreSQL client crate, с
таймаутом и включённым `ON_ERROR_STOP`-эквивалентом для всего script.

### 3. Перенос текущих проверок и новые случаи

- happy path: строки, NULL, внешний TOAST, btree lookup;
- existing non-empty destination overwrite и automatic creation под другим именем;
- ACL/grants/ownership не переносятся и не проверяются: это явный out of scope;
- пустая таблица, dropped columns, несколько индексов и повторный запуск;
- несовпадение схемы, непустой target, unfrozen tuple — ошибка и rollback;
- включённые/выключенные checksums и WAL-режим;
- обрыв source connection и прерывание target во время импорта;
- матрица PG16–PG18 с одинаковыми feature-файлами.

### 4. CI и удаление shell-дублирования

- `docker build --build-arg PG_VERSION=N` собирает образ и extension;
- `docker run --rm ... cargo test --test bdd` запускает сценарии в том же
  контейнере, где находятся оба postmaster;
- CI публикует логи и имеет timeout на весь сценарий;
- после полного переноса `e2e.sh` удаляется либо остаётся только маленьким smoke
  wrapper без собственного набора проверок.

## Definition of Done

- [ ] Один `docker run` поднимает ровно один тестовый контейнер; внутри него
      Cucumber может независимо запускать и останавливать оба postmaster.
- [ ] Все существующие `e2e.sh`-проверки представлены feature-сценариями.
- [ ] Данные во всех feature заданы plain SQL docstring’ами; нет шагов вида
      `create N rows with toast`.
- [ ] Каждый сценарий изолирован и повторяем; failure оставляет читаемые логи.
- [ ] Проверяются отрицательный результат и rollback, а не только успешный SQL.
- [ ] `cargo test --test bdd` работает локально и в GitHub Actions для PG16–PG18.
- [ ] Каждый сценарий имеет timeout; зависший postmaster не блокирует CI навсегда.
- [ ] `cargo clippy --all-targets -- -D warnings -D clippy::pedantic` зелёный.
- [ ] `tests/bdd/README.md` содержит команды запуска, теги и схему каталогов.
- [ ] Shell-больше не содержит параллельного набора assertions.

## Примеры feature

```gherkin
@smoke @toast
Feature: Physical table transfer
  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text NOT NULL,
        note text
      );
      INSERT INTO public.events
      SELECT i, repeat('payload-' || i, 1000), CASE WHEN i % 2 = 0 THEN 'even' END
      FROM generate_series(1, 2000) AS i;
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """

  Scenario: Heap, TOAST and btree survive import
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=55432 dbname=postgres user=postgres',
        'public.events', 'public.events_copy', true
      );
      VACUUM (FREEZE, ANALYZE) public.events_copy;
      """
    Then the following SQL on "target" returns "2000|22893000|4000":
      """
      SELECT count(*), sum(length(payload)), sum(length(note))
      FROM public.events_copy;
      """
    And the following SQL on "target" returns "42|10000":
      """
      SET enable_seqscan = off;
      SELECT id, length(payload) FROM public.events_copy WHERE id = 42;
      """

  @negative
  Scenario: Incompatible target rolls back
    Given the following SQL is executed on "target":
      """
      CREATE TABLE public.events_bad (
        id integer PRIMARY KEY,
        payload bigint,
        note text
      );
      """
    When the following SQL on "target" fails with "source and target table attributes differ":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=55432 dbname=postgres user=postgres',
        'public.events', 'public.events_bad', true
      );
      """
    Then the following SQL on "target" returns "0":
      """
      SELECT count(*) FROM public.events_bad;
      """
```

Порты в примере условные: реальный World подставляет выделенные порты через
конфигурацию окружения, не зашитый в feature пароль. Для проверки планов индекса
нужен отдельный SQL-шаг, который сравнивает результат `EXPLAIN`/`EXPLAIN ANALYZE`
или надёжно проверяет использование индекса, а не только возвращённую строку.
