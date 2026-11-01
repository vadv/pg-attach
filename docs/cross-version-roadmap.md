# Перенос таблиц между major-версиями PostgreSQL

Статус: реализовано в pg_attach 0.3.0 для ограниченного physical-format
подмножества. Перенос остаётся копированием страниц, а не логическим upgrade.

## Поддерживаемая матрица

- Одинаковые major 16→16, 17→17, 18→18, включая разные minor.
- Только upgrade вверх: 16→17, 16→18 и 17→18.
- Downgrade, другие направления и major вне 16–18 отклоняются.
- PostgreSQL 19 не добавляется до отдельного анализа stable release.

`extension_version` — единственная версия совместимости pg_attach и его
wire/schema/SQL API. Независимых protocol, manifest или schema-descriptor версий
нет. Обе стороны должны иметь pg_attach 0.3.0, собранный под свою major.

## Ранняя проверка сервера

Сразу после подключения target вызывает `pg_attach_server_info()` на source. До
`VACUUM (FREEZE)`, locks, `CHECKPOINT`, чтения схемы и изменений target
проверяются:

- разрешённая major-пара и равная `extension_version`;
- `BLCKSZ`, `MAXALIGN`, endianness и server encoding;
- одинаковый checksum mode и `PG_DATA_CHECKSUM_VERSION`;
- `TOAST_MAX_CHUNK_SIZE` и `NAMEDATALEN`;
- корректность ответа о поддержке lz4 сборками.

Несовпадение checksum modes не разрешается. В manifest отдельно записаны
`source_checksums` и `target_checksums`; при текущей границе они обязаны
совпадать. Отсутствие функции, NULL, malformed response или лишние/недостающие
поля закрывают соединение без fallback.

Поддержка lz4 не сравнивается как обязательное равенство сборок: таблица без
lz4-значений может переноситься между разными сборками. Фактические inline и
external lz4-значения отклоняются, если target не умеет их декодировать.

## Почему ограниченный перенос возможен

В PG16–PG18 совпадают page layout version 4, `HeapTupleHeader` и frozen
представление, формат TOAST pointer, метод сжатия в `va_extinfo` и btree version
4. Как и pg_upgrade, pg_attach не конвертирует tuples: изменение их длины
сделало бы TID и все btree недействительными.

Протокол сначала выполняет обычный `VACUUM FREEZE → ACCESS EXCLUSIVE →
validate/retry → CHECKPOINT`, затем читает страницы через SMGR. VM и FSM
переносятся вместе с соответствующими relation forks. XID и TOAST relation OID
не переносятся как ссылки на старый кластер: tuples уже frozen, а
`va_toastrelid` переписывается на target TOAST OID без изменения размера tuple.

## Manifest и fail-closed validation

В manifest входят не OID типов и коллаций, а их стабильные имена, версии и
physical attributes. Source и target должны совпасть по:

- имени и порядку атрибутов, typmod, length, alignment, by-value и storage;
- `attgenerated`, `atthasmissing`, NOT NULL и dropped-разметке;
- stable type identity, collation identity, `collversion` и actual collation
  version (включая database-default collation);
- структурным btree key/include/opclass/collation/indoption/unique/
  nulls-not-distinct полям, а не тексту `pg_get_indexdef`;
- btree metapage version, `btm_allequalimage` и результату
  `_bt_allequalimage()` на target.

TOAST index проверяется так же, как пользовательские btree. Expression, partial,
exclusion, invalid и не-btree indexes отклоняются. Для автоматически созданного
target schema descriptor по-прежнему содержит DDL metadata, но physical
совпадение доказывается структурным manifest.

Перед перезаписью LSN/TOAST pointer/checksum source проверяет checksum каждой
main-fork страницы, если checksums включены, page size/layout и границы header.
Heap tuple attribute count не может превышать descriptor; missing values на
коротком tuple отклоняются. Target повторно проверяет page/checksum и btree
metapage до записи. Все проверки catalog/manifest выполняются до смены target
relfilenumbers; ошибки stream откатывают создание или overwrite целиком.

## Поддерживаемые типы и оставшиеся ограничения

Разрешён whitelist встроенных scalar-типов без catalog OID в значении и массивов
этих типов: boolean, integer и floating point, numeric, text/varchar/char/name,
bytea, date/time/timestamp/interval, UUID, JSON/JSONB/JSONPath, XML, money,
network, bit strings, geometric types, `pg_lsn`, `tsvector` и `tsquery`.

Следующие случаи отклоняются, а не конвертируются:

- fast default / `atthasmissing` и любые missing values;
- virtual generated columns (PG18); stored generated columns сохраняют вид;
- enum и массивы enum, `oid`, `oidvector`, `reg*`, `aclitem`, composite,
  system composite, range/multirange, domain и extension-defined types;
- нестандартные physical layouts и типы вне whitelist;
- различия stored/actual collation versions;
- lz4-значения при target без lz4;
- испорченная source-страница или отличающийся checksum mode;
- non-btree, expression, partial и exclusion indexes.

Автоматическое создание target по-прежнему не поддерживает identity columns и
foreign/exclusion constraints; такой target можно создать явно, если physical
manifest совпадает. ACL, ownership, grants, roles и статистика не переносятся.
После импорта следует выполнять `VACUUM (ANALYZE)` на target.

## BDD и Definition of Done

Same-major suite сохранён и дополнен отдельными negative features для fast
defaults, OID-bearing types, collation drift, lz4 и source checksum. Каждый
случай проверяет SQL-ошибку и видимое состояние target, а не скрытый комментарий.

Mixed-major Docker image содержит обе сборки расширения и versioned PostgreSQL
binaries. Запуск:

```sh
SOURCE_PG_VERSION=16 TARGET_PG_VERSION=17 ./tests/bdd/run.sh
SOURCE_PG_VERSION=16 TARGET_PG_VERSION=18 ./tests/bdd/run.sh
SOURCE_PG_VERSION=17 TARGET_PG_VERSION=18 ./tests/bdd/run.sh
```

Для каждой пары проверяются checksum on/off и `generate_wal` on/off: heap,
external TOAST, NULL, dropped columns, btree lookup, index-only scan и
автоматическое создание target. После переноса выполняются
`verify_heapam` и `bt_index_check(index, heapallindexed=true)`.

Negative matrix включает все downgrade-пары, разные checksum modes, enum и
массивы enum, regclass/OID/oidvector/aclitem/system composite, fast defaults,
PG18 virtual generated columns, collation mismatch, unsupported lz4 и
испорченную source-страницу. Отдельно проверяется отказ до VACUUM для
handshake-ошибок и сохранение прежних строк/отсутствие auto-created target для
остальных ошибок.

Обязательная проверка ветки: `cargo fmt --check`, strict Clippy, unit tests,
все same-major PG16–PG18 BDD и все три cross-major BDD pairs. Расширение
whitelist или разрешение нового checksum/type/layout случая требует отдельных
RED→GREEN regressions и не должно маскироваться под tuple conversion.
