@legacy_frozen
Feature: Legacy frozen tuple representation is rejected, not imported

  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION pg_visibility;
      CREATE EXTENSION pageinspect;
      ALTER DATABASE postgres SET log_min_messages = debug1;
      CREATE FUNCTION legacy_xmin(regclass) RETURNS void
        AS '$libdir/pg_attach_bdd_fixtures', 'pg_attach_test_legacy_xmin'
        LANGUAGE C STRICT;
      CREATE TABLE public.events(id integer PRIMARY KEY) WITH (autovacuum_enabled = false);
      INSERT INTO public.events VALUES (1);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    And the following SQL is executed on "source":
      """
      SELECT legacy_xmin('public.events');
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.kept(id integer PRIMARY KEY);
      INSERT INTO public.kept VALUES (99);
      CREATE TABLE public.saved_storage AS
        SELECT oid, relfilenode FROM pg_class
        WHERE oid = 'public.kept'::regclass
           OR oid IN (SELECT indexrelid FROM pg_index WHERE indrelid = 'public.kept'::regclass);
      """

  Scenario Outline: Streaming rejects a legacy tuple even with an ALL_FROZEN VM bit
    Then the following SQL on "source" returns "2|t|f":
      """
      SELECT t_xmin, 'HEAP_XMIN_COMMITTED' = ANY(raw_flags),
             'HEAP_XMIN_FROZEN' = ANY(combined_flags)
      FROM heap_page_items(get_raw_page('public.events', 0)),
           LATERAL heap_tuple_infomask_flags(t_infomask, t_infomask2);
      """
    And the following SQL on "source" returns "t":
      """
      SELECT all_frozen FROM pg_visibility_map('public.events');
      """
    And the following SQL on "source" returns "0":
      """
      SELECT count(*) FROM pg_check_frozen('public.events');
      """
    And the following SQL on "target" fails with "contains a tuple that is not frozen":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.<destination>', true
      );
      """
    And frozen validation reads 0 data pages across 1 relations
    And the following SQL on "target" returns "1|99|t|t":
      """
      SELECT count(*), min(id), to_regclass('public.new_copy') IS NULL,
        (SELECT bool_and(c.relfilenode = s.relfilenode) FROM public.saved_storage s JOIN pg_class c USING (oid))
      FROM public.kept;
      """
    And the following SQL on "source" returns "t":
      """
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      SELECT true;
      ROLLBACK;
      """
    And source PostgreSQL did not crash

    Examples:
      | destination |
      | kept        |
      | new_copy    |

  Scenario: Missing VM does not let a legacy raw xmin pass tuple validation
    When the following SQL is executed on "source":
      """
      SELECT pg_truncate_visibility_map('public.events');
      """
    Then the following SQL on "source" returns "f":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    And frozen validation reads 1 data pages across 1 relations
