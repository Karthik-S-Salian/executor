use anyhow::Result;
use common::db::Db;
use common::model::{NatsSubmission, SubmissionStatus};
use std::collections::HashMap;
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
            Ok(Some(job)) => {
                let parsed = serde_json::from_slice::<NatsSubmission>(job.payload());
                match parsed {
                    Ok(submission) => match execute_submission(&app_state, &submission).await {
                        Ok(()) => {
                            if let Err(e) = job.ack().await {
                                eprintln!("❌ Worker #{id} ack failed: {e}");
                            }
                        }
                        Err(e) => {
                            eprintln!("❌ Worker #{id} job {} failed: {e}", submission.id);
                            let _ = job.nak().await;
                        }
                    },
                    Err(e) => {
                        // Poison message: redelivering it forever helps nobody.
                        eprintln!("❌ Worker #{id} dropping unparsable message: {e}");
                        let _ = job.ack().await;
                    }
                }
            }
            Ok(None) => {
                sleep(Duration::from_millis(100)).await;
            }
            Err(err) => {
                eprintln!("❌ Worker #{id} error: {err}");
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Claims the row atomically. Duplicate deliveries (requeue sweeper, redelivery)
/// are dropped here instead of executing twice.
async fn claim(db: &Db, id: uuid::Uuid) -> Result<bool> {
    let updated = db
        .execute(
            "UPDATE submissions SET status = $2, updated_at = now() \
             WHERE id = $1 AND status = 'inqueue'",
            &[&id, &SubmissionStatus::Processing],
        )
        .await?;
    Ok(updated > 0)
}

pub async fn execute_submission(state: &AppState, sub: &NatsSubmission) -> Result<()> {
    let id = uuid::Uuid::parse_str(&sub.id)?;
    if !claim(&state.db, id).await? {
        return Ok(());
    }

    let outcome = match run_in_sandbox(state, sub).await {
        Ok(outcome) => outcome,
        Err(e) => JobOutcome::internal(format!("sandbox error: {e}")),
    };

    persist(&state.db, id, outcome).await
}

async fn run_in_sandbox(state: &AppState, sub: &NatsSubmission) -> Result<JobOutcome> {
    let box_id = state.box_counter.get_next_id().await;
    let iso = Sandbox::new(box_id).await?;

    let outcome = execute_in_sandbox(&iso, sub).await;
    iso.cleanup().await;
    outcome
}

async fn execute_in_sandbox(iso: &Sandbox, sub: &NatsSubmission) -> Result<JobOutcome> {
    tokio::fs::write(iso.boxdir.join(&sub.language.source_file), &sub.source_code).await?;
    tokio::fs::write(&iso.stdin_file, sub.stdin.as_deref().unwrap_or("")).await?;

    let mut compile_output = None;
    if sub.language.compile_cmd.is_some() {
        let compiled = iso.compile(sub).await?;
        if !compiled.success {
            let status = if compiled.timed_out {
                SubmissionStatus::TimeLimitExceeded
            } else {
                SubmissionStatus::CompilationError
            };
            let mut outcome = JobOutcome::new(status);
            outcome.compile_output = compiled.output;
            outcome.message = compiled.message;
            return Ok(outcome);
        }
        compile_output = compiled.output;
    }

    let runs = sub.number_of_runs.unwrap_or(1).clamp(1, 10);
    let mut status = SubmissionStatus::Accepted;
    let mut last = None;
    let mut total_time = 0.0;
    let mut total_wall = 0.0;
    let mut total_memory = 0.0;
    let mut completed = 0u32;

    for _ in 0..runs {
        iso.run(sub).await?;
        let metadata = iso.read_metadata().await?;
        let output = iso.read_output().await?;
        let run_status = determine_status(sub, &output, &metadata);

        total_time += parse_meta(&metadata, "time").unwrap_or(0.0);
        total_wall += parse_meta(&metadata, "time-wall").unwrap_or(0.0);
        total_memory += parse_meta(&metadata, "max-rss").unwrap_or(0.0);
        completed += 1;

        let failed = run_status != SubmissionStatus::Accepted;
        last = Some((output, metadata));
        if failed {
            status = run_status;
            break;
        }
    }

    // `runs >= 1`, so a run always completed here.
    let (output, metadata) = last.expect("at least one run");
    let divisor = f64::from(completed);

    let mut outcome = JobOutcome::new(status);
    outcome.stdout = output.stdout;
    outcome.stderr = output.stderr;
    outcome.compile_output = compile_output;
    outcome.message = metadata.get("message").cloned();
    outcome.exit_code = parse_meta(&metadata, "exitcode");
    outcome.exit_signal = parse_meta(&metadata, "exitsig");
    outcome.time = (completed > 0).then(|| total_time / divisor);
    outcome.wall_time = (completed > 0).then(|| total_wall / divisor);
    outcome.memory = (completed > 0).then(|| total_memory / divisor);

    Ok(outcome)
}

async fn persist(db: &Db, id: uuid::Uuid, outcome: JobOutcome) -> Result<()> {
    db.execute(
        "UPDATE submissions SET
            status = $2,
            stdout = $3,
            stderr = $4,
            compile_output = $5,
            message = $6,
            exit_code = $7,
            exit_signal = $8,
            time = $9,
            wall_time = $10,
            memory = $11,
            updated_at = now(),
            finished_at = now()
         WHERE id = $1",
        &[
            &id,
            &outcome.status,
            &outcome.stdout,
            &outcome.stderr,
            &outcome.compile_output,
            &outcome.message,
            &outcome.exit_code,
            &outcome.exit_signal,
            &outcome.time,
            &outcome.wall_time,
            &outcome.memory,
        ],
    )
    .await?;
    Ok(())
}

fn parse_meta<T: std::str::FromStr>(meta: &HashMap<String, String>, key: &str) -> Option<T> {
    meta.get(key).and_then(|v| v.trim().parse().ok())
}

pub fn determine_status(
    sub: &NatsSubmission,
    output: &ProgramOutput,
    meta: &HashMap<String, String>,
) -> SubmissionStatus {
    use SubmissionStatus::*;

    match meta.get("status").map(String::as_str) {
        Some("TO") => TimeLimitExceeded,
        Some("SG") => signal_status(
            meta.get("exitsig")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        ),
        Some("RE") => RuntimeErrorNzec,
        Some("XX") => InternalError,
        _ => {
            let Some(expected) = sub.expected_output.as_deref() else {
                return Accepted;
            };
            let actual = output.stdout.as_deref().unwrap_or("");
            if normalize(expected) == normalize(actual) {
                Accepted
            } else {
                WrongAnswer
            }
        }
    }
}

fn signal_status(sig: i32) -> SubmissionStatus {
    use SubmissionStatus::*;

    match sig {
        11 => RuntimeErrorSigsegv,
        25 => RuntimeErrorSigxfsz,
        8 => RuntimeErrorSigfpe,
        6 => RuntimeErrorSigabrt,
        _ => RuntimeErrorOther,
    }
}

/// Judging convention: ignore trailing whitespace per line and trailing newlines.
fn normalize(s: &str) -> String {
    s.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

struct JobOutcome {
    status: SubmissionStatus,
    stdout: Option<String>,
    stderr: Option<String>,
    compile_output: Option<String>,
    message: Option<String>,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    time: Option<f64>,
    wall_time: Option<f64>,
    memory: Option<f64>,
}

impl JobOutcome {
    fn new(status: SubmissionStatus) -> Self {
        Self {
            status,
            stdout: None,
            stderr: None,
            compile_output: None,
            message: None,
            exit_code: None,
            exit_signal: None,
            time: None,
            wall_time: None,
            memory: None,
        }
    }

    fn internal(message: String) -> Self {
        let mut outcome = Self::new(SubmissionStatus::InternalError);
        outcome.message = Some(message);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::model::Language;

    fn submission(expected: Option<&str>) -> NatsSubmission {
        NatsSubmission {
            id: "test".into(),
            source_code: String::new(),
            language: Language {
                name: "c".into(),
                source_file: "main.c".into(),
                file_extension: "c".into(),
                compile_cmd: None,
                run_cmd: "./main".into(),
                allow_network: false,
            },
            compiler_options: None,
            command_line_arguments: None,
            stdin: None,
            expected_output: expected.map(str::to_string),
            cpu_time_limit: None,
            cpu_extra_time: None,
            wall_time_limit: None,
            memory_limit: None,
            stack_limit: None,
            max_processes_and_or_threads: None,
            enable_per_process_and_thread_time_limit: None,
            enable_per_process_and_thread_memory_limit: None,
            max_file_size: None,
            redirect_stderr_to_stdout: None,
            enable_network: None,
            number_of_runs: None,
            additional_files: None,
            callback_url: None,
        }
    }

    fn metadata(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn output(stdout: &str) -> ProgramOutput {
        ProgramOutput {
            stdout: Some(stdout.to_string()),
            stderr: None,
        }
    }

    #[test]
    fn accepts_matching_output_with_trailing_newline() {
        let sub = submission(Some("hello\n"));
        let status = determine_status(&sub, &output("hello\n\n"), &metadata(&[]));
        assert_eq!(status, SubmissionStatus::Accepted);
    }

    #[test]
    fn rejects_mismatched_output() {
        let sub = submission(Some("hello"));
        let status = determine_status(&sub, &output("world"), &metadata(&[]));
        assert_eq!(status, SubmissionStatus::WrongAnswer);
    }

    #[test]
    fn no_expected_output_means_no_check() {
        let sub = submission(None);
        let status = determine_status(&sub, &output("anything"), &metadata(&[]));
        assert_eq!(status, SubmissionStatus::Accepted);
    }

    #[test]
    fn maps_isolate_statuses() {
        assert_eq!(
            determine_status(
                &submission(Some("")),
                &output(""),
                &metadata(&[("status", "TO")])
            ),
            SubmissionStatus::TimeLimitExceeded
        );
        assert_eq!(
            determine_status(
                &submission(None),
                &output(""),
                &metadata(&[("status", "XX")])
            ),
            SubmissionStatus::InternalError
        );
        assert_eq!(
            determine_status(
                &submission(None),
                &output(""),
                &metadata(&[("status", "SG"), ("exitsig", "11")])
            ),
            SubmissionStatus::RuntimeErrorSigsegv
        );
    }

    #[test]
    fn maps_every_supported_signal() {
        assert_eq!(signal_status(25), SubmissionStatus::RuntimeErrorSigxfsz);
        assert_eq!(signal_status(8), SubmissionStatus::RuntimeErrorSigfpe);
        assert_eq!(signal_status(6), SubmissionStatus::RuntimeErrorSigabrt);
        assert_eq!(signal_status(9), SubmissionStatus::RuntimeErrorOther);
    }
}
