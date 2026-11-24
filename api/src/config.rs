use anyhow::{Context, Result, anyhow};
use common::model::{Language, NewSubmission};
use config::{Config, File};
use serde::Deserialize;
use std::env;
use std::fs;

const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_STDIN_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct SubmissionDefaults {
    pub cpu_time_limit: f64,
    pub wall_time_limit: f64,
    pub memory_limit: f64,
    pub number_of_runs: i32,
    pub enable_network: bool,
}

impl Default for SubmissionDefaults {
    fn default() -> Self {
        Self {
            cpu_time_limit: 2.0,
            wall_time_limit: 5.0,
            memory_limit: 128_000.0,
            number_of_runs: 1,
            enable_network: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawLanguage {
    pub name: String,
    pub source_file: String,
    pub file_extension: String,
    pub compile_cmd: Option<String>,
    pub compile_cmd_file: Option<String>,
    pub run_cmd: Option<String>,
    pub run_cmd_file: Option<String>,
    pub allow_network: bool,
}

impl Default for RawLanguage {
    fn default() -> Self {
        Self {
            name: String::new(),
            source_file: String::new(),
            file_extension: String::new(),
            compile_cmd: None,
            compile_cmd_file: None,
            run_cmd: None,
            run_cmd_file: None,
            allow_network: false,
        }
    }
}

impl RawLanguage {
    pub fn into_resolved(self) -> Result<Language> {
        let compile_cmd = match (self.compile_cmd, self.compile_cmd_file) {
            (Some(cmd), _) => Some(cmd),
            (_, Some(file)) => Some(
                fs::read_to_string(&file)
                    .with_context(|| format!("Failed to read compile_cmd_file: {}", file))?
                    .trim()
                    .to_string(),
            ),
            (None, None) => None,
        };

        let run_cmd = match (self.run_cmd, self.run_cmd_file) {
            (Some(cmd), _) => Ok(cmd),
            (_, Some(file)) => fs::read_to_string(&file)
                .with_context(|| format!("Failed to read run_cmd_file: {}", file))
                .map(|s| s.trim().to_string()),
            (None, None) => Err(anyhow!("Language '{}' missing run command", self.name)),
        }?;

        Ok(Language {
            name: self.name,
            source_file: self.source_file,
            file_extension: self.file_extension,
            compile_cmd,
            run_cmd,
            allow_network: self.allow_network,
        })
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct RawAppConfig {
    pub submission_defaults: SubmissionDefaults,
    pub languages: Vec<RawLanguage>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub submission_defaults: SubmissionDefaults,
    pub languages: Vec<Language>,
}

impl AppConfig {
    pub fn get_language(&self, name: &str) -> Option<Language> {
        self.languages.iter().find(|l| l.name == name).cloned()
    }

    pub fn get_language_names(&self) -> Vec<String> {
        self.languages.iter().map(|l| l.name.clone()).collect()
    }

    /// Fills missing limits from config defaults before insert, so the worker
    /// never has to guess.
    pub fn apply_defaults(&self, ns: &mut NewSubmission) {
        let d = &self.submission_defaults;
        ns.cpu_time_limit.get_or_insert(d.cpu_time_limit);
        ns.wall_time_limit.get_or_insert(d.wall_time_limit);
        ns.memory_limit.get_or_insert(d.memory_limit);
        ns.number_of_runs.get_or_insert(d.number_of_runs);
        ns.enable_network.get_or_insert(d.enable_network);
    }

    /// Rejects requests the engine cannot honour instead of silently ignoring them.
    pub fn validate(&self, ns: &NewSubmission, lang: &Language) -> Result<(), String> {
        if ns.source_code.len() > MAX_SOURCE_BYTES {
            return Err(format!("source_code exceeds {} bytes", MAX_SOURCE_BYTES));
        }
        if ns.stdin.as_ref().is_some_and(|s| s.len() > MAX_STDIN_BYTES) {
            return Err(format!("stdin exceeds {} bytes", MAX_STDIN_BYTES));
        }
        if !ns.cpu_time_limit.unwrap_or(0.0).is_finite() || ns.cpu_time_limit.unwrap_or(0.0) <= 0.0
        {
            return Err("cpu_time_limit must be > 0".into());
        }
        if ns.wall_time_limit.unwrap_or(0.0) <= ns.cpu_time_limit.unwrap_or(0.0) {
            return Err("wall_time_limit must be greater than cpu_time_limit".into());
        }
        if ns.cpu_extra_time.is_some_and(|v| v < 0.0) {
            return Err("cpu_extra_time must not be negative".into());
        }
        if ns.stack_limit.is_some_and(|v| v <= 0)
            || ns.max_file_size.is_some_and(|v| v <= 0)
            || ns.max_processes_and_or_threads.is_some_and(|v| v <= 0)
        {
            return Err(
                "stack_limit, max_file_size and max_processes_and_or_threads must be > 0".into(),
            );
        }
        if !(1.0..=4_194_304.0).contains(&ns.memory_limit.unwrap_or(0.0)) {
            return Err("memory_limit must be between 1 and 4194304 KB".into());
        }
        if !(1..=10).contains(&ns.number_of_runs.unwrap_or(1)) {
            return Err("number_of_runs must be between 1 and 10".into());
        }
        // Keeps the stale-worker recovery window (15 min) longer than any
        // legitimate job.
        if ns.wall_time_limit.unwrap_or(0.0) * f64::from(ns.number_of_runs.unwrap_or(1)) > 600.0 {
            return Err("wall_time_limit * number_of_runs must not exceed 600 seconds".into());
        }
        if ns.enable_network.unwrap_or(false) && !lang.allow_network {
            return Err(format!(
                "network is not allowed for language '{}'",
                lang.name
            ));
        }
        if ns.additional_files.is_some() {
            return Err("additional_files is not supported yet".into());
        }
        if ns.callback_url.is_some() {
            return Err("callback_url is not supported yet".into());
        }
        if ns.enable_per_process_and_thread_time_limit == Some(true)
            || ns.enable_per_process_and_thread_memory_limit == Some(true)
        {
            return Err("per-process time/memory limits are not supported yet".into());
        }
        Ok(())
    }
}

pub fn load_config() -> Result<AppConfig> {
    let path = env::var("EXECUTOR_CONFIG_PATH").unwrap_or_else(|_| "config.toml".to_string());

    let raw: RawAppConfig = Config::builder()
        .add_source(File::with_name(&path).required(false))
        .build()
        .with_context(|| format!("Failed to read config from {}", path))?
        .try_deserialize()
        .context("Failed to deserialize AppConfig")?;

    let languages = raw
        .languages
        .into_iter()
        .map(|lang| lang.into_resolved())
        .collect::<Result<Vec<_>>>()?;

    Ok(AppConfig {
        submission_defaults: raw.submission_defaults,
        languages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::model::NewSubmission;

    fn config() -> AppConfig {
        AppConfig {
            submission_defaults: SubmissionDefaults::default(),
            languages: vec![],
        }
    }

    fn submission() -> NewSubmission {
        NewSubmission {
            source_code: "print(1)".into(),
            language: "python".into(),
            compiler_options: None,
            command_line_arguments: None,
            stdin: None,
            expected_output: None,
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

    fn language(allow_network: bool) -> Language {
        Language {
            name: "python".into(),
            source_file: "main.py".into(),
            file_extension: "py".into(),
            compile_cmd: None,
            run_cmd: "python3 main.py".into(),
            allow_network,
        }
    }

    #[test]
    fn fills_defaults() {
        let mut sub = submission();
        config().apply_defaults(&mut sub);
        assert_eq!(sub.cpu_time_limit, Some(2.0));
        assert_eq!(sub.wall_time_limit, Some(5.0));
        assert_eq!(sub.memory_limit, Some(128_000.0));
        assert_eq!(sub.number_of_runs, Some(1));
        assert_eq!(sub.enable_network, Some(false));
    }

    #[test]
    fn respects_explicit_values() {
        let mut sub = submission();
        sub.cpu_time_limit = Some(1.0);
        config().apply_defaults(&mut sub);
        assert_eq!(sub.cpu_time_limit, Some(1.0));
    }

    #[test]
    fn rejects_network_when_language_disallows_it() {
        let mut sub = submission();
        config().apply_defaults(&mut sub);
        sub.enable_network = Some(true);
        assert!(config().validate(&sub, &language(false)).is_err());
        assert!(config().validate(&sub, &language(true)).is_ok());
    }

    #[test]
    fn rejects_unsupported_fields() {
        let mut sub = submission();
        config().apply_defaults(&mut sub);
        sub.additional_files = Some("{}".into());
        assert!(config().validate(&sub, &language(false)).is_err());
    }
}
