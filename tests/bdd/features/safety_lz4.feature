@validation @safety-lz4
Feature: Unsupported TOAST compression fails closed

  Scenario: lz4 data is rejected when the target lacks lz4 support
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTENDED;
      ALTER TABLE public.events ALTER COLUMN payload SET COMPRESSION lz4;
      INSERT INTO public.events VALUES (1, repeat('lz4', 10000));
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTENDED;
      INSERT INTO public.events VALUES (42, 'kept');
      """
    Then the following SQL on "target" fails with "lz4":
      """
      SET pg_attach.debug_disable_lz4 = on;
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL on "target" returns "1|42|kept":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """
