use crate::postgres::PostgresPair;
use crate::world::AttachWorld;
use cucumber::{gherkin::Step, given, then, when};
use std::time::Duration;

#[given("a fresh PostgreSQL pair")]
async fn fresh_pair(world: &mut AttachWorld) {
    let pair = tokio::time::timeout(Duration::from_secs(60), PostgresPair::start())
        .await
        .expect("PostgreSQL pair startup timed out")
        .unwrap_or_else(|error| panic!("failed to start PostgreSQL pair: {error}"));
    world.pair = Some(pair);
}

#[given(expr = "a fresh PostgreSQL pair with checksums {string}")]
async fn fresh_pair_with_checksums(world: &mut AttachWorld, enabled: String) {
    let pair = tokio::time::timeout(
        Duration::from_secs(60),
        PostgresPair::start_with_checksums(checksum_setting(&enabled)),
    )
    .await
    .expect("PostgreSQL pair startup timed out")
    .unwrap_or_else(|error| panic!("failed to start PostgreSQL pair: {error}"));
    world.pair = Some(pair);
}

#[given(expr = "a fresh PostgreSQL {int} to {int} pair with checksums {string}")]
async fn fresh_cross_pair(world: &mut AttachWorld, source: u16, target: u16, enabled: String) {
    let pair = tokio::time::timeout(
        Duration::from_secs(60),
        PostgresPair::start_versions(source, target, checksum_setting(&enabled)),
    )
    .await
    .expect("PostgreSQL pair startup timed out")
    .unwrap_or_else(|error| panic!("failed to start PostgreSQL pair: {error}"));
    world.pair = Some(pair);
}

#[given(
    expr = "a fresh PostgreSQL {int} to {int} pair with source checksums {string} and target checksums {string}"
)]
async fn fresh_cross_pair_checksum_mismatch(
    world: &mut AttachWorld,
    source: u16,
    target: u16,
    source_enabled: String,
    target_enabled: String,
) {
    let pair = tokio::time::timeout(
        Duration::from_secs(60),
        PostgresPair::start_with_options(
            source,
            target,
            checksum_setting(&source_enabled),
            checksum_setting(&target_enabled),
        ),
    )
    .await
    .expect("PostgreSQL pair startup timed out")
    .unwrap_or_else(|error| panic!("failed to start PostgreSQL pair: {error}"));
    world.pair = Some(pair);
}

#[given(
    expr = "a fresh PostgreSQL {int} to {int} pair with default ICU locales {string} and {string}"
)]
async fn fresh_cross_pair_icu_locales(
    world: &mut AttachWorld,
    source: u16,
    target: u16,
    source_locale: String,
    target_locale: String,
) {
    let pair = tokio::time::timeout(
        Duration::from_secs(60),
        PostgresPair::start_with_icu_locales(source, target, &source_locale, &target_locale),
    )
    .await
    .expect("PostgreSQL pair startup timed out")
    .unwrap_or_else(|error| panic!("failed to start PostgreSQL pair: {error}"));
    world.pair = Some(pair);
}

fn checksum_setting(enabled: &str) -> bool {
    match enabled {
        "enabled" => true,
        "disabled" => false,
        _ => panic!("checksums must be enabled or disabled"),
    }
}

#[given(expr = "the following SQL is executed on {string}:")]
async fn given_sql(world: &mut AttachWorld, side: String, step: &Step) {
    execute_sql(world, &side, step).await;
}

#[when(expr = "the following SQL is executed on {string}:")]
async fn when_sql(world: &mut AttachWorld, side: String, step: &Step) {
    execute_sql(world, &side, step).await;
}

async fn execute_sql(world: &AttachWorld, side: &str, step: &Step) {
    let sql = docstring(step);
    tokio::time::timeout(Duration::from_secs(30), world.execute(side, sql))
        .await
        .expect("SQL execution timed out")
        .unwrap_or_else(|error| panic!("SQL on {side} failed: {error}\nSQL:\n{sql}"));
}

#[given(expr = "a non-default tablespace is available on {string}")]
async fn create_tablespace(world: &mut AttachWorld, side: String) {
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    tokio::time::timeout(Duration::from_secs(30), pair.create_tablespace(&side))
        .await
        .expect("tablespace creation timed out")
        .unwrap_or_else(|error| panic!("failed to create tablespace: {error}"));
}

#[when(expr = "the data file path of target relation {string} is recorded")]
async fn record_target_data_file(world: &mut AttachWorld, relation: String) {
    let sql = format!("SELECT pg_relation_filepath('{relation}')");
    let relative = tokio::time::timeout(Duration::from_secs(30), world.query("target", &sql))
        .await
        .expect("SQL query timed out")
        .unwrap_or_else(|error| panic!("query on target failed: {error}\nSQL:\n{sql}"));
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not started");
    world.recorded_data_file = Some(pair.target_data_path(&relative));
}

#[when("target postmaster is crashed")]
fn crash_target_without_restart(world: &mut AttachWorld) {
    world
        .pair
        .as_mut()
        .expect("PostgreSQL pair is not started")
        .crash_target_only()
        .expect("failed to crash target postmaster");
}

#[when("the recorded target data file is removed")]
fn remove_recorded_data_file(world: &mut AttachWorld) {
    let path = world
        .recorded_data_file
        .clone()
        .expect("no recorded target data file");
    assert!(
        path.exists(),
        "recorded data file {} does not exist",
        path.display()
    );
    std::fs::remove_file(&path)
        .unwrap_or_else(|error| panic!("failed to remove {}: {error}", path.display()));
    let stem = path
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned();
    for segment in 1.. {
        let sibling = path.with_file_name(format!("{stem}_{segment}"));
        if !sibling.exists() {
            break;
        }
        std::fs::remove_file(&sibling)
            .unwrap_or_else(|error| panic!("failed to remove {}: {error}", sibling.display()));
    }
}

#[when("target postmaster is started")]
async fn start_target_postmaster(world: &mut AttachWorld) {
    let start = world
        .pair
        .as_mut()
        .expect("PostgreSQL pair is not started")
        .start_target();
    tokio::time::timeout(Duration::from_secs(60), start)
        .await
        .expect("target postmaster start timed out")
        .unwrap_or_else(|error| panic!("failed to start target: {error}"));
}

#[given("source PostgreSQL is stopped")]
fn stop_source(world: &mut AttachWorld) {
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    pair.stop_source()
        .unwrap_or_else(|error| panic!("failed to stop source PostgreSQL: {error}"));
}

#[when("target postmaster is crashed and restarted")]
async fn crash_target(world: &mut AttachWorld) {
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    pair.crash_target()
        .await
        .unwrap_or_else(|error| panic!("failed to crash and restart target: {error}"));
}

#[when("source is stopped during the following SQL on \"target\":")]
async fn stop_source_during_import(world: &mut AttachWorld, step: &Step) {
    let sql = world.expand_sql(docstring(step));
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    let import = pair.execute("target", &sql);
    tokio::pin!(import);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "source COPY did not start"
        );
        let check = pair.query(
            "source",
            "SELECT count(*)::text FROM pg_stat_activity \
             WHERE state = 'active' AND query LIKE 'COPY (SELECT pg_attach_export_stream(%'",
        );
        tokio::pin!(check);
        let result = tokio::select! {
            result = &mut import => panic!("import finished before source shutdown: {result:?}"),
            result = &mut check => result.unwrap_or_else(|error| panic!("COPY query failed: {error}")),
        };
        if result == "1" {
            break;
        }
    }
    pair.stop_source()
        .unwrap_or_else(|error| panic!("failed to stop source PostgreSQL: {error}"));
    let error = import
        .await
        .expect_err("import succeeded despite source shutdown");
    eprintln!("source disconnect import error: {error}");
}

#[when("a source writer is blocked during the following SQL on \"target\":")]
async fn source_writer_is_blocked(world: &mut AttachWorld, step: &Step) {
    let sql = world.expand_sql(docstring(step));
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    let import = pair.execute("target", &sql);
    tokio::pin!(import);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "source import did not acquire AccessExclusiveLock"
        );
        let check = pair.query(
            "source",
            "SELECT count(*)::text FROM pg_locks l JOIN pg_class c ON c.oid = l.relation JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND c.relname = 'events' AND l.mode = 'AccessExclusiveLock' AND l.granted",
        );
        tokio::pin!(check);
        let result = tokio::select! {
            result = &mut import => panic!("import finished before lock was observed: {result:?}"),
            result = &mut check => result.unwrap_or_else(|error| panic!("lock query failed: {error}")),
        };
        if result == "1" {
            break;
        }
    }
    let writer = pair.execute(
        "source",
        "INSERT INTO public.events VALUES (20001, encode(gen_random_bytes(1000), 'hex'), 'writer')",
    );
    tokio::pin!(writer);
    tokio::select! {
        result = &mut import => panic!("import finished before writer block check: {result:?}"),
        result = &mut writer => panic!("source writer was not blocked: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(50)) => {}
    }
    import
        .await
        .unwrap_or_else(|error| panic!("direct import failed: {error}"));
    writer
        .await
        .unwrap_or_else(|error| panic!("blocked source writer failed: {error}"));
}

#[when("the following SQL on \"target\" is cancelled:")]
async fn cancel_target_import(world: &mut AttachWorld, step: &Step) {
    let sql = world.expand_sql(docstring(step));
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    pair.cancel_target(&sql, Duration::from_millis(1))
        .await
        .unwrap_or_else(|error| panic!("target cancellation failed: {error}"));
}

#[when(expr = "the following SQL on {string} is interrupted by timeout:")]
async fn timeout_sql(world: &mut AttachWorld, side: String, step: &Step) {
    let sql = world.expand_sql(docstring(step));
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    tokio::time::timeout(Duration::from_millis(50), pair.execute(&side, &sql))
        .await
        .expect_err("SQL was not interrupted by timeout");
}

#[then(expr = "the following SQL on {string} returns {string}:")]
async fn query_result(world: &mut AttachWorld, side: String, expected: String, step: &Step) {
    let sql = docstring(step);
    let actual = tokio::time::timeout(Duration::from_secs(30), world.query(&side, sql))
        .await
        .expect("SQL query timed out")
        .unwrap_or_else(|error| panic!("query on {side} failed: {error}\nSQL:\n{sql}"));
    assert_eq!(actual, expected, "unexpected query result\nSQL:\n{sql}");
}

#[then(expr = "the following SQL on {string} contains {string}:")]
async fn query_result_contains(
    world: &mut AttachWorld,
    side: String,
    expected: String,
    step: &Step,
) {
    let sql = docstring(step);
    let actual = tokio::time::timeout(Duration::from_secs(30), world.query(&side, sql))
        .await
        .expect("SQL query timed out")
        .unwrap_or_else(|error| panic!("query on {side} failed: {error}\nSQL:\n{sql}"));
    assert!(
        actual.contains(&expected),
        "query result does not contain {expected:?}: {actual}\nSQL:\n{sql}"
    );
}

#[then(expr = "the following SQL on {string} fails with {string}:")]
async fn query_error(world: &mut AttachWorld, side: String, expected: String, step: &Step) {
    let sql = docstring(step);
    let error = tokio::time::timeout(Duration::from_secs(30), world.execute(&side, sql))
        .await
        .expect("SQL execution timed out")
        .expect_err("SQL unexpectedly succeeded");
    assert!(
        error.contains(&expected),
        "unexpected SQL error: {error}\nexpected substring: {expected}\nSQL:\n{sql}"
    );
}

#[then(expr = "frozen validation reads {int} data pages across {int} relations")]
fn validation_reads(world: &mut AttachWorld, expected_reads: u32, expected_relations: usize) {
    let log = world
        .pair
        .as_ref()
        .expect("PostgreSQL pair is not running")
        .source_log()
        .expect("cannot read source log");
    let reads: Vec<u32> = log
        .lines()
        .filter(|line| line.contains("pg_attach: frozen validation "))
        .map(|line| {
            line.rsplit_once("data pages read=")
                .expect("missing validation read count")
                .1
                .parse()
                .expect("invalid validation read count")
        })
        .collect();
    assert_eq!(
        reads.len(),
        expected_relations,
        "missing validation diagnostics"
    );
    assert_eq!(reads.iter().sum::<u32>(), expected_reads, "{reads:?}");
}

#[then("source PostgreSQL did not crash")]
fn source_did_not_crash(world: &mut AttachWorld) {
    let log = world
        .pair
        .as_ref()
        .expect("PostgreSQL pair is not running")
        .source_log()
        .expect("cannot read source log");
    assert!(
        !log.contains("terminated by signal")
            && !log.contains("non-unwinding panic")
            && !log.contains("reinitializing")
            && !log.contains("automatic recovery in progress"),
        "source backend crashed:\n{log}"
    );
}

fn docstring(step: &Step) -> &str {
    step.docstring
        .as_deref()
        .expect("SQL docstring is required")
}
