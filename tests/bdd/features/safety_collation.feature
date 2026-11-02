@validation @safety-collation
Feature: Collation versions must match

  Scenario: A target collation-version mismatch preserves existing rows
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text COLLATE "C");
      INSERT INTO public.events VALUES (1, 'source');
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text COLLATE "C");
      INSERT INTO public.events VALUES (42, 'kept');
      UPDATE pg_collation SET collversion = 'bdd-mismatch' WHERE oid = '"C"'::regcollation;
      """
    Then the following SQL on "target" fails with "collation":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL on "target" returns "1|42|kept|bdd-mismatch":
      """
      SELECT count(*), min(id), min(payload),
             (SELECT collversion FROM pg_collation WHERE oid = '"C"'::regcollation)
      FROM public.events;
      """
