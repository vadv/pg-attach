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

#[then(expr = "the following SQL on {string} returns {string}:")]
async fn query_result(world: &mut AttachWorld, side: String, expected: String, step: &Step) {
    let sql = docstring(step);
    let actual = tokio::time::timeout(Duration::from_secs(30), world.query(&side, sql))
        .await
        .expect("SQL query timed out")
        .unwrap_or_else(|error| panic!("query on {side} failed: {error}\nSQL:\n{sql}"));
    assert_eq!(actual, expected, "unexpected query result\nSQL:\n{sql}");
}

fn docstring(step: &Step) -> &str {
    step.docstring
        .as_deref()
        .expect("SQL docstring is required")
}
