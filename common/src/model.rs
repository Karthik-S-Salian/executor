use std::fmt;
use chrono::{DateTime, Utc};
use poem_openapi::Enum;
use poem_openapi::Object;
use serde::{Deserialize, Serialize};
use tokio_postgres::Row;
use postgres_types::{ToSql, FromSql};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Enum, ToSql, FromSql)]
#[serde(rename_all = "lowercase")]
#[postgres(name = "submission_status", rename_all = "lowercase")]
pub enum SubmissionStatus {
    InQueue,
    Processing,
    Accepted,
    WrongAnswer,
    TimeLimitExceeded,
    CompilationError,
    RuntimeErrorSigsegv,
    RuntimeErrorSigxfsz,
    RuntimeErrorSigfpe,
    RuntimeErrorSigabrt,
    RuntimeErrorNzec,
    RuntimeErrorOther,
    InternalError,
    ExecFormatError,
}

impl std::str::FromStr for SubmissionStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use SubmissionStatus::*;
        Ok(match s {
            "inqueue" => InQueue,
            "processing" => Processing,
            "accepted" => Accepted,
            "wronganswer" => WrongAnswer,
            "timelimitexceeded" => TimeLimitExceeded,
            "compilationerror" => CompilationError,
            "runtimeerrorsigsegv" => RuntimeErrorSigsegv,
            "runtimeerrorsigxfsz" => RuntimeErrorSigxfsz,
            "runtimeerrorsigfpe" => RuntimeErrorSigfpe,
            "runtimeerrorsigabrt" => RuntimeErrorSigabrt,
            "runtimeerrornzec" => RuntimeErrorNzec,
            "runtimeerrorother" => RuntimeErrorOther,
            "internalerror" => InternalError,
            "execformaterror" => ExecFormatError,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Enum, ToSql, FromSql)]
#[serde(rename_all = "lowercase")]
#[oai(rename_all = "lowercase")] 
#[postgres(name = "language", rename_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    Cpp,
    C,
}

impl std::str::FromStr for Language {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use Language::*;
        Ok(match s {
            "rust" => Rust,
            "python" => Python,
            "cpp" => Cpp,
            "c" => C,
            _ => return Err(()),
        })
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Language::*;
        let name = match self {
            Rust => "rust",
            Python => "python",
            Cpp => "cpp",
            C => "c",
        };
        write!(f, "{}", name)
    }
}

impl Language {
    pub fn all() -> Vec<Language> {
        use Language::*;
        vec![Rust, Python, Cpp, C]
    }
}

#[derive(Debug, Clone, Object)]
pub struct Submission {
    pub id: String,
    pub source_code: String,
    pub language: Language,
    pub compiler_options: Option<String>,
    pub command_line_arguments: Option<String>,
    pub stdin: Option<String>,
    pub expected_output: Option<String>,
    pub cpu_time_limit: Option<f64>,
    pub cpu_extra_time: Option<f64>,
    pub wall_time_limit: Option<f64>,
    pub memory_limit: Option<f64>,
    pub stack_limit: Option<i32>,
    pub max_processes_and_or_threads: Option<i32>,
    pub enable_per_process_and_thread_time_limit: Option<bool>,
    pub enable_per_process_and_thread_memory_limit: Option<bool>,
    pub max_file_size: Option<i32>,
    pub redirect_stderr_to_stdout: Option<bool>,
    pub enable_network: Option<bool>,
    pub number_of_runs: Option<i32>,
    pub additional_files: Option<String>,
    pub callback_url: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub compile_output: Option<String>,
    pub message: Option<String>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub status: SubmissionStatus,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub time: Option<f64>,
    pub wall_time: Option<f64>,
    pub memory: Option<f64>,
}

impl From<Row> for Submission {
    fn from(row: Row) -> Self {
        Submission {
            id: row.get("id"),
            source_code: row.get("source_code"),
            language: row.get::<_, String>("language").parse().unwrap(),
            compiler_options: row.try_get("compiler_options").ok().flatten(),
            command_line_arguments: row.try_get("command_line_arguments").ok().flatten(),
            stdin: row.try_get("stdin").ok().flatten(),
            expected_output: row.try_get("expected_output").ok().flatten(),
            cpu_time_limit: row.try_get("cpu_time_limit").ok().flatten(),
            cpu_extra_time: row.try_get("cpu_extra_time").ok().flatten(),
            wall_time_limit: row.try_get("wall_time_limit").ok().flatten(),
            memory_limit: row.try_get("memory_limit").ok().flatten(),
            stack_limit: row.try_get("stack_limit").ok().flatten(),
            max_processes_and_or_threads: row
                .try_get("max_processes_and_or_threads")
                .ok()
                .flatten(),
            enable_per_process_and_thread_time_limit: row
                .try_get("enable_per_process_and_thread_time_limit")
                .ok()
                .flatten(),
            enable_per_process_and_thread_memory_limit: row
                .try_get("enable_per_process_and_thread_memory_limit")
                .ok()
                .flatten(),
            max_file_size: row.try_get("max_file_size").ok().flatten(),
            redirect_stderr_to_stdout: row.try_get("redirect_stderr_to_stdout").ok().flatten(),
            enable_network: row.try_get("enable_network").ok().flatten(),
            number_of_runs: row.try_get("number_of_runs").ok().flatten(),
            additional_files: row.try_get("additional_files").ok().flatten(),
            callback_url: row.try_get("callback_url").ok().flatten(),
            stdout: row.try_get("stdout").ok().flatten(),
            stderr: row.try_get("stderr").ok().flatten(),
            compile_output: row.try_get("compile_output").ok().flatten(),
            message: row.try_get("message").ok().flatten(),
            exit_code: row.try_get("exit_code").ok().flatten(),
            exit_signal: row.try_get("exit_signal").ok().flatten(),
            status: row.get::<_, String>("status").parse().unwrap(),
            created_at: row.get("created_at"),
            finished_at: row.try_get("finished_at").ok().flatten(),
            time: row.try_get("time").ok().flatten(),
            wall_time: row.try_get("wall_time").ok().flatten(),
            memory: row.try_get("memory").ok().flatten(),
        }
    }
}

#[derive(Debug, Clone, Object)]
pub struct NewSubmission {
    pub source_code: String,
    pub language: Language,
    pub compiler_options: Option<String>,
    pub command_line_arguments: Option<String>,
    pub stdin: Option<String>,
    pub expected_output: Option<String>,
    pub cpu_time_limit: Option<f64>,
    pub cpu_extra_time: Option<f64>,
    pub wall_time_limit: Option<f64>,
    pub memory_limit: Option<f64>,
    pub stack_limit: Option<i32>,
    pub max_processes_and_or_threads: Option<i32>,
    pub enable_per_process_and_thread_time_limit: Option<bool>,
    pub enable_per_process_and_thread_memory_limit: Option<bool>,
    pub max_file_size: Option<i32>,
    pub redirect_stderr_to_stdout: Option<bool>,
    pub enable_network: Option<bool>,
    pub number_of_runs: Option<i32>,
    pub additional_files: Option<String>,
    pub callback_url: Option<String>,
}


#[derive(Debug, Serialize, Deserialize)]
pub struct RedisSubmission{
    pub id:String,
    pub source_code: String,
    pub language: Language,
    pub compiler_options: Option<String>,
    pub command_line_arguments: Option<String>,
    pub stdin: Option<String>,
    pub expected_output: Option<String>,
    pub cpu_time_limit: Option<f64>,
    pub cpu_extra_time: Option<f64>,
    pub wall_time_limit: Option<f64>,
    pub memory_limit: Option<f64>,
    pub stack_limit: Option<i32>,
    pub max_processes_and_or_threads: Option<i32>,
    pub enable_per_process_and_thread_time_limit: Option<bool>,
    pub enable_per_process_and_thread_memory_limit: Option<bool>,
    pub max_file_size: Option<i32>,
    pub redirect_stderr_to_stdout: Option<bool>,
    pub enable_network: Option<bool>,
    pub number_of_runs: Option<i32>,
    pub additional_files: Option<String>,
    pub callback_url: Option<String>,
}


impl From<(String, NewSubmission)> for RedisSubmission {
    fn from((id, ns): (String, NewSubmission)) -> Self {
        Self {
            id: id,
            source_code: ns.source_code,
            language: ns.language,
            compiler_options: ns.compiler_options,
            command_line_arguments: ns.command_line_arguments,
            stdin: ns.stdin,
            expected_output: ns.expected_output,
            cpu_time_limit: ns.cpu_time_limit,
            cpu_extra_time: ns.cpu_extra_time,
            wall_time_limit: ns.wall_time_limit,
            memory_limit: ns.memory_limit,
            stack_limit: ns.stack_limit,
            max_processes_and_or_threads: ns.max_processes_and_or_threads,
            enable_per_process_and_thread_time_limit: ns.enable_per_process_and_thread_time_limit,
            enable_per_process_and_thread_memory_limit: ns.enable_per_process_and_thread_memory_limit,
            max_file_size: ns.max_file_size,
            redirect_stderr_to_stdout: ns.redirect_stderr_to_stdout,
            enable_network: ns.enable_network,
            number_of_runs: ns.number_of_runs,
            additional_files: ns.additional_files,
            callback_url: ns.callback_url,
        }
    }
}
