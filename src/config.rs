use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DolConfig {
    pub store: Option<String>,
    pub output: Option<String>,
    pub metrics_interval: Option<u64>,
    pub snapshot_interval: Option<u64>,
    pub host: Option<String>,
}

impl Default for DolConfig {
    fn default() -> Self {
        Self {
            store: None,
            output: None,
            metrics_interval: None,
            snapshot_interval: None,
            host: None,
        }
    }
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

    #[allow(dead_code)]
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
