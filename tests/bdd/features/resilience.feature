Feature: Transfer resilience and rollback

  @wal
  Scenario: WAL mode creates a persistent logged destination
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events SELECT i, repeat('payload', 1000), 'wal' FROM generate_series(1, 100) AS i;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.wal_probe(before_lsn pg_lsn);
      INSERT INTO public.wal_probe SELECT pg_current_wal_lsn();
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.wal_events', true
      );
      """
    Then the following SQL on "target" returns "p|t|100":
      """
      SELECT c.relpersistence, pg_current_wal_lsn() > w.before_lsn, count(*)
      FROM public.wal_probe w, pg_class c, public.wal_events e
      WHERE c.oid = 'public.wal_events'::regclass
      GROUP BY c.relpersistence, w.before_lsn;
      """

  @wal
  Scenario: No-WAL PoC mode preserves permanent catalog state
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events SELECT i, repeat('payload', 1000), 'nowal' FROM generate_series(1, 100) AS i;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.nowal_events', false
      );
      """
    Then the following SQL on "target" returns "p|100|nowal":
      """
      SELECT c.relpersistence, count(e.*), min(e.note)
      FROM pg_class c, public.nowal_events e
      WHERE c.oid = 'public.nowal_events'::regclass
      GROUP BY c.relpersistence;
      """

  Scenario: Import works with data checksums enabled
    Given a fresh PostgreSQL pair with checksums "enabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events VALUES (1, repeat('checksum', 1000), 'on');
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
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
    Then the following SQL on "target" returns "on|1|on":
      """
      SELECT current_setting('data_checksums'), count(*), min(note) FROM public.events_copy;
      """

  Scenario: Import works with data checksums disabled
    Given a fresh PostgreSQL pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events VALUES (1, repeat('checksum', 1000), 'off');
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
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
    Then the following SQL on "target" returns "off|1|off":
      """
      SELECT current_setting('data_checksums'), count(*), min(note) FROM public.events_copy;
      """

  @slow
  Scenario: Source disconnect during COPY leaves destination unchanged
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION pgcrypto;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events SELECT i, encode(gen_random_bytes(1000), 'hex'), 'new' FROM generate_series(1, 20000) AS i;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.interrupted(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.interrupted VALUES (7, 'original', 'kept');
      """
    When source is stopped during the following SQL on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.interrupted', true
      );
      """
    Then the following SQL on "target" returns "1|7|original|kept":
      """
      SELECT count(*), min(id), min(payload), min(note) FROM public.interrupted;
      """

  @slow
  Scenario: Target cancellation leaves destination unchanged
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION pgcrypto;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events SELECT i, encode(gen_random_bytes(1000), 'hex'), 'new' FROM generate_series(1, 20000) AS i;
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE, ANALYZE) public.events;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.interrupted(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.interrupted VALUES (7, 'original', 'kept');
      """
    When the following SQL on "target" is cancelled:
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.interrupted', true
      );
      """
    Then the following SQL on "target" returns "1|7|original|kept":
      """
      SELECT count(*), min(id), min(payload), min(note) FROM public.interrupted;
      """
