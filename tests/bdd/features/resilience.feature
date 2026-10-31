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

  @wal @slow
  Scenario: Target crash after WAL import recovers through full-page images
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events SELECT i, repeat('payload', 1000), 'crash' FROM generate_series(1, 2000) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.crashed_events', true
      );
      """
    And target postmaster is crashed and restarted
    Then the following SQL on "target" returns "2000|2000":
      """
      SELECT count(*)::text, count(*) FILTER (WHERE length(payload) = 7000)::text
      FROM public.crashed_events;
      """

  Scenario: Import works with data checksums enabled
    Given a fresh PostgreSQL pair with checksums "enabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events VALUES (1, repeat('checksum', 1000), 'on');
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
  Scenario: Hung source operation is interrupted by timeout
    Given a fresh PostgreSQL pair
    When the following SQL on "source" is interrupted by timeout:
      """
      SELECT pg_sleep(60);
      """
    Then the following SQL on "target" returns "1":
      """
      SELECT 1;
      """

  @slow
  Scenario: Hung target operation is interrupted by timeout
    Given a fresh PostgreSQL pair
    When the following SQL on "target" is interrupted by timeout:
      """
      SELECT pg_sleep(60);
      """
    Then the following SQL on "target" returns "1":
      """
      SELECT 1;
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
  Scenario: Direct reader holds the source lock against a concurrent writer
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION pgcrypto;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events SELECT i, encode(gen_random_bytes(1000), 'hex'), 'original' FROM generate_series(1, 20000) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      """
    When a source writer is blocked during the following SQL on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "source" returns "20001":
      """
      SELECT count(*)::text FROM public.events;
      """
    And the following SQL on "target" returns "20000|0":
      """
      SELECT count(*)::text, count(*) FILTER (WHERE id = 20001)::text FROM public.events;
      """

  Scenario: Target cancellation leaves destination unchanged
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION pgcrypto;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events SELECT i, encode(gen_random_bytes(1000), 'hex'), 'new' FROM generate_series(1, 20000) AS i;
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

  @checksum
  Scenario Outline: A corrupted transported page aborts the import and keeps the old table
    Given a fresh PostgreSQL pair with checksums "<checksums>"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      INSERT INTO public.events
      SELECT i, repeat('payload-' || i, 1000) FROM generate_series(1, 200) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      INSERT INTO public.events VALUES (0, 'kept');
      """
    Then the following SQL on "target" fails with "page verification failed":
      """
      SET pg_attach.debug_corrupt_page = 1;
      SET pg_attach.debug_corrupt_offset = <offset>;
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL on "target" returns "1|kept":
      """
      SELECT count(*), min(payload) FROM public.events;
      """

    Examples:
      | checksums | offset |
      | enabled   | 8190   |
      | enabled   | 14     |
      | disabled  | 14     |

  @wal
  Scenario: WAL full-page images reconstruct a deleted heap file
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      INSERT INTO public.events
      SELECT i, repeat('payload-' || i, 1000), 'fpi' FROM generate_series(1, 200) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CHECKPOINT;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    And the data file path of target relation "public.imported_events" is recorded
    And target postmaster is crashed
    And the recorded target data file is removed
    And target postmaster is started
    Then the following SQL on "target" returns "200|10000|fpi":
      """
      SELECT count(*),
             (SELECT length(payload) FROM public.imported_events WHERE id = 42),
             (SELECT note FROM public.imported_events WHERE id = 42);
      """
