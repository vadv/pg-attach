@destination
Feature: Destination table modes

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

  Scenario: Existing non-empty destination is completely overwritten
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events VALUES (1, 'source', 'new');
      """
    And the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (99, 'stale', 'old');
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|1|0":
      """
      SELECT count(*), min(id), count(*) FILTER (WHERE id = 99)
      FROM public.events;
      """

  Scenario: Auto-create restores constraints and defaults under another name
    When the following SQL is executed on "source":
      """
      ALTER TABLE public.events ALTER COLUMN payload SET NOT NULL;
      ALTER TABLE public.events ALTER COLUMN note SET DEFAULT 'missing';
      ALTER TABLE public.events ADD CONSTRAINT events_note_check
        CHECK (note IN ('even', 'odd', 'missing'));
      CREATE INDEX events_note_idx ON public.events(note);
      INSERT INTO public.events VALUES (1, 'payload', 'even');
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.archive_events', true
      );
      """
    Then the following SQL on "target" returns "1|2|t":
      """
      SELECT count(*),
             (SELECT count(*) FROM pg_index
              WHERE indrelid = 'public.archive_events'::regclass),
             (SELECT attnotnull FROM pg_attribute
              WHERE attrelid = 'public.archive_events'::regclass
                AND attname = 'payload')
      FROM public.archive_events;
      """
    And the following SQL on "target" returns "missing":
      """
      INSERT INTO public.archive_events(id, payload) VALUES (2, 'after import')
      RETURNING note;
      """
    And the following SQL on "target" fails with "violates check constraint":
      """
      INSERT INTO public.archive_events VALUES (3, 'bad', 'invalid');
      """
    And the following SQL on "target" fails with "null value in column":
      """
      INSERT INTO public.archive_events VALUES (4, NULL, 'even');
      """

  Scenario: Multiple indexes survive repeated import into auto-created destination
    When the following SQL is executed on "source":
      """
      CREATE INDEX events_payload_idx ON public.events(payload);
      CREATE INDEX events_note_idx ON public.events(note);
      INSERT INTO public.events VALUES (1, 'old payload', 'old');
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events_copy', true
      );
      """
    And the following SQL is executed on "source":
      """
      TRUNCATE public.events;
      INSERT INTO public.events VALUES (999, 'new payload', 'new');
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events_copy', true
      );
      """
    Then the following SQL on "target" returns "1|999|3|new payload":
      """
      SET enable_seqscan = off;
      SELECT count(*), min(id),
             (SELECT count(*) FROM pg_index
              WHERE indrelid = 'public.events_copy'::regclass),
             min(payload)
      FROM public.events_copy;
      """
