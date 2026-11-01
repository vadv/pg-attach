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

## Текущее состояние: версия не проверяется

README требует одинаковую major, но код этого не обеспечивает:

- manifest v1 (`src/pg_attach/wire.rs`) не содержит ни версии сервера, ни
  catversion, ни `MAXALIGN`/порядка байт. Экспортер, собранный под 16, и
  импортер под 18 уже сейчас договорятся по протоколу;
- `BLCKSZ` фактически зафиксирован: `encode_page` и `StreamReader` принимают
  только 8192 байта, но ошибка возникает уже в потоке, а не при handshake;
- совместимость проверяется только после `VACUUM (FREEZE)`, `ACCESS EXCLUSIVE`
  и `CHECKPOINT` на source — то есть после дорогой и блокирующей подготовки.

Первый шаг — сделать ограничение same-major явным, и только потом его ослаблять.

## Расширение протокола

### 1. Handshake до freeze

Новая source-функция (например, `pg_attach_source_info()`) отдаёт компактный
бинарный дескриптор по тому же COPY-транспорту. Importer вызывает её **до**
`prepare_direct` и отклоняет несовместимый source, не трогая его lock/checkpoint.

Состав:

- версия расширения и поддерживаемые версии wire/schema;
- `server_version_num`, `catversion` (для диагностики и сообщений);
- поля `pg_control_init()`: `database_block_size`, `max_data_alignment`,
  `max_toast_chunk_size`, `max_identifier_length`, `data_page_checksum_version`
  (в PG18 есть ещё `default_char_signedness` — нужен только для будущих GIN);
- порядок байт сборки (`cfg!(target_endian)`), т.к. `pg_control_init` его не
  отдаёт;
- методы TOAST-сжатия, поддерживаемые сборкой (`pglz`, `lz4`).

Правила на target:

| Условие | Решение |
| --- | --- |
| source major = target major | как сейчас |
| source major < target major, обе в матрице | разрешено при выполнении проверок ниже |
| source major > target major | отказ |
| major вне матрицы | отказ |
| `BLCKSZ`, `MAXALIGN`, endianness, TOAST chunk size, `NAMEDATALEN` различаются | отказ |
| версии wire/расширения несовместимы | отказ |
| source поддерживает метод сжатия, которого нет на target | отказ |

### 2. Manifest v2

`VERSION = 2`; v1 не принимать (в v1 нет версии source, решить безопасно нельзя;
PoC не обещает совместимость протокола).

Добавить:

- **Источник:** `server_version_num`, отдельно `source_checksums` и
  `target_checksums` (сейчас это одно поле `checksums`).
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

Сигнатура `pg_attach_export_stream` меняется (target передаёт свою major и
checksum-режим). Нужен update script расширения. Обе стороны — один и тот же
релиз pg_attach, собранный под свою major; handshake проверяет `extversion`.

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

0. **Исправить same-major риски:** `atthasmissing`, virtual generated, проверка
   checksum на source, отказ для enum/OID-типов, сверка `collversion`. Плюс
   явный отказ при разных major (чтобы README стал правдой).
1. **Handshake и manifest v2** с правилом «только same major», но уже с полной
   информацией о source.
2. **Инварианты страниц и туплов** на source и target.
3. **Безопасность типов:** рекурсивный whitelist и отказ для системных
   composite/`information_schema`.
4. **Структурное описание индексов и схемы**, DDL на стороне target.
5. **Включить 16→17, 17→18, 16→18.** Отдельно — переход checksums off → on.
6. **Известные несовместимости по парам версий:** перед включением каждой пары
   разобрать раздел «Migration to Version N» в release notes (пункты про
   `REINDEX`, изменения типов и функций) и закрепить вывод проверкой или BDD.

## Definition of Done

- [ ] Несовместимый source отклоняется **до** `VACUUM (FREEZE)`/lock/checkpoint
      с понятным сообщением (major, `BLCKSZ`, `MAXALIGN`, endianness, сжатие).
- [ ] Downgrade и major вне матрицы отклоняются.
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
