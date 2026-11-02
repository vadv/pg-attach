@cross-major @validation @review-collation
Feature: Complete collation ordering identity is required

  Scenario: Equal collation name and version with a different locale is rejected without changing the target
    Given a fresh PostgreSQL 16 to 17 pair with checksums "disabled"
    And the following SQL is executed on "source":
      """
      CREATE EXTENSION pg_attach;
      CREATE COLLATION public.review_coll (provider = icu, locale = 'de-u-co-phonebk');
      CREATE TABLE public.events(id integer, payload text COLLATE public.review_coll PRIMARY KEY);
      INSERT INTO public.events VALUES (1, 'ä'), (2, 'z');
      ALTER COLLATION public.review_coll REFRESH VERSION;
      """
    Then the following SQL on "source" returns "t":
      """
      SELECT collname = 'review_coll' AND collversion = '153.120.42'
         AND pg_collation_actual_version(oid) = '153.120.42'
         AND collprovider = 'i' AND coalesce(row_to_json(pg_collation)->>'colllocale', row_to_json(pg_collation)->>'colliculocale', collcollate) = 'de-u-co-phonebk'
      FROM pg_collation WHERE oid = 'public.review_coll'::regcollation;
      """
    When the following SQL is executed on "target":
      """
      CREATE EXTENSION pg_attach;
      CREATE COLLATION public.review_coll (provider = icu, locale = 'sv');
      CREATE TABLE public.events(id integer, payload text COLLATE public.review_coll PRIMARY KEY);
      INSERT INTO public.events VALUES (42, 'kept');
      ALTER COLLATION public.review_coll REFRESH VERSION;
      """
    Then the following SQL on "target" returns "t":
      """
      SELECT collname = 'review_coll' AND collversion = '153.120.42'
         AND pg_collation_actual_version(oid) = '153.120.42'
         AND collprovider = 'i' AND coalesce(row_to_json(pg_collation)->>'colllocale', row_to_json(pg_collation)->>'colliculocale', collcollate) = 'sv'
      FROM pg_collation WHERE oid = 'public.review_coll'::regcollation;
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
