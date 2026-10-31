@freeze_retry
Feature: A committed writer between vacuum and lock triggers bounded retry

  Background:
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION dblink;
      CREATE TABLE public.events(id integer PRIMARY KEY);
      INSERT INTO public.events VALUES (1);
      CREATE SEQUENCE public.freeze_attempts;
      CREATE TABLE public.fault_injection(attempts integer);
      ALTER FUNCTION pg_attach_lock_source(text) RENAME TO real_lock_source;
      CREATE FUNCTION pg_attach_lock_source(source_table text) RETURNS void
      LANGUAGE plpgsql AS $$
      DECLARE n bigint;
      BEGIN
        n := nextval('public.freeze_attempts');
        IF n <= (SELECT attempts FROM public.fault_injection) THEN
          PERFORM dblink_exec(
            'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
            format('INSERT INTO public.events VALUES (%s)', 100 + n)
          );
        END IF;
        PERFORM real_lock_source(source_table);
      END
      $$;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.kept(id integer PRIMARY KEY);
      INSERT INTO public.kept VALUES (99);
      """

  Scenario: One committed writer causes exactly one retry and its row is included
    Given the following SQL is executed on "source":
      """
      INSERT INTO public.fault_injection VALUES (1);
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.kept', true
      );
      """
    Then the following SQL on "source" returns "2":
      """
      SELECT last_value FROM public.freeze_attempts;
      """
    And the following SQL on "target" returns "1,101":
      """
      SELECT string_agg(id::text, ',' ORDER BY id) FROM public.kept;
      """
    And the following SQL on "source" returns "t":
      """
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      SELECT true;
      ROLLBACK;
      """

  Scenario Outline: Three consecutive writers exhaust retry without changing the destination
    Given the following SQL is executed on "source":
      """
      INSERT INTO public.fault_injection VALUES (3);
      """
    Then the following SQL on "target" fails with "after 3 VACUUM FREEZE attempts":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.<destination>', true
      );
      """
    And the following SQL on "source" returns "3|4":
      """
      SELECT last_value, (SELECT count(*) FROM public.events) FROM public.freeze_attempts;
      """
    And the following SQL on "target" returns "1|99|t":
      """
      SELECT count(*), min(id), to_regclass('public.new_copy') IS NULL FROM public.kept;
      """
    And the following SQL on "source" returns "t":
      """
      BEGIN;
      LOCK TABLE public.events IN ACCESS EXCLUSIVE MODE NOWAIT;
      SELECT true;
      ROLLBACK;
      """
    When the following SQL is executed on "source":
      """
      UPDATE public.fault_injection SET attempts = 0;
      """
    And the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.<destination>', true
      );
      """
    Then the following SQL on "source" returns "4":
      """
      SELECT last_value FROM public.freeze_attempts;
      """
    And the following SQL on "target" returns "1,101,102,103":
      """
      SELECT string_agg(id::text, ',' ORDER BY id) FROM public.<destination>;
      """

    Examples:
      | destination |
      | kept        |
      | new_copy    |
