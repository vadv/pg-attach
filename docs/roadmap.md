# Дальнейшее развитие pg_attach

Статус: research/backlog. Здесь нет обещаний, что перечисленные index AM и
сценарии уже поддерживаются.

## Матрица совместимости

Цель проекта — одинаковая major-версия на source и target из диапазона
PostgreSQL 16, 17, 18 и 19. Это не cross-version physical upgrade: page layout,
`BLCKSZ`, encoding, checksum mode и остальные physical prerequisites должны
совпадать.

- `pg16`, `pg17`, `pg18` — отдельные Cargo features pgrx 0.19.3;
- каждая сборка выбирает ровно одну feature и соответствующий `pg_config`;
- локальный Docker BDD и strict build пройдены на PostgreSQL 16, 17 и 18;
- GitHub Actions запускает build/lint/BDD для каждой поддерживаемой major;
- PostgreSQL 19 не входит в matrix до stable release;
- зелёная компиляция не считается доказательством физической совместимости.

## Индексы и physical cases

Сейчас поддерживаются только обычные валидные non-partial, non-exclusion btree.
Следующие элементы требуют исследования и отдельных реализаций, а не просто
удаления проверки `BTREE_AM_OID`:

1. **Btree variants**: включить included columns, expression indexes, custom
   opclasses/collation, `NULLS NOT DISTINCT`, deduplication и multi-page/metapage
   cases. Проверять, что manifest описывает index definition достаточно точно.
2. **GiST**: исследовать `gistinitpage`, opaque page metadata, `F_FOLLOW_RIGHT`,
   NSN/LSN semantics и recovery. До отдельной реализации не переносить как raw
   pages; logical fallback не считать эквивалентом.
3. **GIN**: отдельно исследовать pending list, entry/posting trees, metapage,
   fastupdate и очистку pending state. Нужны тесты на bulk insert и `VACUUM`.
4. **BRIN**: исследовать revmap, summaries, страницы с диапазонами и зависимость
   от heap block numbers. Physical copy возможен только при неизменной раскладке
   heap и корректной перенастройке метаданных.
5. **Hash**: исследовать bucket pages, bitmap, metapage, split state и WAL.
6. **SP-GiST**: исследовать inner/leaf tuple formats и специальные страницы.
7. **Неструктурные случаи**: partitioned tables, partitioned indexes, foreign
   tables, unlogged relations, TOAST с нестандартными типами, generated/dropped
   columns, composite types, collations и checksums.

Для каждого AM сначала записываются page invariants и rollback/recovery story,
затем реализуется один узкий happy path и только после этого расширяется matrix.

## C boundary: принцип и план

C оставляем только там, где он даёт проверяемую пользу: PostgreSQL macros в
горячем цикле по heap tuple/TOAST, физическая подготовка страницы и локальные
version-specific операции. Не переносим макросы в собственный Rust layout-клон.

Уже вынесено в Rust/pgrx: relation block count, `reltoastrelid`, имена и
relcache-признаки, проверка btree, checksum setting и обычный index list.
В C остаются attributes/index definitions, page/heap tuple macros, TOAST rewrite,
SMGR page I/O и WAL/fsync. Дальнейшие сокращения:

- attributes и index definitions можно перенести после отдельной проверки ABI;
- WAL/fsync и page read/write — только после benchmark и проверки error boundary,
  поскольку это физический per-page path.

`pg_attach_prepare_heap_page` и финальная обработка LSN/prune/checksum остаются
кандидатами на C: это горячий код и PostgreSQL macro ABI. Любой оставшийся C
вызывается через корректный pgrx error boundary; `ereport(ERROR)` не должен
перескакивать Rust-кадры с ресурсами. Невыровненный TOAST pointer обрабатывается
через PostgreSQL `VARATT_EXTERNAL_GET_POINTER` и `memcpy`.

## Definition of Done для каждого нового AM/case

- [ ] Есть короткий design note: page layout, version differences, locking,
      WAL/checksum/recovery semantics и причина выбора C или Rust.
- [ ] Есть manifest/validation для всех новых prerequisites; неподдерживаемый
      случай отклоняется до изменения target storage.
- [ ] Source и target проходят одинаковый happy path на PG16–PG18 либо явно
      документируется и проверяется ограничение major.
- [ ] Есть данные, которые действительно создают нужные страницы/edge cases;
      проверяется физическая/логическая корректность и index usage.
- [ ] Проверены empty/large/corrupt/unfrozen/rollback cases, где применимо.
- [ ] Есть отказоустойчивость: ошибка во время stream не оставляет видимых
      частичных изменений; source/target locks освобождаются.
- [ ] Проверены checksum on/off и `generate_wal` on/off; для no-WAL mode явно
      указаны ограничения replicas/PITR.
- [ ] Добавлены Rust unit/integration и BDD-сценарии без shell-only assertions.
- [ ] `cargo fmt --check`, строгий Clippy, unit tests и соответствующий Docker
      BDD зелёные; warnings не подавляются без письменной причины.
- [ ] Для per-page изменений есть benchmark до/после; регрессия throughput
      объяснена и принята, а не скрыта.

## Не входит автоматически

Cross-major upgrade, online без freeze, merge в непустую таблицу, автоматическое
создание произвольных index AM и production failover — отдельные проекты, а не
побочный эффект добавления одного index type. ACL, ownership, grants, roles и
privileges пока сознательно не переносятся (out of scope).
