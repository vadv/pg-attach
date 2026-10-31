@visibility_map
Feature: Frozen validation avoids data-page reads using the visibility map

  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION pg_visibility;
      ALTER DATABASE postgres SET log_min_messages = debug1;
      """

  Scenario: Automatic freeze skips validation reads for both heap and external TOAST
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTERNAL;
      INSERT INTO public.events SELECT i, repeat(md5(i::text), 256) FROM generate_series(1, 100) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events_copy', true
      );
      """
    Then the following SQL on "source" returns "t|t":
      """
      SELECT
        (SELECT count(*) > 0 AND bool_and(all_frozen) FROM pg_visibility_map('public.events')),
        (SELECT count(*) > 0 AND bool_and(all_frozen) FROM pg_visibility_map(c.reltoastrelid))
      FROM pg_class c WHERE c.oid = 'public.events'::regclass;
      """
    And frozen validation reads 0 data pages across 2 relations
    And the following SQL on "target" returns "100|819200|t":
      """
      SELECT count(*), sum(length(payload)), bool_and(payload = repeat(md5(id::text), 256))
      FROM public.events_copy;
      """

  Scenario: Standalone validation waits for an ordinary reader before trusting VM
    Given the following SQL is executed on "source":
      """
      CREATE EXTENSION dblink;
      CREATE TABLE public.events(id integer);
      INSERT INTO public.events VALUES (1);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    When the following SQL is executed on "source":
      """
      SELECT dblink_connect('validator',
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres application_name=vm-validator');
      BEGIN;
      LOCK TABLE public.events IN ACCESS SHARE MODE;
      SELECT dblink_send_query('validator',
        'BEGIN; SELECT pg_attach_source_is_frozen(''public.events'')');
      DO $$
      DECLARE deadline timestamptz := clock_timestamp() + interval '5 seconds';
      BEGIN
        LOOP
          EXIT WHEN EXISTS (
            SELECT FROM pg_locks l JOIN pg_stat_activity a USING (pid)
            WHERE a.application_name = 'vm-validator'
              AND l.relation = 'public.events'::regclass
              AND l.mode = 'AccessExclusiveLock' AND NOT l.granted
              AND pg_backend_pid() = ANY(pg_blocking_pids(l.pid))
          );
          IF dblink_is_busy('validator') = 0 OR clock_timestamp() > deadline THEN
            RAISE EXCEPTION 'validator did not wait for reader using ACCESS EXCLUSIVE';
          END IF;
          PERFORM pg_sleep(0.01);
        END LOOP;
      END $$;
      COMMIT;
      SELECT * FROM dblink_get_result('validator') AS result(status text);
      SELECT * FROM dblink_get_result('validator') AS result(frozen boolean);
      SELECT * FROM dblink_get_result('validator') AS result(status text);
      SELECT dblink_exec('validator', 'COMMIT');
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      COMMIT;
      SELECT dblink_disconnect('validator');
      """
    Then frozen validation reads 0 data pages across 1 relations

  Scenario: Standalone validation retains ACCESS EXCLUSIVE until transaction commit
    Given the following SQL is executed on "source":
      """
      CREATE EXTENSION dblink;
      CREATE TABLE public.events(id integer);
      INSERT INTO public.events VALUES (1);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    When the following SQL is executed on "source":
      """
      BEGIN;
      SELECT pg_attach_source_is_frozen('public.events');
      DO $$
      BEGIN
        IF NOT EXISTS (SELECT FROM pg_locks WHERE pid = pg_backend_pid()
                       AND relation = 'public.events'::regclass
                       AND mode = 'AccessExclusiveLock' AND granted) THEN
          RAISE EXCEPTION 'validation did not retain ACCESS EXCLUSIVE';
        END IF;
        BEGIN
          PERFORM dblink_exec(
            'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres options=-clock_timeout=100ms',
            'INSERT INTO public.events VALUES (2)'
          );
          RAISE EXCEPTION 'writer was not blocked';
        EXCEPTION WHEN lock_not_available THEN
          NULL;
        END;
      END $$;
      COMMIT;
      SELECT dblink_exec(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres options=-clock_timeout=1s',
        'INSERT INTO public.events VALUES (2)'
      );
      """
    Then the following SQL on "source" returns "1,2":
      """
      SELECT string_agg(id::text, ',' ORDER BY id) FROM public.events;
      """
    And frozen validation reads 0 data pages across 1 relations

  Scenario Outline: Validation releases VM pins before returning within the same transaction
    Given the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_buffercache;
      CREATE TABLE public.events(id integer, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTERNAL;
      INSERT INTO public.events VALUES (1, repeat('x', 3000));
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    When the following SQL is executed on "source":
      """
      <change>
      BEGIN;
      DO $$
      BEGIN
        FOR i IN 1..3 LOOP
          IF pg_attach_source_is_frozen('public.events') IS DISTINCT FROM <frozen> THEN
            RAISE EXCEPTION 'unexpected frozen result';
          END IF;
          IF EXISTS (
            SELECT FROM pg_buffercache b
            WHERE b.reldatabase = (SELECT oid FROM pg_database WHERE datname = current_database())
              AND b.relforknumber = 2 AND b.pinning_backends > 0
              AND b.relfilenode IN (
                SELECT pg_relation_filenode(r)
                FROM pg_class c, LATERAL unnest(ARRAY[c.oid, c.reltoastrelid]) AS r
                WHERE c.oid = 'public.events'::regclass
              )
          ) THEN
            RAISE EXCEPTION 'validation leaked a VM pin';
          END IF;
        END LOOP;
      END $$;
      ROLLBACK;
      """
    Then source PostgreSQL did not crash

    Examples:
      | change                                                                                                  | frozen |
      | SELECT 1;                                                                                               | true   |
      | SELECT pg_truncate_visibility_map(r) FROM pg_class c, LATERAL unnest(ARRAY[c.oid, c.reltoastrelid]) AS r WHERE c.oid = 'public.events'::regclass; | true |
      | SELECT * FROM public.events FOR KEY SHARE;                                                               | false  |

  @slow
  Scenario: Validation crosses a visibility-map page boundary without reading heap
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE PLAIN;
      INSERT INTO public.events SELECT i, repeat('x', 7000) FROM generate_series(1, 33000) AS i;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    Then the following SQL on "source" returns "t|t":
      """
      SELECT pg_relation_size('public.events', 'vm') > current_setting('block_size')::integer,
             (SELECT count(*) = 33000 AND bool_and(all_frozen)
              FROM pg_visibility_map('public.events'));
      """
    And the following SQL on "source" returns "t":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads 0 data pages across 2 relations

  Scenario: Empty heap and TOAST need no validation data reads
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer, payload text);
      """
    Then the following SQL on "source" returns "t":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads 0 data pages across 2 relations

  Scenario Outline: Missing VM for heap or TOAST reads only the unmarked relation
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTERNAL;
      INSERT INTO public.events VALUES (1, repeat('x', 3000));
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    Then the following SQL on "source" returns "8192|8192":
      """
      SELECT pg_relation_size(oid), pg_relation_size(reltoastrelid)
      FROM pg_class WHERE oid = 'public.events'::regclass;
      """
    When the following SQL is executed on "source":
      """
      SELECT pg_truncate_visibility_map(r)
      FROM pg_class c, LATERAL unnest(ARRAY[<relations>]::oid[]) AS r
      WHERE c.oid = 'public.events'::regclass;
      """
    Then the following SQL on "source" returns "t":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads <reads> data pages across 2 relations

    Examples:
      | relations             | reads |
      | c.oid                 | 1     |
      | c.reltoastrelid        | 1     |
      | c.oid, c.reltoastrelid | 2     |

  Scenario: Missing visibility map falls back to checking frozen heap tuples
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer);
      INSERT INTO public.events SELECT generate_series(1, 100);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    When the following SQL is executed on "source":
      """
      SELECT pg_truncate_visibility_map('public.events');
      """
    Then the following SQL on "source" returns "0":
      """
      SELECT all_frozen FROM pg_visibility_map_summary('public.events');
      """
    And the following SQL on "source" returns "t":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads 1 data pages across 1 relations

  Scenario: A writer after freeze is detected while unchanged frozen pages are skipped
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE PLAIN;
      INSERT INTO public.events SELECT i, repeat('x', 6000) FROM generate_series(1, 2) AS i;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events VALUES (3, repeat('y', 6000));
      """
    Then the following SQL on "source" returns "2|1":
      """
      SELECT count(*) FILTER (WHERE all_frozen), count(*) FILTER (WHERE NOT all_frozen)
      FROM pg_visibility_map('public.events');
      """
    And the following SQL on "source" returns "f":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads 1 data pages across 1 relations

  Scenario: All-visible alone does not bypass validation after a tuple lock
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer PRIMARY KEY);
      INSERT INTO public.events VALUES (1);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    When the following SQL is executed on "source":
      """
      SELECT * FROM public.events FOR KEY SHARE;
      """
    Then the following SQL on "source" returns "t|f":
      """
      SELECT all_visible, all_frozen FROM pg_visibility_map('public.events');
      """
    And the following SQL on "source" returns "f":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads 1 data pages across 1 relations

  Scenario Outline: DML clears a frozen page bit and forces tuple validation
    Given the following SQL is executed on "source":
      """
      CREATE TABLE public.events(id integer);
      INSERT INTO public.events VALUES (1);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    Then the following SQL on "source" returns "1":
      """
      SELECT all_frozen FROM pg_visibility_map_summary('public.events');
      """
    When the following SQL is executed on "source":
      """
      <change>
      """
    Then the following SQL on "source" returns "0":
      """
      SELECT all_frozen FROM pg_visibility_map_summary('public.events');
      """
    And the following SQL on "source" returns "f":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads 1 data pages across 1 relations

    Examples:
      | change                               |
      | INSERT INTO public.events VALUES (2); |
      | UPDATE public.events SET id = 2;      |
      | DELETE FROM public.events;            |
