use std::sync::Arc;

use common::db::Db;
use common::nats::{NatsClient, NatsConsumer, SUBMISSION_STREAM, SUBMISSION_SUBJECT};
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
        // Random start so restarts do not reuse a box id a crashed run still holds.
        // isolate allows box ids 0..max_boxes (default 1000).
        let seed = (uuid::Uuid::new_v4().as_u128() % 1000) as u32;
        Self {
            next: Mutex::new(seed),
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
    queue: NatsConsumer,
    db: Db,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL not found");
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());

    let client = NatsClient::new(&nats_url).await?;
    let queue = NatsConsumer::new(
        &client,
        SUBMISSION_STREAM,
        SUBMISSION_SUBJECT,
        "submissions",
    )
    .await?;

    let app_state = Arc::new(AppState {
        box_counter: BoxIdManager::new(),
        queue,
        db: Db::init(&database_url).await.expect("could not init db"),
    });

    spawn_workers(app_state, config.num_workers as usize).await?;
    Ok(())
}
