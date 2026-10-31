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

## Следующая итерация после BDD: direct disk read

После завершения текущего BDD-набора исследуем замену чтения source через
buffer manager на прямое чтение relation files. Причина — не предполагаемый
выигрыш page-level buffer lock: при удерживаемом `ACCESS EXCLUSIVE` обычные
писатели уже заблокированы. Производительность direct I/O нужно измерить
отдельно; переход не считается заранее более быстрым.

Базовый протокол: `VACUUM FREEZE → LOCK → CHECKPOINT → READ`.

1. Выполнить `VACUUM (FREEZE) table` в autocommit. Vacuum не блокирует DML.
2. В connection A выполнить `BEGIN` и `LOCK TABLE table IN ACCESS EXCLUSIVE
   MODE`; удерживать lock до конца чтения.
3. **После получения lock** повторно проверить heap и TOAST: все tuples frozen,
   нет действующего `xmax` или других неподдерживаемых состояний. При неудаче
   откатить lock и повторить vacuum, а не помечать tuples frozen принудительно.
4. В connection B выполнить `CHECKPOINT` и дождаться его завершения.
5. В connection A читать heap, TOAST и поддерживаемые btree напрямую с диска,
   включая все relation segments и необходимые forks, затем завершить
   транзакцию и снять lock.

Гонки и ограничения протокола явно входят в реализацию:

- между `VACUUM FREEZE` и `LOCK TABLE` возможен DML; поэтому обязательна
  проверка frozen state именно после lock, с abort/retry;
- между checkpoint и чтением не должно быть окна до lock: checkpoint выполняем
  только после `ACCESS EXCLUSIVE`, иначе dirty pages могут появиться снова;
- `LOCK TABLE` сам не сбрасывает shared buffers; без checkpoint или адресного
  flush прямой `pread()` может прочитать устаревшую страницу;
- `VACUUM` и `CHECKPOINT` нельзя выполнить внутри transaction block/SQL
  function, а table lock живёт до конца транзакции. Поэтому одна SQL-функция
  `lock + vacuum + checkpoint + read` не подходит: нужен внешний orchestration
  с двумя подключениями либо отдельно спроектированный background worker;
- checkpoint затрагивает весь кластер и может быть дорогим. Для PoC это
  допустимо; затем исследовать адресный flush heap/TOAST/index relations;
- прямое чтение обязано учитывать tablespace, relfilenode, сегменты и forks,
  а не открывать только один путь из `pg_relation_filepath()`.

До реализации завершить BDD matrix и CI. В новой итерации сначала добавить
race/rollback BDD-сценарии и benchmark buffer manager против direct read, затем
менять per-page path. Текущий checked-in SMGR/buffer-manager reader остаётся
рабочим до доказательства корректности замены.

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
