@core @smoke
Feature: Core physical table transfer

  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """

  @toast @index
  Scenario: Heap, external TOAST, NULL and btree survive import
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events
      SELECT i, repeat('payload-' || i, 1000),
             CASE WHEN i % 2 = 0 THEN 'even' END
      FROM generate_series(1, 2000) AS i;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    Then the following SQL on "target" returns "2000|22893000|4000":
      """
      SELECT count(*), sum(length(payload)), sum(length(note))
      FROM public.imported_events;
      """
    And the following SQL on "target" returns "42|10000":
      """
      SET enable_seqscan = off;
      SELECT id, length(payload) FROM public.imported_events WHERE id = 42;
      """

  Scenario: An empty table is transferred
    When the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "0":
      """
      SELECT count(*) FROM public.events;
      """

  Scenario: Dropped columns preserve the physical attribute layout
    When the following SQL is executed on "source":
      """
      ALTER TABLE public.events ADD COLUMN obsolete text;
      INSERT INTO public.events VALUES (1, repeat('x', 12000), 'kept', 'drop me');
      ALTER TABLE public.events DROP COLUMN obsolete;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|12000|4":
      """
      SELECT count(*), sum(length(payload)), sum(length(note)) FROM public.events;
      """

  Scenario: Repeated import replaces the destination
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events VALUES (1, repeat('x', 12000), 'old');
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL is executed on "source":
      """
      TRUNCATE public.events;
      INSERT INTO public.events VALUES (2, repeat('y', 12000), 'new');
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|2|3":
      """
      SELECT count(*), min(id), sum(length(note)) FROM public.events;
      """
