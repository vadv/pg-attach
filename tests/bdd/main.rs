mod postgres;
mod steps;
mod world;

use cucumber::{writer::Stats, World};
use world::AttachWorld;

#[tokio::main]
async fn main() {
    let features = std::env::var("PG_ATTACH_FEATURES").unwrap_or_else(|_| "features".to_string());
    let writer = AttachWorld::cucumber()
        .max_concurrent_scenarios(1)
        .fail_fast()
        .run(features)
        .await;
    if writer.execution_has_failed() {
        std::process::exit(1);
    }
}
