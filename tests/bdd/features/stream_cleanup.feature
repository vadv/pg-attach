@stream_cleanup
Feature: Streaming resources survive early exit and errors without crashing source

  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION dblink;
      CREATE TABLE public.events(id integer PRIMARY KEY) WITH (autovacuum_enabled = false);
      INSERT INTO public.events SELECT generate_series(1, 10000);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """

  Scenario: LIMIT discards the iterator immediately after its header
    When the following SQL is executed on "source":
      """
      BEGIN;
      SELECT pg_attach_lock_source('public.events');
      SELECT dblink_exec('host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres', 'CHECKPOINT');
      DO $$
      BEGIN
        IF (SELECT count(*) FROM (
          SELECT pg_attach_export_stream('public.events', 'public.copy', 0::oid, false) LIMIT 1
        ) AS header) <> 1 THEN
          RAISE EXCEPTION 'stream header missing';
        END IF;
      END $$;
      COMMIT;
      """
    Then the following SQL on "source" returns "10000":
      """
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      SELECT count(*) FROM public.events;
      ROLLBACK;
      """
    And source PostgreSQL did not crash

  Scenario: Executor error after the header does not double-close relation handles
    Given the following SQL is executed on "source":
      """
      CREATE FUNCTION fail_after_header(bytea) RETURNS bytea LANGUAGE plpgsql AS $$
      BEGIN RAISE EXCEPTION 'executor failure after header'; END $$;
      """
    Then the following SQL on "source" fails with "executor failure after header":
      """
      BEGIN;
      SELECT pg_attach_lock_source('public.events');
      SELECT dblink_exec('host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres', 'CHECKPOINT');
      SELECT fail_after_header(pg_attach_export_stream('public.events', 'public.copy', 0::oid, false));
      """
    And the following SQL on "source" returns "10000":
      """
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      SELECT count(*) FROM public.events;
      ROLLBACK;
      """
    And source PostgreSQL did not crash

  Scenario: Cancellation between iterator calls does not crash source
    Given the following SQL is executed on "source":
      """
      CREATE FUNCTION pause_after_header(bytea) RETURNS bytea LANGUAGE plpgsql AS $$
      BEGIN PERFORM pg_sleep(1); RETURN $1; END $$;
      """
    Then the following SQL on "source" fails with "canceling statement due to statement timeout":
      """
      BEGIN;
      SELECT pg_attach_lock_source('public.events');
      SELECT dblink_exec('host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres', 'CHECKPOINT');
      SET LOCAL statement_timeout = '100ms';
      SELECT pause_after_header(pg_attach_export_stream('public.events', 'public.copy', 0::oid, false));
      """
    And the following SQL on "source" returns "10000":
      """
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      SELECT count(*) FROM public.events;
      ROLLBACK;
      """
    And source PostgreSQL did not crash

  Scenario Outline: Legacy rejection on a later page rolls back streamed storage
    Given the following SQL is executed on "source":
      """
      CREATE EXTENSION pageinspect;
      CREATE EXTENSION pg_visibility;
      ALTER DATABASE postgres SET log_min_messages = debug1;
      CREATE FUNCTION legacy_xmin(regclass) RETURNS void
        AS '$libdir/pg_attach_bdd_fixtures', 'pg_attach_test_legacy_xmin' LANGUAGE C STRICT;
      SELECT legacy_xmin('public.events');
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.kept(id integer PRIMARY KEY);
      INSERT INTO public.kept VALUES (99);
      CREATE TABLE public.saved_storage AS SELECT oid, relfilenode FROM pg_class
        WHERE oid = 'public.kept'::regclass
           OR oid IN (SELECT indexrelid FROM pg_index WHERE indrelid = 'public.kept'::regclass);
      """
    Then the following SQL on "source" returns "t|t|t|t":
      """
      SELECT pg_relation_size('public.events') > 8192,
        (SELECT bool_and(all_frozen) FROM pg_visibility_map('public.events')),
        (SELECT bool_and(t_xmin <> '2'::xid) FROM heap_page_items(get_raw_page('public.events', 0))),
        (SELECT t_xmin = '2'::xid FROM heap_page_items(get_raw_page('public.events',
          (pg_relation_size('public.events') / 8192 - 1)::integer)) WHERE lp = 1);
      """
    And the following SQL on "target" fails with "contains a tuple that is not frozen":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.<destination>', <wal>
      );
      """
    And frozen validation reads 0 data pages across 1 relations
    And the following SQL on "target" returns "1|99|t|t":
      """
      SELECT count(*), min(id), to_regclass('public.new_copy') IS NULL,
        (SELECT bool_and(c.relfilenode = s.relfilenode) FROM public.saved_storage s JOIN pg_class c USING (oid))
      FROM public.kept;
      """
    And the following SQL on "source" returns "10000":
      """
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      SELECT count(*) FROM public.events;
      ROLLBACK;
      """
    And source PostgreSQL did not crash

    Examples:
      | destination | wal   |
      | kept        | true  |
      | new_copy    | true  |
      | kept        | false |
      | new_copy    | false |
