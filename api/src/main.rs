use std::{sync::Arc, time::Duration};

use common::{
    db,
    error::StringError,
    model::{Language, NatsSubmission, NewSubmission, Submission, SubmissionStatus},
    nats::{self, NatsClient},
};
use poem::{
    EndpointExt, Route, Server,
    error::{BadRequest, InternalServerError},
    listener::TcpListener,
    middleware::Cors,
    web::{Data, Path},
};
use poem_openapi::{
    ApiResponse, Object, OpenApi, OpenApiService,
    payload::{Json, PlainText},
};
use tokio::time::sleep;
use tokio_postgres::types::ToSql;

use crate::config::{AppConfig, load_config};

mod config;

/// How long an `inqueue` row may sit before we assume its NATS message was lost.
const REQUEUE_AFTER_SECS: i64 = 60;

#[derive(ApiResponse)]
enum SubmissionGetResponse {
    #[oai(status = 200)]
    Submission(Json<Submission>),

    #[oai(status = 404)]
    NotFound(PlainText<String>),
}

#[derive(Object)]
struct SubmissionResponse {
    id: String,
}

struct Api;

#[OpenApi]
impl Api {
    #[oai(path = "/submissions/new", method = "post")]
    async fn create_submission(
        &self,
        data: Data<&Arc<AppData>>,
        new_submission: Json<NewSubmission>,
    ) -> Result<Json<SubmissionResponse>, poem::Error> {
        let language = data
            .config
            .get_language(&new_submission.language)
            .ok_or(BadRequest(StringError::new("unrecognised language")))?;

        let mut new_submission = new_submission.0;
        data.config.apply_defaults(&mut new_submission);
        data.config
            .validate(&new_submission, &language)
            .map_err(|msg| BadRequest(StringError::new(&msg)))?;

        let params: &[&(dyn ToSql + Sync)] = &[
            &new_submission.source_code,
            &language.name,
            &new_submission.compiler_options,
            &new_submission.command_line_arguments,
            &new_submission.stdin,
            &new_submission.expected_output,
            &new_submission.cpu_time_limit,
            &new_submission.cpu_extra_time,
            &new_submission.wall_time_limit,
            &new_submission.memory_limit,
            &new_submission.stack_limit,
            &new_submission.max_processes_and_or_threads,
            &new_submission.enable_per_process_and_thread_time_limit,
            &new_submission.enable_per_process_and_thread_memory_limit,
            &new_submission.max_file_size,
            &new_submission.redirect_stderr_to_stdout,
            &new_submission.enable_network,
            &new_submission.number_of_runs,
            &new_submission.additional_files,
            &new_submission.callback_url,
            &SubmissionStatus::InQueue,
        ];

        let row = data
            .db
            .query_one(
                r#"
        INSERT INTO submissions (
            source_code,
            language,
            compiler_options,
            command_line_arguments,
            stdin,
            expected_output,
            cpu_time_limit,
            cpu_extra_time,
            wall_time_limit,
            memory_limit,
            stack_limit,
            max_processes_and_or_threads,
            enable_per_process_and_thread_time_limit,
            enable_per_process_and_thread_memory_limit,
            max_file_size,
            redirect_stderr_to_stdout,
            enable_network,
            number_of_runs,
            additional_files,
            callback_url,
            status
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18, $19, $20,$21
        )

        RETURNING id
        "#,
                params,
            )
            .await
            .map_err(|e| {
                InternalServerError(StringError::new(&format!("DB insert failed: {}", e)))
            })?;

        use uuid::Uuid;

        let id: Uuid = row.get("id");
        let id_str = id.to_string();

        let submission = NatsSubmission::from((id_str.clone(), new_submission, language));
        let payload = serde_json::to_vec(&submission)
            .map_err(|_| InternalServerError(StringError::new("could not serialize submission")))?;

        if let Err(e) = publish_job(&data.nats, &payload).await {
            let message = format!("could not enqueue submission: {}", e);
            let _ = data
                .db
                .execute(
                    "UPDATE submissions SET status = $2, message = $3, updated_at = now(), finished_at = now() WHERE id = $1",
                    &[&id, &SubmissionStatus::InternalError, &message],
                )
                .await;
            return Err(InternalServerError(StringError::new(&message)));
        }

        Ok(Json(SubmissionResponse { id: id_str }))
    }

    #[oai(path = "/submissions/:id", method = "get")]
    async fn get_submission(
        &self,
        data: Data<&Arc<AppData>>,
        id: Path<String>,
    ) -> Result<SubmissionGetResponse, poem::Error> {
        let Ok(uuid) = uuid::Uuid::parse_str(&id.0) else {
            return Ok(SubmissionGetResponse::NotFound(PlainText(format!(
                "Submission `{}` not found",
                id.0
            ))));
        };
        let params: &[&(dyn ToSql + Sync)] = &[&uuid];
        let row = data
            .db
            .query_opt("select * from submissions where id= $1", params)
            .await
            .map_err(|_| InternalServerError(StringError::new("could not able to query")))?;

        match row {
            Some(submission) => Ok(SubmissionGetResponse::Submission(Json(submission.into()))),
            None => Ok(SubmissionGetResponse::NotFound(PlainText(format!(
                "Submission `{}` not found",
                id.0
            )))),
        }
    }

    #[oai(path = "/languages/", method = "get")]
    async fn languages(&self, data: Data<&Arc<AppData>>) -> Result<Json<Vec<String>>, poem::Error> {
        Ok(Json(data.config.get_language_names()))
    }
}

struct AppData {
    db: db::Db,
    config: AppConfig,
    nats: NatsClient,
}

async fn publish_job(nats: &NatsClient, payload: &[u8]) -> anyhow::Result<()> {
    let mut last_err = None;
    for attempt in 0..3 {
        match nats
            .publish(nats::SUBMISSION_SUBJECT.to_string(), payload.to_vec())
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                sleep(Duration::from_millis(200 * (attempt + 1))).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("publish failed")))
}

fn to_nats(sub: Submission, language: Language) -> NatsSubmission {
    NatsSubmission {
        id: sub.id,
        source_code: sub.source_code,
        language,
        compiler_options: sub.compiler_options,
        command_line_arguments: sub.command_line_arguments,
        stdin: sub.stdin,
        expected_output: sub.expected_output,
        cpu_time_limit: sub.cpu_time_limit,
        cpu_extra_time: sub.cpu_extra_time,
        wall_time_limit: sub.wall_time_limit,
        memory_limit: sub.memory_limit,
        stack_limit: sub.stack_limit,
        max_processes_and_or_threads: sub.max_processes_and_or_threads,
        enable_per_process_and_thread_time_limit: sub.enable_per_process_and_thread_time_limit,
        enable_per_process_and_thread_memory_limit: sub.enable_per_process_and_thread_memory_limit,
        max_file_size: sub.max_file_size,
        redirect_stderr_to_stdout: sub.redirect_stderr_to_stdout,
        enable_network: sub.enable_network,
        number_of_runs: sub.number_of_runs,
        additional_files: sub.additional_files,
        callback_url: sub.callback_url,
    }
}

/// Re-publishes rows whose queue message was lost (NATS restart, publish race,
/// worker crash before ack). Safe against duplicates: workers claim rows atomically.
async fn requeue_stuck(app: Arc<AppData>) {
    loop {
        sleep(Duration::from_secs(30)).await;

        // A worker that died mid-job leaves the row in `processing`. Requeue it
        // once the job is older than any legitimate run could be (see validate()).
        if let Err(e) = app
            .db
            .execute(
                "UPDATE submissions SET status = 'inqueue', updated_at = now() \
                 WHERE status = 'processing' AND updated_at < now() - interval '15 minutes'",
                &[],
            )
            .await
        {
            eprintln!("requeue: could not recover stale processing rows: {e}");
        }

        let rows = match app
            .db
            .query(
                "SELECT * FROM submissions WHERE status = 'inqueue' \
                 AND created_at < now() - make_interval(secs => $1) \
                 ORDER BY created_at LIMIT 100",
                &[&(REQUEUE_AFTER_SECS as f64)],
            )
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("requeue: DB query failed: {e}");
                continue;
            }
        };

        let mut requeued = 0usize;
        for row in rows {
            let sub: Submission = row.into();
            let Some(lang) = app.config.get_language(&sub.language) else {
                eprintln!(
                    "requeue: language '{}' no longer configured, skipping {}",
                    sub.language, sub.id
                );
                continue;
            };
            let job = to_nats(sub, lang);
            match serde_json::to_vec(&job) {
                Ok(payload) => match publish_job(&app.nats, &payload).await {
                    Ok(()) => requeued += 1,
                    Err(e) => eprintln!("requeue: publish failed: {e}"),
                },
                Err(e) => eprintln!("requeue: serialize failed: {e}"),
            }
        }

        if requeued > 0 {
            println!("requeued {requeued} stuck submission(s)");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL not found");
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let client = NatsClient::new(&nats_url).await?;
    client
        .ensure_stream(nats::SUBMISSION_STREAM, nats::SUBMISSION_SUBJECT)
        .await?;

    let app_data = Arc::new(AppData {
        db: db::Db::init(&database_url)
            .await
            .expect("could not init db"),
        config: load_config()?,
        nats: client,
    });

    tokio::spawn(requeue_stuck(app_data.clone()));

    let api_service = OpenApiService::new(Api, "Executor", "0.0.1").server("http://localhost:3000");
    let ui = api_service.swagger_ui();
    let spec = api_service.spec();
    let route = Route::new()
        .at("/health", poem::endpoint::make_sync(|_| "ok"))
        .nest("/", api_service)
        .nest("/docs", ui)
        .at(
            "/openapi.json",
            poem::endpoint::make_sync(move |_| spec.clone()),
        )
        .with(Cors::new())
        .data(app_data);

    Server::new(TcpListener::bind(bind_addr)).run(route).await?;
    Ok(())
}
