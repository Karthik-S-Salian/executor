use std::sync::Arc;

use common::nats::{NatsClient, NatsConsumer};
use tokio::sync::Mutex;

use crate::{config::load_config, workers::spawn_workers};

mod config;
mod isolate;
mod workers;

pub struct BoxIdManager {
    next: Mutex<u32>,
}

impl BoxIdManager {
    pub fn new() -> Self {
        Self {
            next: Mutex::new(0),
        }
    }

    pub async fn get_next_id(&self) -> u32 {
        let mut lock = self.next.lock().await;
        let id = *lock;
        *lock = lock.wrapping_add(1);
        id
    }
}

struct AppState {
    box_counter: BoxIdManager,
    queue:NatsConsumer
}

impl AppState {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config();
    let client = NatsClient::new("localhost").await?;
    let queue = NatsConsumer::new(
        &client,
        "SUBMISSIONS",
        "submission.new",
        "submissions",
    )
    .await?;

    let app_state = Arc::new(AppState {
        box_counter: BoxIdManager::new(),
        queue
    });

    spawn_workers(app_state, config.num_workers as usize).await?;
    Ok(())
}
