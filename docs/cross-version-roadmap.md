# Перенос таблиц между major-версиями PostgreSQL

Статус: research/backlog. Сейчас перенос между major не поддерживается. Этот
документ фиксирует, что нужно расширить в протоколе, и перечисляет функционал,
который при переносе может тихо испортить данные.

## Граница задачи

- Только **вверх**: 16→17, 16→18, 17→18 (`source major ≤ target major`, обе из
  поддерживаемой матрицы). PostgreSQL 19 — после stable release.
- **Без конвертации формата.** Если формат страницы, тупла или типа отличается,
  перенос отклоняется до изменения target storage. Конвертировать не будем.
- Downgrade (18→16 и т.п.) не поддерживается и отклоняется.

## Почему это вообще возможно

pg_upgrade **не деформирует туплы**: он копирует/линкует файлы как есть и
опирается на стабильность on-disk формата. Если формат типа изменился, он
отказывает (`check_for_data_types_usage` в `src/bin/pg_upgrade/check.c`).
Последняя такая проверка — `aclitem`, `threshold_version = 1500`; для старого
кластера 16/17 pg_upgrade 18 не содержит ни одной проверки формата типов.

Совпадает в 16, 17, 18:

- page layout version 4 (`PG_PAGE_LAYOUT_VERSION`, с 8.3);
- `HeapTupleHeader`, `t_infomask`/`t_infomask2`, frozen-представление (с 9.4);
- TOAST pointer `varatt_external` и метод сжатия в `va_extinfo` (с PG14);
- btree `BTREE_VERSION = 4` (с PG13), читаются версии 2–4.

Две вещи, которые pg_upgrade решает сам, pg_attach обходит иначе:

| Проблема | pg_upgrade | pg_attach |
| --- | --- | --- |
| XID, `pg_xact`, multixact | переносит clog, выставляет next XID | требует frozen tuples и invalid `xmax`: страница не ссылается на XID источника |
| OID внутри данных | сохраняет OID (`--binary-upgrade`) | переписывает `va_toastrelid`; остальные встроенные OID — только отказ (см. ниже) |

## Текущее состояние: same-major handshake реализован

Сразу после подключения target вызывает SQL-функцию `pg_attach_server_info()` на
source. Проверка выполняется **до** `VACUUM (FREEZE)`, locks, `CHECKPOINT`, чтения
схемы и изменений target. Отсутствие функции, malformed response или несовпадение
параметров закрывает соединение без fallback.

Правила на target:

| Условие | Решение |
| --- | --- |
| source major = target major | разрешено, включая разные minor |
| source major ≠ target major | отказ |
| `extension_version` различается | отказ |
| `BLCKSZ`, `MAXALIGN`, endianness, encoding, checksum mode различаются | отказ |

`extension_version` — единственная версия совместимости pg_attach, включая его
wire/schema-форматы. Независимых версий протокола, manifest и schema descriptor
нет. Manifest не дублирует server version и ABI-параметры, потому что они уже
проверены handshake. Любое изменение wire/schema/SQL API, влияющее на
совместимость, требует изменения версии расширения перед публикацией.

## Дальнейшее расширение протокола

### 1. Дополнительные handshake-параметры

Для будущего cross-major handshake нужно добавить:

- `max_toast_chunk_size`, `max_identifier_length`, версию checksum-алгоритма
  (в PG18 есть ещё `default_char_signedness` — нужен только для будущих GIN);
- методы TOAST-сжатия, поддерживаемые сборкой (`pglz`, `lz4`).

Это дополнительные physical prerequisites, а не независимые версии протокола.

### 2. Изменения manifest

Менять manifest нужно только вместе с версией расширения. Добавлять отдельное
`VERSION`-поле или negotiation между форматами не требуется.

Предстоит добавить:

- **Источник:** отдельно `source_checksums` и `target_checksums`, если переход
  checksum off → on будет разрешён (сейчас это одно поле `checksums`).
- **Атрибуты** (`wire::Attribute`): `attgenerated`, `atthasmissing` и
  текстовое `attmissingval`, `attalign`, `attbyval`, `attstorage`. Плюс
  отпечаток типа вместо голого `type_oid` (см. «OID внутри данных»).
- **Коллации:** имя, провайдер, `collversion` /
  `pg_collation_actual_version()` для каждой коллации, участвующей в индексах.
- **Индексы:** структурное описание вместо текста `pg_get_indexdef`:
  `indkey`, opclass/opfamily по имени, `indcollation`, `indoption`,
  `indisunique`, `indnullsnotdistinct`, наличие выражений/предиката. Текст
  ruleutils отличается между версиями и даст ложные отказы.
- **Btree metapage:** `btm_version`, `btm_allequalimage` каждого индекса.

### 3. SQL API

Для cross-major сигнатура `pg_attach_export_stream` может измениться (target
передаёт свою major и checksum-режим). Нужен update script расширения. Обе
стороны — один и тот же релиз pg_attach, собранный под свою major; handshake
проверяет `extversion`.

### 4. Инварианты страниц и туплов

На source перед модификацией страницы:

- при включённых source checksums проверять сохранённую checksum **до**
  переписывания LSN/TOAST pointer/checksum (сейчас её никто не проверяет, см.
  «Отмывание порчи»);
- `PageGetPageLayoutVersion == 4`, `PageGetPageSize == BLCKSZ`;
- для heap: `HeapTupleHeaderGetNatts == natts` либо доказанная эквивалентность
  missing values (см. `atthasmissing`).

На target после приёма:

- те же проверки layout version и page size;
- btree metapage: `btm_magic`, `btm_version` в диапазоне
  `[BTREE_MIN_VERSION, BTREE_VERSION]` target;
- `_bt_allequalimage()` на target совпадает с `btm_allequalimage` из source.

## Что может сломать данные

Колонка «Как проявится»: **тихо** — запрос вернёт неверные данные без ошибки;
**ошибка** — данные после commit нечитаемы. Отказ до изменения target storage
порчей не считается.

### Уже присутствует в same-major (исправить до cross-version)

Ни один из этих случаев не покрыт BDD; каждый нужно сначала подтвердить
сценарием.

| Случай | Механизм | Как проявится | Мера |
| --- | --- | --- | --- |
| `ALTER TABLE … ADD COLUMN … DEFAULT` (fast default, `atthasmissing`) | старые туплы короче tupdesc, значение берётся из `attmissingval` target. Созданный через `CREATE TABLE … DEFAULT` target не имеет missing value → NULL вместо default | тихо | проверять `t_natts` в `pg_attach_prepare_heap_page`; передавать и сравнивать `atthasmissing`/`attmissingval`; иначе отказ |
| Virtual generated column (PG18) | на диске virtual-колонка хранится как NULL; `create_target` всегда пишет `STORED`; `Attribute` не сравнивает `attgenerated` | тихо (NULL в STORED-колонке) | сравнивать `attgenerated`, генерировать `VIRTUAL`/`STORED` по виду |
| Отмывание порчи | source-страница читается `smgrread` без проверки checksum, затем exporter пересчитывает checksum. Испорченная страница приходит на target с валидной checksum | тихо | проверять checksum на source до модификации |
| Совпавший OID пользовательского типа | сравнивается только `type_oid` колонки. У кластеров с одинаковой историей DDL OID совпадают (оба начинают с 16384), а OID меток `pg_enum` могут разойтись | тихо (enum показывает чужую метку) | отказ для enum и других типов с OID внутри значения |
| Версия glibc/ICU | порядок btree по `text` задаётся библиотекой хоста, а не PG. При upgrade часто меняется и ОС | тихо (поиск/unique по индексу врёт) | сравнивать `collversion`; при различии — отказ или перестройка индекса |

### Появляется или усиливается при cross-version

| Функционал | Механизм | Как проявится | Мера |
| --- | --- | --- | --- |
| Снятие проверки одинаковых checksums (16 off → 18 on: `initdb` в PG18 включает их по умолчанию) | без source checksums порчу нельзя обнаружить, а target получит свежую валидную checksum | тихо | разрешать только с явной оговоркой; при source on — обязательная проверка до модификации |
| Системные composite-типы (`pg_class` и др.) и типы `information_schema` в колонках | OID может совпадать (83 у `pg_class`), но набор колонок каталога меняется между версиями; OID типов `information_schema` выдаются при `initdb` | тихо (мусор в composite) | отказ, как в pg_upgrade (проверка «system-defined composite types in user tables» в `check.c`) |
| OID внутри значения | enum (OID метки), `reg*`, `oid`, `oidvector`, `aclitem` (OID ролей), arrays (`elemtype`), composite (`datum_typeid`), range/multirange (`rangetypid`) | тихо | рекурсивный обход типа (domain → base, array → elem, composite → атрибуты, range → subtype). Разрешать только фиксированные OID из `.dat` (`< FirstGenbkiObjectId`) без каталожных ссылок в значении |
| Маппинг типов/коллаций по имени вместо OID (понадобится для пользовательских типов) | формат extension-типов (PostGIS, hstore, vector) определяет версия расширения, а не PG; при major upgrade её обычно тоже обновляют | тихо | отказ по умолчанию; поддерживать только при совпадении версии расширения и явном whitelist |
| Expression-индексы (сейчас не отклоняются: `is_btree` проверяет только предикат, и то через лениво загружаемый `rd_indpred`) | результат функции может отличаться между версиями (Unicode-таблицы, `normalize()`, багфиксы) → индекс не соответствует heap | тихо | отклонять expression-индексы при cross-version или перестраивать их на target |
| Btree deduplication (`btm_allequalimage`) | флаг вычислен на source; если target считает opclass/коллацию не equalimage, дедупликация небезопасна | тихо | сверять `_bt_allequalimage()` на target |
| TOAST/inline сжатие `lz4` | target собран без lz4 | ошибка при чтении | метод сжатия в handshake |
| Описание схемы, сгенерированное `ruleutils` старого сервера | DDL выполняется на новом; в основном это ошибки `CREATE TABLE` (безопасный rollback), но семантика может разойтись: NOT NULL в `pg_constraint` (PG18), virtual по умолчанию для `GENERATED` (PG18) | ошибка или тихо | генерировать DDL на стороне target по структурным данным каталога (как `pg_dump`: новый клиент читает старый каталог) |
| Downgrade | PostgreSQL не обещает, что старая версия прочитает данные новой | тихо или ошибка | отказ |
| Конвертация туплов «на лету» | при изменении длины тупла меняются TID, все btree становятся невалидными; плюс в новом бинаре нужен код старого формата | тихо (неверные TID) | не делать; отказ, как у pg_upgrade |

### Опциональный функционал с отдельным риском

- **Перенос VM/FSM — реализован.** Форки читаются после freeze-валидации и
  `CHECKPOINT` под тем же `ACCESS EXCLUSIVE`, так что биты VM соответствуют
  переданным heap-страницам (`PD_ALL_VISIBLE` согласован). В WAL-режиме
  страницы VM/FSM логируются тем же `log_relation` путём, что и heap.
- **Перенос статистики.** `pg_statistic.stavalues*` — `anyarray` с OID типа и
  version-specific форматом. Безопаснее `ANALYZE` на target, как сейчас.
- **Другие index AM.** GIN с `pg_trgm` зависит от `default_char_signedness` (PG18
  `pg_upgrade --set-char-signedness`); hash/GiST/SP-GiST/BRIN — см.
  `docs/roadmap.md`.

## Этапы

0. **Исправить оставшиеся same-major риски:** `atthasmissing`, virtual generated,
   проверка checksum на source, отказ для enum/OID-типов, сверка `collversion`.
   Явный отказ при разных major уже реализован.
1. **Дополнить handshake** недостающими physical prerequisites и расширить
   manifest без введения независимой версии формата.
2. **Инварианты страниц и туплов** на source и target.
3. **Безопасность типов:** рекурсивный whitelist и отказ для системных
   composite/`information_schema`.
4. **Структурное описание индексов и схемы**, DDL на стороне target.
5. **Включить 16→17, 17→18, 16→18.** Отдельно — переход checksums off → on.
6. **Известные несовместимости по парам версий:** перед включением каждой пары
   разобрать раздел «Migration to Version N» в release notes (пункты про
   `REINDEX`, изменения типов и функций) и закрепить вывод проверкой или BDD.

## Definition of Done

- [x] Несовместимые major и текущие physical prerequisites отклоняются **до**
      `VACUUM (FREEZE)`/lock/checkpoint с понятным сообщением.
- [ ] Дополнительные prerequisites (TOAST chunk, `NAMEDATALEN`, сжатие)
      проверяются до побочных эффектов.
- [x] Downgrade и major вне матрицы отклоняются.
- [ ] BDD-образ с двумя разными major-версиями; сценарии 16→17, 16→18, 17→18:
      heap, external TOAST, NULL, dropped columns, btree lookup и
      index-only scan, auto-create target.
- [ ] Отрицательные сценарии: enum, `reg*`, системный composite, fast default,
      virtual generated (PG18), различие `collversion`, lz4 без поддержки,
      испорченная source-страница при включённых checksums.
- [ ] После переноса `amcheck` (`bt_index_check` с `heapallindexed`) и
      `verify_heapam` на target проходят без ошибок.
- [ ] Checksums on/off и `generate_wal` on/off на каждой паре.
- [ ] `cargo fmt --check`, строгий Clippy, unit tests и Docker BDD зелёные для
      всех поддерживаемых major.
