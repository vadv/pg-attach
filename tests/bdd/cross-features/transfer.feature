@cross-major @core
Feature: Upward physical transfer across PostgreSQL majors

  Scenario Outline: PostgreSQL <source_major> to <target_major> preserves heap, TOAST, NULL, dropped columns and btree with checksums <checksums> and WAL <wal>
    Given a fresh PostgreSQL <source_major> to <target_major> pair with checksums "<checksums>"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTERNAL;
      ALTER TABLE public.events ADD COLUMN obsolete text;
      ALTER TABLE public.events DROP COLUMN obsolete;
      INSERT INTO public.events
      SELECT i, repeat('payload-' || i, 1000), CASE WHEN i % 2 = 0 THEN 'even' END
      FROM generate_series(1, 40) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION amcheck;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', <wal>
      );
      """
    Then the following SQL on "target" returns "40|391000|20":
      """
      SELECT count(*), sum(length(payload)), count(*) FILTER (WHERE note IS NULL)
      FROM public.imported_events;
      """
    And the following SQL on "target" returns "20|10000|even":
      """
      SET enable_seqscan = off;
      SELECT id, length(payload), note FROM public.imported_events WHERE id = 20;
      """
    And the following SQL on "target" contains "Index Only Scan":
      """
      SET enable_seqscan = off;
      EXPLAIN (COSTS OFF) SELECT id FROM public.imported_events WHERE id = 20;
      """
    And the following SQL on "target" returns "0|1":
      """
      SELECT (SELECT count(*) FROM verify_heapam('public.imported_events')),
             (SELECT count(*) FROM (
                SELECT bt_index_check('public.imported_events_pkey', true) AS result
              ) AS checked);
      """

    Examples:
      | source_major | target_major | checksums | wal   |
      | 16           | 17           | enabled   | true  |
      | 16           | 17           | enabled   | false |
      | 16           | 17           | disabled  | true  |
      | 16           | 17           | disabled  | false |
      | 16           | 18           | enabled   | true  |
      | 16           | 18           | enabled   | false |
      | 16           | 18           | disabled  | true  |
      | 16           | 18           | disabled  | false |
      | 17           | 18           | enabled   | true  |
      | 17           | 18           | enabled   | false |
      | 17           | 18           | disabled  | true  |
      | 17           | 18           | disabled  | false |

  Scenario Outline: Built-in <type> values survive a 16 to 18 transfer
    Given a fresh PostgreSQL 16 to 18 pair with checksums "enabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload <type>);
      INSERT INTO public.events VALUES (1, <value>);
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    Then the following SQL on "target" returns "1|<expected>":
      """
      SELECT id, payload::text FROM public.imported_events;
      """

    Examples:
      | type         | value                              | expected                    |
      | numeric(10,3)| 12.340                             | 12.340                      |
      | uuid         | '11111111-2222-3333-4444-555555555555' | 11111111-2222-3333-4444-555555555555 |
      | jsonb        | '[1,2]'::jsonb                    | [1, 2]                      |
      | timestamptz  | '2024-01-02 03:04:05+00'           | 2024-01-02 03:04:05+00      |
      | bytea        | decode('0102', 'hex')              | \x0102                      |
      | inet         | '192.168.1.1/24'                   | 192.168.1.1/24              |
      | integer[]    | ARRAY[1, 2]                        | {1,2}                       |
      | text[]       | ARRAY['a', 'b']                    | {a,b}                       |
      | tsvector     | 'b:2 a:1'::tsvector                | 'a':1 'b':2                 |
      | pg_lsn       | '0/16B6C50'::pg_lsn                | 0/16B6C50                   |
      | varbit       | B'1010'                            | 1010                        |
