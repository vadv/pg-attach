@cross-major @validation
Feature: Cross-major transfer rejects unsafe physical layouts

  Scenario Outline: Downgrade <source_major> to <target_major> is rejected before VACUUM and target changes
    Given a fresh PostgreSQL <source_major> to <target_major> pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      INSERT INTO public.events VALUES (1, 'source');
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      INSERT INTO public.events VALUES (42, 'kept');
      """
    Then the following SQL on "target" fails with "PostgreSQL major":
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
    And the following SQL on "source" returns "0|0":
      """
      SELECT vacuum_count, autovacuum_count FROM pg_stat_user_tables
      WHERE relid = 'public.events'::regclass;
      """

    Examples:
      | source_major | target_major |
      | 17           | 16           |
      | 18           | 16           |
      | 18           | 17           |

  Scenario Outline: Different checksum modes are rejected before VACUUM
    Given a fresh PostgreSQL 16 to 17 pair with source checksums "<source_checksums>" and target checksums "<target_checksums>"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY);
      INSERT INTO public.events VALUES (1);
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY);
      INSERT INTO public.events VALUES (42);
      """
    Then the following SQL on "target" fails with "data checksums":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL on "target" returns "1|42":
      """
      SELECT count(*), min(id) FROM public.events;
      """
    And the following SQL on "source" returns "0|0":
      """
      SELECT vacuum_count, autovacuum_count FROM pg_stat_user_tables
      WHERE relid = 'public.events'::regclass;
      """

    Examples:
      | source_checksums | target_checksums |
      | enabled          | disabled         |
      | disabled         | enabled          |

  Scenario Outline: OID-bearing <label> values are rejected by a 16 to 17 transfer
    Given a fresh PostgreSQL 16 to 17 pair with checksums "disabled"
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

    Examples:
      | label            | type                | value                       |
      | enum             | public.mood         | 'happy'::public.mood        |
      | enum array       | public.mood[]       | ARRAY['happy'::public.mood] |
      | regclass         | regclass            | 'pg_class'::regclass        |
      | OID              | oid                 | 42                          |
      | oidvector        | oidvector           | '42 43'::oidvector          |
      | aclitem          | aclitem             | 'postgres=r/postgres'       |
      | system composite | pg_class            | NULL::pg_class              |

  Scenario: Fast defaults are rejected by a 16 to 17 transfer
    Given a fresh PostgreSQL 16 to 17 pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      INSERT INTO public.events VALUES (1, 'before default');
      ALTER TABLE public.events ADD COLUMN missing_value text NOT NULL DEFAULT 'fast default';
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    Then the following SQL on "target" fails with "missing values":
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

  Scenario: Virtual generated columns are rejected instead of being recreated as stored
    Given a fresh PostgreSQL 18 to 18 pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(
        id integer PRIMARY KEY,
        payload text,
        virtual_value text GENERATED ALWAYS AS (payload || '-virtual') VIRTUAL
      );
      INSERT INTO public.events(id, payload) VALUES (1, 'source');
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    Then the following SQL on "target" fails with "virtual generated":
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

  Scenario: Collation-version drift is rejected by a 16 to 17 transfer
    Given a fresh PostgreSQL 16 to 17 pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text COLLATE "C");
      INSERT INTO public.events VALUES (1, 'source');
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text COLLATE "C");
      INSERT INTO public.events VALUES (42, 'kept');
      UPDATE pg_collation SET collversion = 'bdd-mismatch' WHERE oid = '"C"'::regcollation;
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

  Scenario: Unsupported lz4 is rejected by a 16 to 18 transfer
    Given a fresh PostgreSQL 16 to 18 pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTENDED;
      ALTER TABLE public.events ALTER COLUMN payload SET COMPRESSION lz4;
      INSERT INTO public.events VALUES (1, repeat('lz4', 10000));
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTENDED;
      INSERT INTO public.events VALUES (42, 'kept');
      """
    Then the following SQL on "target" fails with "lz4":
      """
      SET pg_attach.debug_disable_lz4 = on;
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL on "target" returns "1|42|kept":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """

  Scenario: A 16 to 17 export rejects a corrupted source page
    Given a fresh PostgreSQL 16 to 17 pair with checksums "enabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE EXTENSION dblink;
      CREATE TABLE public.events(id integer PRIMARY KEY) WITH (autovacuum_enabled = false);
      INSERT INTO public.events VALUES (1);
      """
    And the following SQL is executed on "source":
      """
      VACUUM (FREEZE) public.events;
      """
    And the following SQL is executed on "source":
      """
      CHECKPOINT;
      """
    And the following SQL is executed on "source":
      """
      CREATE FUNCTION pg_attach_test_corrupt_page(regclass, integer, integer)
      RETURNS void AS 'pg_attach_bdd_fixtures', 'pg_attach_test_corrupt_page'
      LANGUAGE C STRICT;
      SELECT pg_attach_test_corrupt_page('public.events', 0, 100);
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE TABLE public.events(id integer PRIMARY KEY);
      INSERT INTO public.events VALUES (42);
      """
    Then the following SQL on "source" fails with "checksum":
      """
      BEGIN;
      SELECT pg_attach_lock_source('public.events');
      SELECT dblink_exec('host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres', 'CHECKPOINT');
      SELECT pg_attach_export_stream('public.events', 'public.events', 0::oid, true, true);
      COMMIT;
      """
    And the following SQL on "source" returns "1":
      """
      SELECT count(*) FROM public.events;
      """
    And the following SQL on "target" returns "1|42":
      """
      SELECT count(*), min(id) FROM public.events;
      """
