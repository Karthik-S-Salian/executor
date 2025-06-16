use config::{Config, File};
use serde::Deserialize;
use std::env;
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

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct AppConfig {
    pub submission_defaults: SubmissionDefaults,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            submission_defaults: SubmissionDefaults::default(),
        }
    }
}

pub fn load_config() -> AppConfig {
    let path = env::var("EXECUTOR_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config.toml"));

    Config::builder()
        .add_source(File::from(path).required(false))
        .build()
        .and_then(|c| c.try_deserialize::<AppConfig>())
        .unwrap_or_else(|_| AppConfig::default())
}
