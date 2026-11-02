@validation @safety-oid-types
Feature: Types containing catalog OIDs fail closed

  Scenario Outline: A <label> column is rejected without creating a target
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TYPE public.mood AS ENUM ('happy', 'sad');
      CREATE TABLE public.events(id integer PRIMARY KEY, unsafe_value <type>);
      INSERT INTO public.events VALUES (1, <value>);
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TYPE public.mood AS ENUM ('sad', 'happy');
      """
    Then the following SQL on "target" fails with "unsafe type":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.imported_events', true
      );
      """
    And the following SQL on "target" returns "NULL":
      """
      SELECT to_regclass('public.imported_events');
      """
    And the following SQL on "source" returns "1":
      """
      SELECT count(*) FROM public.events;
      """

    Examples:
      | label            | type                | value                       |
      | enum             | public.mood         | 'happy'::public.mood        |
      | enum array       | public.mood[]       | ARRAY['happy'::public.mood] |
      | regclass         | regclass            | 'pg_class'::regclass        |
      | OID              | oid                 | 42                          |
      | oidvector        | oidvector           | '42 43'::oidvector          |
      | aclitem          | aclitem             | 'postgres=r/postgres'       |
      | system composite | pg_class            | NULL::pg_class              |
