use std::sync::Arc;

use common::{
    db,
    error::StringError,
    model::{Language, NewSubmission, RedisSubmission, Submission, SubmissionStatus},
    nats::NatsClient,
};
use poem::{
    EndpointExt, Result, Route, Server,
    error::InternalServerError,
    listener::TcpListener,
    middleware::Cors,
    web::{Data, Path},
};
use poem_openapi::{
    ApiResponse, Object, OpenApi, OpenApiService,
    payload::{Json, PlainText},
};
use tokio_postgres::types::ToSql;

use crate::config::{AppConfig, load_config};

mod config;

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
    ) -> Result<Json<SubmissionResponse>> {
        let params: &[&(dyn ToSql + Sync)] = &[
            &new_submission.source_code,
            &new_submission.language,
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

        let submission = RedisSubmission::from((id_str.clone(), new_submission.0));
        let json = serde_json::to_vec(&submission)
            .map_err(|_| InternalServerError(StringError::new("couldnot serialize submission")))?;
        data.nats
            .publish("submission.new".to_string(), json)
            .await
            .map_err(|_| InternalServerError(StringError::new("couldnot serialize submission")))?;

        Ok(Json(SubmissionResponse { id: id_str }))
    }

    #[oai(path = "/submissions/:id", method = "get")]
    async fn get_submission(
        &self,
        data: Data<&Arc<AppData>>,
        id: Path<String>,
    ) -> Result<SubmissionGetResponse> {
        let params: &[&(dyn ToSql + Sync)] = &[&id.0];
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
    async fn languages(&self) -> Result<Json<Vec<Language>>> {
        Ok(Json(Language::all()))
    }
}

struct AppData {
    db: db::Db,
    config: AppConfig,
    nats: NatsClient,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL not found");

    let client = NatsClient::new("localhost").await?;

    let app_data = Arc::new(AppData {
        db: db::Db::init(&database_url).await.expect("couldnot init db"),
        config: load_config(),
        nats: client,
    });

    let api_service = OpenApiService::new(Api, "Executor", "0.0.1").server("http://localhost:3000");
    let ui = api_service.swagger_ui();
    let spec = api_service.spec();
    let route = Route::new()
        .nest("/", api_service)
        .nest("/docs", ui)
        .at(
            "/openapi.json",
            poem::endpoint::make_sync(move |_| spec.clone()),
        )
        .with(Cors::new())
        .data(app_data);

    Server::new(TcpListener::bind("0.0.0.0:3000"))
        .run(route)
        .await?;
    Ok(())
}
