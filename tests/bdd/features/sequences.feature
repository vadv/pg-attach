@core
Feature: Sequence state transfer

  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events (
        id serial PRIMARY KEY,
        payload text
      );
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """

  Scenario: Auto-create rejects serial defaults before touching the target
    When the following SQL on "target" fails with "automatic target creation does not support serial/nextval defaults":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    And the following SQL on "target" returns "f":
      """
      SELECT to_regclass('public.imported_events') IS NOT NULL;
      """

  Scenario: Explicit target inherits the source sequence state
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events(payload)
      SELECT 'row-' || i FROM generate_series(1, 100) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE TABLE public.imported_events (id serial PRIMARY KEY, payload text);
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    Then the following SQL on "target" returns "100|100":
      """
      SELECT count(*), max(id) FROM public.imported_events;
      """
    And the following SQL on "target" returns "100|t":
      """
      SELECT last_value, is_called FROM public.imported_events_id_seq;
      """
    And the following SQL on "target" returns "101":
      """
      INSERT INTO public.imported_events(payload) VALUES ('after import') RETURNING id;
      """

  Scenario: An uncalled sequence stays uncalled
    When the following SQL is executed on "target":
      """
      CREATE TABLE public.imported_events (id serial PRIMARY KEY, payload text);
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    Then the following SQL on "target" returns "1|f":
      """
      SELECT last_value, is_called FROM public.imported_events_id_seq;
      """
    And the following SQL on "target" returns "1":
      """
      INSERT INTO public.imported_events(payload) VALUES ('after import') RETURNING id;
      """

  Scenario: Explicit target without a serial default imports without setval
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events(payload)
      SELECT 'row-' || i FROM generate_series(1, 10) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE TABLE public.imported_events (id integer PRIMARY KEY, payload text);
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    Then the following SQL on "target" returns "10|10":
      """
      SELECT count(*), max(id) FROM public.imported_events;
      """

  Scenario: An unowned nextval default on the target is rejected
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events(payload)
      SELECT 'row-' || i FROM generate_series(1, 10) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE SEQUENCE public.imported_events_id_seq;
      CREATE TABLE public.imported_events (
        id integer PRIMARY KEY DEFAULT nextval('public.imported_events_id_seq'),
        payload text
      );
      """
    And the following SQL on "target" fails with "uses nextval() on a sequence the column does not own":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
