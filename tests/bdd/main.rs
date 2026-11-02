mod postgres;
mod steps;
mod world;

use cucumber::{writer::Stats as _, World as _};
use world::AttachWorld;

#[tokio::main]
async fn main() {
    let features = std::env::var("PG_ATTACH_FEATURES").unwrap_or_else(|_| "features".to_string());
    let writer = AttachWorld::cucumber()
        .max_concurrent_scenarios(1)
        .fail_fast()
        .run(features)
        .await;
    let scenarios = writer.scenarios_stats();
    let passed = scenarios.passed;
    let skipped = scenarios.skipped;
    let failed = scenarios.failed;
    println!("BDD summary: {passed} passed, {skipped} skipped, {failed} failed");
    if passed + skipped + failed == 0 {
        eprintln!("no BDD scenarios matched; check PG_ATTACH_FEATURES");
        std::process::exit(1);
    }
    if writer.execution_has_failed() {
        std::process::exit(1);
    }
}
