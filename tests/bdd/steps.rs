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
    let checksums = match enabled.as_str() {
        "enabled" => true,
        "disabled" => false,
        _ => panic!("checksums must be enabled or disabled"),
    };
    let pair = tokio::time::timeout(
        Duration::from_secs(60),
        PostgresPair::start_with_checksums(checksums),
    )
    .await
    .expect("PostgreSQL pair startup timed out")
    .unwrap_or_else(|error| panic!("failed to start PostgreSQL pair: {error}"));
    world.pair = Some(pair);
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

#[given("source PostgreSQL is stopped")]
fn stop_source(world: &mut AttachWorld) {
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    pair.stop_source()
        .unwrap_or_else(|error| panic!("failed to stop source PostgreSQL: {error}"));
}

#[when("source is stopped during the following SQL on \"target\":")]
async fn stop_source_during_import(world: &mut AttachWorld, step: &Step) {
    let sql = world.expand_sql(docstring(step));
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    let import = pair.execute("target", &sql);
    tokio::pin!(import);
    tokio::select! {
        result = &mut import => panic!("import finished before source shutdown: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(1)) => {
            pair.stop_source()
                .unwrap_or_else(|error| panic!("failed to stop source PostgreSQL: {error}"));
            let error = import.await.expect_err("import succeeded despite source shutdown");
            eprintln!("source disconnect import error: {error}");
        }
    }
}

#[when("the following SQL on \"target\" is cancelled:")]
async fn cancel_target_import(world: &mut AttachWorld, step: &Step) {
    let sql = world.expand_sql(docstring(step));
    let pair = world.pair.as_ref().expect("PostgreSQL pair is not running");
    pair.cancel_target(&sql, Duration::from_millis(1))
        .await
        .unwrap_or_else(|error| panic!("target cancellation failed: {error}"));
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

fn docstring(step: &Step) -> &str {
    step.docstring
        .as_deref()
        .expect("SQL docstring is required")
}
