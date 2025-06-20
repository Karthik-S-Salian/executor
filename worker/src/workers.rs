use anyhow::Result;
use common::model::NatsSubmission;
use common::nats::NatsConsumer;
use std::sync::Arc;
use tokio::task;
use tokio::time::{Duration, sleep};

use crate::isolate::Sandbox; // Adjust path if needed

pub async fn spawn_workers(worker_count: usize, job_queue: Arc<NatsConsumer>) -> Result<()> {
    let mut handles = Vec::with_capacity(worker_count);

    for id in 0..worker_count {
        let queue = Arc::clone(&job_queue);
        let handle = task::spawn(async move {
            worker_loop(id, queue).await;
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    Ok(())
}

async fn worker_loop(id: usize, queue: Arc<NatsConsumer>) -> Result<()> {
    loop {
        match queue.pull().await {
            Ok(Some(payload)) => {
                let submission: NatsSubmission = serde_json::from_slice(&payload)?;
                execute_submission(submission);
            }
            Ok(None) => {
                println!("🧵 Worker #{id} no job yet");
                sleep(Duration::from_millis(100)).await;
            }
            Err(err) => {
                eprintln!("❌ Worker #{id} error: {err}");
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

pub async fn execute_submission(sub: NatsSubmission) -> Result<()> {
    let iso = Sandbox::new(sub.id)?;

    io_utils::write_source(&iso.boxdir, &sub.source_code, &sub.language).await?;
    io_utils::write_stdin(&iso.workdir, &sub.stdin).await?;

    if sub.language.compile_cmd.is_some() {
        if !iso.compile(&sub).await? {
            iso.cleanup().await?;
            return Ok(());
        }
    }

    iso.run(&sub).await?;
    let metadata = iso.read_metadata().await?;
    let output = io_utils::read_output(&iso).await?;

    let status = verify::determine_status(&sub, &output, &metadata);

    iso.cleanup().await?;
    Ok(())
}
