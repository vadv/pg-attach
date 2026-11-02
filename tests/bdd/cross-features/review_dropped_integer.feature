@cross-major @review-dropped-integer
Feature: Dropped fixed-width placeholders preserve physical layout

  Scenario: Automatic target creation preserves a dropped integer attribute
    Given a fresh PostgreSQL 16 to 17 pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, obsolete integer);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTERNAL;
      INSERT INTO public.events VALUES (1, repeat('x', 12000), 99);
      ALTER TABLE public.events DROP COLUMN obsolete;
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
        'public.events', 'public.imported_events', true
      );
      """
    Then the following SQL on "target" returns "1|12000|4|t":
      """
      SELECT count(*), sum(length(payload)),
             (SELECT attlen FROM pg_attribute
              WHERE attrelid = 'public.imported_events'::regclass AND attnum = 3),
             (SELECT attbyval FROM pg_attribute
              WHERE attrelid = 'public.imported_events'::regclass AND attnum = 3)
      FROM public.imported_events;
      """
    And the following SQL on "target" returns "0|1":
      """
      SELECT (SELECT count(*) FROM verify_heapam('public.imported_events')),
             (SELECT count(*) FROM (
                SELECT bt_index_check('public.imported_events_pkey', true) AS result
              ) AS checked);
      """
