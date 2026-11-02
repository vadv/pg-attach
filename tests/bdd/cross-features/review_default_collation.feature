@cross-major @validation @review-default-collation
Feature: Database-default collation identity must be provable

  Scenario: Different default ICU locales without matching catalog rows are rejected without changing the target
    Given a fresh PostgreSQL 16 to 17 pair with default ICU locales "de-u-co-phonebk" and "sv"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer, payload text PRIMARY KEY);
      INSERT INTO public.events VALUES (1, 'ä'), (2, 'z');
      """
    Then the following SQL on "source" returns "t":
      """
      SELECT coalesce(row_to_json(pg_database)->>'daticulocale',
                      row_to_json(pg_database)->>'datlocale') = 'de-u-co-phonebk'
         AND NOT EXISTS (
           SELECT 1 FROM pg_collation WHERE collname = 'de-u-co-phonebk'
         )
      FROM pg_database WHERE datname = current_database();
      """
    When the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer, payload text PRIMARY KEY);
      INSERT INTO public.events VALUES (42, 'kept');
      """
    Then the following SQL on "target" returns "t":
      """
      SELECT coalesce(row_to_json(pg_database)->>'daticulocale',
                      row_to_json(pg_database)->>'datlocale') = 'sv'
         AND NOT EXISTS (
           SELECT 1 FROM pg_collation WHERE collname = 'sv'
         )
      FROM pg_database WHERE datname = current_database();
      """
    Then the following SQL on "target" fails with "collation":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL on "target" returns "1|42|kept":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """
