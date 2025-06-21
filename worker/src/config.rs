use config::{Config, File};
use serde::Deserialize;
use std::env;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct AppConfig {
    pub num_workers: i32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { num_workers: -1 }
    }
}

pub fn load_config() -> AppConfig {
    let path = env::var("WORKER_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config.toml"));

    let mut app_config = Config::builder()
        .add_source(File::from(path).required(false))
        .build()
        .and_then(|c| c.try_deserialize::<AppConfig>())
        .unwrap_or_else(|_| AppConfig::default());

    if app_config.num_workers < 1 {
        app_config.num_workers = num_cpus::get() as i32;
    }

    app_config
}
