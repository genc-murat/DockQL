use std::path::PathBuf;

use clap::Subcommand;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DolConfig {
    pub store: Option<String>,
    pub output: Option<String>,
    pub metrics_interval: Option<u64>,
    pub snapshot_interval: Option<u64>,
    pub host: Option<String>,
}

impl DolConfig {
    pub fn load() -> Self {
        let paths = config_paths();
        for path in &paths {
            if path.exists() {
                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Ok(config) = serde_yaml::from_str::<DolConfig>(&content) {
                    return config;
                }
                if let Ok(config) = toml::from_str::<DolConfig>(&content) {
                    return config;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_yaml::to_string(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigAction {
    Init,
    Set { key: String, value: String },
    View,
}

pub fn execute_config(action: ConfigAction) -> anyhow::Result<()> {
    match action {
        ConfigAction::Init => {
            let path = config_path();
            if path.exists() {
                anyhow::bail!("config file already exists at {}", path.display());
            }
            let config = DolConfig::default();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let content = serde_yaml::to_string(&config)?;
            std::fs::write(&path, content)?;
            println!("Created default config at {}", path.display());
            Ok(())
        }
        ConfigAction::Set { key, value } => {
            let mut config = DolConfig::load();
            match key.as_str() {
                "store" => config.store = Some(value.clone()),
                "output" => config.output = Some(value.clone()),
                "metrics-interval" | "metrics_interval" => {
                    config.metrics_interval = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
                "snapshot-interval" | "snapshot_interval" => {
                    config.snapshot_interval = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
                "host" => config.host = Some(value.clone()),
                _ => anyhow::bail!("unknown config key: {key}"),
            }
            config.save()?;
            println!("Set {key} = {value}");
            Ok(())
        }
        ConfigAction::View => {
            let config = DolConfig::load();
            let content = serde_yaml::to_string(&config)?;
            print!("{content}");
            Ok(())
        }
    }
}

fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("dol").join("config.yaml"));
        paths.push(config_dir.join("dol").join("config.toml"));
        paths.push(config_dir.join("dolrc"));
    }

    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".dolrc"));
        paths.push(home.join(".dolrc.yaml"));
        paths.push(home.join(".dolrc.toml"));
    }

    paths.push(PathBuf::from(".dolrc"));
    paths.push(PathBuf::from("dol.yaml"));
    paths.push(PathBuf::from("dol.toml"));

    paths
}

fn config_path() -> PathBuf {
    if let Some(config_dir) = dirs::config_dir() {
        config_dir.join("dol").join("config.yaml")
    } else {
        PathBuf::from(".dolrc")
    }
}
