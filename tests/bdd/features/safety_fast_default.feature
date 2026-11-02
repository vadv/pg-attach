@validation @safety-fast-default
Feature: Fast defaults fail closed

  Scenario: A source fast default is rejected without creating a target
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      INSERT INTO public.events VALUES (1, 'before default');
      ALTER TABLE public.events ADD COLUMN missing_value text NOT NULL DEFAULT 'fast default';
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    Then the following SQL on "target" fails with "missing values":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    And the following SQL on "target" returns "NULL|0":
      """
      SELECT to_regclass('public.imported_events'), count(*) FROM pg_stat_user_tables;
      """
    And the following SQL on "source" returns "t|fast default":
      """
      SELECT a.atthasmissing, trim(BOTH '{}"' FROM a.attmissingval::text)
      FROM pg_attribute a
      WHERE a.attrelid = 'public.events'::regclass AND a.attname = 'missing_value';
      """
