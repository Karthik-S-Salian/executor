use anyhow::Result;
use common::model::{Language, NatsSubmission};
use common::nats::NatsConsumer;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::task;
use tokio::time::{Duration, sleep};

use crate::AppState;
use crate::isolate::{ProgramOutput, Sandbox};

pub async fn spawn_workers(app_state: Arc<AppState>, worker_count: usize) -> Result<()> {
    let mut handles = Vec::with_capacity(worker_count);

    for id in 0..worker_count {
        let state = Arc::clone(&app_state);
        let handle = task::spawn(async move {
            let _ = worker_loop(state, id).await;
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await?;
    }

    Ok(())
}

async fn worker_loop(app_state: Arc<AppState>, id: usize) -> Result<()> {
    loop {
        match app_state.queue.pull().await {
            Ok(Some(payload)) => {
                let submission: NatsSubmission = serde_json::from_slice(&payload)?;
                execute_submission(app_state.clone(), submission).await?;
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

/// Writes the source code to the appropriate file inside the box directory.
pub async fn write_source(boxdir: &Path, source_code: &str, lang: &Language) -> Result<()> {
    let path = boxdir.join(&lang.source_file);
    tokio::fs::write(&path, source_code).await?;
    Ok(())
}

/// Writes stdin content to `stdin.txt` in the workdir.
pub async fn write_stdin(workdir: &Path, stdin: &Option<String>) -> Result<()> {
    if let Some(input) = stdin {
        let path = workdir.join("stdin.txt");
        tokio::fs::write(&path, input).await?;
    }
    Ok(())
}

pub async fn execute_submission(app_state: Arc<AppState>, sub: NatsSubmission) -> Result<()> {
    let id = app_state.box_counter.get_next_id().await;
    let iso = Sandbox::new(id)?;
    write_source(&iso.boxdir, &sub.source_code, &sub.language).await?;
    write_stdin(&iso.workdir, &sub.stdin).await?;

    if sub.language.compile_cmd.is_some() {
        if !iso.compile(&sub).await? {
            iso.cleanup().await?;
            return Ok(());
        }
    }

    iso.run(&sub).await?;
    let metadata = iso.read_metadata().await?;
    let output = iso.read_output().await?;

    let status = determine_status(&sub, &output, &metadata);

    println!("{:?}", status);
    println!("{:?}", output);
    println!("{:?}", metadata);

    iso.cleanup().await?;
    Ok(())
}

#[derive(Debug)]
pub enum JudgeStatus {
    Accepted,
    WrongAnswer,
    TimeLimitExceeded,
    RuntimeError,
    SignalError(i32),
    BoxError,
}

pub fn determine_status(
    sub: &NatsSubmission,
    output: &ProgramOutput,
    meta: &HashMap<String, String>,
) -> JudgeStatus {
    match meta.get("status").map(|s| s.as_str()) {
        Some("TO") => JudgeStatus::TimeLimitExceeded,
        Some("SG") => {
            let sig = meta
                .get("exitsig")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            JudgeStatus::SignalError(sig)
        }
        Some("RE") => JudgeStatus::RuntimeError,
        Some("XX") => JudgeStatus::BoxError,
        _ => {
            let expected = sub.expected_output.as_deref().unwrap_or("").trim();
            let actual = output.stdout.as_deref().unwrap_or("").trim();
            if expected == actual {
                JudgeStatus::Accepted
            } else {
                JudgeStatus::WrongAnswer
            }
        }
    }
}
