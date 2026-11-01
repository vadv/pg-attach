Feature: Local table clone inside a single database
  The same-page pipeline as the cross-instance import, but without libpq:
  pg_attach_import_local freezes the source itself and clones it under one
  postmaster, so the user calls a single function.

  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """

  @local
  Scenario: Local clone transfers TOAST and btree pages
    Given the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text NOT NULL,
        note text
      );
      INSERT INTO public.events
        SELECT i, repeat(md5(i::text), 700), CASE WHEN i % 2 = 0 THEN 'even' ELSE 'odd' END
        FROM generate_series(1, 500) AS i;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import_local('public.events', 'public.events_copy', true);
      """
    Then the following SQL on "target" returns "500|500|700|even":
      """
      SELECT (SELECT count(*) FROM public.events_copy)
          || '|' || (SELECT count(*) FROM public.events_copy WHERE id BETWEEN 1 AND 500)
          || '|' || (SELECT length(payload) / 32 FROM public.events_copy WHERE id = 42)
          || '|' || (SELECT note FROM public.events_copy WHERE id = 42);
      """

  @local
  Scenario: Local clone freezes an unfrozen source by itself
    Given the following SQL is executed on "target":
      """
      CREATE TABLE public.warm (id integer PRIMARY KEY, value text NOT NULL);
      INSERT INTO public.warm VALUES (1, 'one');
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import_local('public.warm', 'public.warm_copy', false);
      """
    Then the following SQL on "target" returns "1|one|t":
      """
      SELECT (SELECT count(*) FROM public.warm_copy)
          || '|' || (SELECT value FROM public.warm_copy WHERE id = 1)
          || '|' || (SELECT count(*) = 0 FROM pg_stat_activity
                      WHERE query LIKE '%VACUUM (FREEZE)%' AND state = 'active');
      """

  @local
  Scenario: Local clone without WAL keeps rows readable
    Given the following SQL is executed on "target":
      """
      CREATE TABLE public.small (id integer PRIMARY KEY, value text NOT NULL);
      INSERT INTO public.small VALUES (1, 'one'), (2, 'two');
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import_local('public.small', 'public.small_copy', false);
      """
    Then the following SQL on "target" returns "2|two":
      """
      SELECT (SELECT count(*) FROM public.small_copy)
          || '|' || (SELECT value FROM public.small_copy WHERE id = 2);
      """

  @local
  Scenario: Local clone rejects the source table as its own target
    Given the following SQL is executed on "target":
      """
      CREATE TABLE public.self (id integer PRIMARY KEY);
      INSERT INTO public.self VALUES (1);
      """
    When the following SQL on "target" fails with "must differ from the source table":
      """
      SELECT pg_attach_import_local('public.self', 'public.self', true);
      """
