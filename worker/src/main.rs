use std::sync::Arc;

use common::nats::{NatsClient, NatsConsumer};
use uuid::Uuid;

use crate::{config::load_config, workers::spawn_workers};

mod config;
mod workers;
mod isolate;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config();
    let client = NatsClient::new("localhost").await?;
    let queue = NatsConsumer::new(
        &client,
        "SUBMISSIONS",
        "submission.new",
        &Uuid::new_v4().to_string(),
    )
    .await?;

    let queue = Arc::new(queue);

    spawn_workers(config.num_workers as usize, queue).await?;
    Ok(())
}
