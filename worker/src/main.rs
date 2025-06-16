use common::nats::{NatsClient, NatsConsumer};
use uuid::Uuid;

use crate::config::load_config;

mod config;

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
    Ok(())
}
