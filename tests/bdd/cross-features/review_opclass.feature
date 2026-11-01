@cross-major @validation @review-opclass
Feature: Custom btree opclasses are rejected

  Scenario: A same-named custom opclass with reversed comparison semantics is rejected without changing the target
    Given a fresh PostgreSQL 16 to 17 pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE OPERATOR CLASS public.review_ops FOR TYPE integer USING btree AS
        OPERATOR 1 < (integer, integer),
        OPERATOR 2 <= (integer, integer),
        OPERATOR 3 = (integer, integer),
        OPERATOR 4 >= (integer, integer),
        OPERATOR 5 > (integer, integer),
        FUNCTION 1 btint4cmp(integer, integer);
      CREATE TABLE public.events(id integer, payload integer);
      CREATE INDEX events_payload_idx ON public.events(payload public.review_ops);
      INSERT INTO public.events VALUES (1, 1), (2, 2), (3, 3);
      """
    And the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE FUNCTION public.reverse_int4_cmp(integer, integer)
      RETURNS integer LANGUAGE sql IMMUTABLE STRICT AS $$
        SELECT -btint4cmp($1, $2);
      $$;
      CREATE OPERATOR public.< (LEFTARG = integer, RIGHTARG = integer, FUNCTION = int4gt);
      CREATE OPERATOR public.<= (LEFTARG = integer, RIGHTARG = integer, FUNCTION = int4ge);
      CREATE OPERATOR public.= (LEFTARG = integer, RIGHTARG = integer, FUNCTION = int4eq);
      CREATE OPERATOR public.>= (LEFTARG = integer, RIGHTARG = integer, FUNCTION = int4le);
      CREATE OPERATOR public.> (LEFTARG = integer, RIGHTARG = integer, FUNCTION = int4lt);
      CREATE OPERATOR CLASS public.review_ops FOR TYPE integer USING btree AS
        OPERATOR 1 < (integer, integer),
        OPERATOR 2 <= (integer, integer),
        OPERATOR 3 = (integer, integer),
        OPERATOR 4 >= (integer, integer),
        OPERATOR 5 > (integer, integer),
        FUNCTION 1 public.reverse_int4_cmp(integer, integer);
      CREATE TABLE public.events(id integer, payload integer);
      CREATE INDEX events_payload_idx ON public.events(payload public.review_ops);
      INSERT INTO public.events VALUES (42, 42);
      """
    Then the following SQL on "target" fails with "custom opclass":
      """
      SELECT pg_attach_import(
        'host=127.0.0.1 port=${SOURCE_PORT} dbname=postgres user=postgres',
        'public.events', 'public.events', true
      );
      """
    And the following SQL on "target" returns "1|42|42":
      """
      SELECT count(*), min(id), min(payload) FROM public.events;
      """
