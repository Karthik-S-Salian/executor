use anyhow::{Context, Result, anyhow};
use common::model::Language;
use config::{Config, File};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

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
