@validation
Feature: Transfer validation and failure isolation

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

  Scenario: Incompatible table layout leaves destination unchanged
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events VALUES (1, 'source', 'new');
      """
    And the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id bigint PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (99, 'stale', 'old');
      """
    Then the following SQL on "target" fails with "source and target table attributes differ":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|99|stale":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: Missing source relation leaves destination unchanged
    When the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (42, 'keep', 'old');
      """
    Then the following SQL on "target" fails with "does not exist":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.missing', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|42|keep":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: Invalid source schema name leaves destination unchanged
    When the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (42, 'keep', 'old');
      """
    Then the following SQL on "target" fails with "not a valid identifier":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public..events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|42|keep":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: Invalid target schema name leaves destination unchanged
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
      INSERT INTO public.events VALUES (42, 'keep', 'old');
      """
    Then the following SQL on "target" fails with "invalid name syntax":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public..events', true
      );
      """
    Then the following SQL on "target" returns "1|42|keep":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: Non-default tablespace is readable by a non-superuser source owner
    Given a non-default tablespace is available on "source"
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events VALUES (1, 'from tablespace', 'owner');
      ALTER TABLE public.events SET TABLESPACE attach_space;
      CREATE ROLE attach_owner LOGIN;
      GRANT pg_checkpoint TO attach_owner;
      ALTER TABLE public.events OWNER TO attach_owner;
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=attach_owner',
        'public.events', 'public.events_copy', true
      );
      """
    Then the following SQL on "target" returns "1|from tablespace|owner":
      """
      SELECT count(*), min(payload), min(note) FROM public.events_copy;
      """

  Scenario: Import freezes unfrozen source tuples automatically
    When the following SQL is executed on "source":
      """
      INSERT INTO public.events VALUES (1, 'frozen by import', 'new');
      """
    And the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (42, 'keep', 'old');
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "source" returns "t":
      """
      SELECT pg_attach_source_is_frozen('public.events');
      """
    Then the following SQL on "target" returns "1|1|frozen by import":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: Unsupported unlogged relation leaves destination unchanged
    When the following SQL is executed on "source":
      """
      ALTER TABLE public.events SET UNLOGGED;
      """
    And the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (42, 'keep', 'old');
      """
    Then the following SQL on "target" fails with "source must be a plain logged table":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|42|keep":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: Non-btree source index leaves destination unchanged
    When the following SQL is executed on "source":
      """
      CREATE INDEX events_id_hash ON public.events USING hash (id);
      """
    And the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (42, 'keep', 'old');
      """
    Then the following SQL on "target" fails with "is not a btree":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|42|keep":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: Offline source leaves destination unchanged
    When the following SQL is executed on "target":
      """
      CREATE TABLE public.events (
        id integer PRIMARY KEY,
        payload text,
        note text
      );
      INSERT INTO public.events VALUES (42, 'keep', 'old');
      """
    Given source PostgreSQL is stopped
    Then the following SQL on "target" fails with "Connection refused":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    Then the following SQL on "target" returns "1|42|keep":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """
