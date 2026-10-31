@visibility_map @toast
Feature: VM fast path preserves physical transfer across storage modes

  Scenario Outline: External TOAST survives the VM fast path with checksums and WAL modes
    Given a fresh PostgreSQL pair with checksums "<checksums>"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      ALTER DATABASE postgres SET log_min_messages = debug1;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTERNAL;
      INSERT INTO public.events SELECT i, repeat(md5(i::text), 160) FROM generate_series(1, 50) AS i;
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      """
    When the following SQL is executed on "target":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events_copy', <wal>
      );
      """
    Then frozen validation reads 0 data pages across 2 relations
    And the following SQL on "target" returns "50|256000|t":
      """
      SELECT count(*), sum(length(payload)), bool_and(payload = repeat(md5(id::text), 160))
      FROM public.events_copy;
      """
    And the following SQL on "target" returns "t":
      """
      SET enable_seqscan = off;
      SELECT payload = repeat(md5('42'), 160) FROM public.events_copy WHERE id = 42;
      """

    Examples:
      | checksums | wal   |
      | enabled   | true  |
      | enabled   | false |
      | disabled  | true  |
      | disabled  | false |

  Scenario: Automatic freeze cleans dead and aborted versions without resurrecting them
    Given a fresh PostgreSQL pair
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      ALTER DATABASE postgres SET log_min_messages = debug1;
      CREATE TABLE public.events(id integer PRIMARY KEY, payload text, note text);
      ALTER TABLE public.events ALTER COLUMN payload SET STORAGE EXTERNAL;
      INSERT INTO public.events SELECT i, repeat('old', 2000), 'kept' FROM generate_series(1, 4) AS i;
      UPDATE public.events SET payload = repeat('updated', 1000), note = 'updated' WHERE id = 1;
      DELETE FROM public.events WHERE id = 2;
      INSERT INTO public.events VALUES (5, repeat('new', 2000), 'new');
      """
    And the following SQL is executed on "source":
      """
      BEGIN;
      INSERT INTO public.events VALUES (9, repeat('aborted', 1000), 'aborted');
      UPDATE public.events SET payload = 'rolled back' WHERE id = 3;
      DELETE FROM public.events WHERE id = 4;
      ROLLBACK;
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
    Then frozen validation reads 0 data pages across 2 relations
    And the following SQL on "target" returns "1:updated,3:kept,4:kept,5:new|25000":
      """
      SELECT string_agg(id || ':' || note, ',' ORDER BY id), sum(length(payload))
      FROM public.events_copy;
      """
    And the following SQL on "target" returns "t":
      """
      SET enable_seqscan = off;
      SELECT payload = repeat('old', 2000) FROM public.events_copy WHERE id = 3;
      """
