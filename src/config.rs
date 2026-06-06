//! Configuration management.
//!
//! Loads/saves DOL configuration from YAML or TOML files. Supports
//! Docker host, output format, timeouts, and theme settings. Config
//! is searched in standard paths (`$XDG_CONFIG_HOME/dol/`, `~/.dolrc`, etc.).
//!
//! # Example
//!
//! ```ignore
//! let config = DolConfig::load();
//! let api_cfg = DockerApiConfig::from(&config);
//! ```

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
    /// Default colour theme: "dark" or "light".
    pub theme: Option<String>,

    // ── Docker API timeout settings (seconds) ─────────────────
    /// Timeout for standard Docker API calls (list, inspect, etc.). Default: 30s.
    pub api_timeout: Option<u64>,
    /// Timeout for lightweight Docker API calls (ping). Default: 10s.
    pub api_quick_timeout: Option<u64>,
    /// Timeout for per-container stats calls. Default: 10s.
    pub stats_timeout: Option<u64>,
    /// Max seconds to wait for a single event from the events stream. Default: 30s.
    pub events_timeout: Option<u64>,
    /// Timeout for alert webhook HTTP POST. Default: 10s.
    pub webhook_timeout: Option<u64>,
    /// Timeout for alert container restart action. Default: 30s.
    pub restart_timeout: Option<u64>,

    // ── SMTP / Email notification settings ───────────────────
    /// SMTP server hostname for email alerts. Default: localhost.
    pub smtp_host: Option<String>,
    /// SMTP server port. Default: 25.
    pub smtp_port: Option<u16>,
    /// SMTP username (optional).
    pub smtp_user: Option<String>,
    /// SMTP password (optional).
    pub smtp_pass: Option<String>,

    // ── Anomaly detection thresholds ────────────────────────
    /// CPU % above which a warning is issued. Default: 80.
    pub analysis_high_cpu_percent: Option<f64>,
    /// CPU % above which a critical alert is issued. Default: 95.
    pub analysis_critical_cpu_percent: Option<f64>,
    /// Memory usage/limit ratio above which a warning is issued. Default: 0.85.
    pub analysis_memory_pressure_ratio: Option<f64>,
    /// Memory usage/limit ratio above which a critical alert is issued. Default: 0.95.
    pub analysis_critical_memory_ratio: Option<f64>,
    /// Restart count threshold for restart loop detection. Default: 3.
    pub analysis_restart_loop_count: Option<u64>,
    /// Number of die events indicating deployment failure. Default: 3.
    pub analysis_deployment_error_threshold: Option<u64>,
    /// Memory increase % indicating a resource leak. Default: 20.
    pub analysis_leak_memory_increase_pct: Option<f64>,
    /// Min metric samples needed for leak detection. Default: 3.
    pub analysis_leak_min_samples: Option<u64>,
    /// Network I/O bytes warning threshold. Default: 1048576 (1 MB).
    pub analysis_high_network_bytes: Option<u64>,
    /// Network I/O bytes critical threshold. Default: 10485760 (10 MB).
    pub analysis_critical_network_bytes: Option<u64>,
    /// Disk I/O bytes warning threshold. Default: 10485760 (10 MB).
    pub analysis_high_disk_bytes: Option<u64>,
    /// Disk I/O bytes critical threshold. Default: 104857600 (100 MB).
    pub analysis_critical_disk_bytes: Option<u64>,
}

impl DolConfig {
    #[must_use]
    pub fn load() -> Self {
        let paths = config_paths();
        for path in &paths {
            if path.exists() {
                let Ok(content) = std::fs::read_to_string(path) else {
                    continue;
                };
                if let Ok(config) = serde_yaml::from_str::<Self>(&content) {
                    return config;
                }
                if let Ok(config) = toml::from_str::<Self>(&content) {
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
    /// Create a default config file at the standard config path.
    Init,
    /// Set a config key to a value (e.g. `theme light`, `api-timeout 60`).
    Set { key: String, value: String },
    /// Display the current configuration.
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
                "theme" => config.theme = Some(value.clone()),
                "api-timeout" | "api_timeout" => {
                    config.api_timeout = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer (seconds)")
                    })?);
                }
                "api-quick-timeout" | "api_quick_timeout" => {
                    config.api_quick_timeout = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer (seconds)")
                    })?);
                }
                "stats-timeout" | "stats_timeout" => {
                    config.stats_timeout = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer (seconds)")
                    })?);
                }
                "events-timeout" | "events_timeout" => {
                    config.events_timeout = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer (seconds)")
                    })?);
                }
                "webhook-timeout" | "webhook_timeout" => {
                    config.webhook_timeout = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer (seconds)")
                    })?);
                }
                "restart-timeout" | "restart_timeout" => {
                    config.restart_timeout = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer (seconds)")
                    })?);
                }
                "smtp-host" | "smtp_host" => {
                    config.smtp_host = Some(value.clone());
                }
                "smtp-port" | "smtp_port" => {
                    config.smtp_port = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected port number")
                    })?);
                }
                "smtp-user" | "smtp_user" => {
                    config.smtp_user = Some(value.clone());
                }
                "smtp-pass" | "smtp_pass" => {
                    config.smtp_pass = Some(value.clone());
                }
                // ── Anomaly detection threshold keys ────────────────────
                "analysis-high-cpu-percent" | "analysis_high_cpu_percent" => {
                    config.analysis_high_cpu_percent = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected number")
                    })?);
                }
                "analysis-critical-cpu-percent" | "analysis_critical_cpu_percent" => {
                    config.analysis_critical_cpu_percent = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected number")
                    })?);
                }
                "analysis-memory-pressure-ratio" | "analysis_memory_pressure_ratio" => {
                    config.analysis_memory_pressure_ratio = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected number")
                    })?);
                }
                "analysis-critical-memory-ratio" | "analysis_critical_memory_ratio" => {
                    config.analysis_critical_memory_ratio = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected number")
                    })?);
                }
                "analysis-restart-loop-count" | "analysis_restart_loop_count" => {
                    config.analysis_restart_loop_count = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
                "analysis-deployment-error-threshold" | "analysis_deployment_error_threshold" => {
                    config.analysis_deployment_error_threshold =
                        Some(value.parse().map_err(|_| {
                            anyhow::anyhow!("invalid value for {key}: expected integer")
                        })?);
                }
                "analysis-leak-memory-increase-pct" | "analysis_leak_memory_increase_pct" => {
                    config.analysis_leak_memory_increase_pct =
                        Some(value.parse().map_err(|_| {
                            anyhow::anyhow!("invalid value for {key}: expected number")
                        })?);
                }
                "analysis-leak-min-samples" | "analysis_leak_min_samples" => {
                    config.analysis_leak_min_samples = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
                "analysis-high-network-bytes" | "analysis_high_network_bytes" => {
                    config.analysis_high_network_bytes = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
                "analysis-critical-network-bytes" | "analysis_critical_network_bytes" => {
                    config.analysis_critical_network_bytes = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
                "analysis-high-disk-bytes" | "analysis_high_disk_bytes" => {
                    config.analysis_high_disk_bytes = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
                "analysis-critical-disk-bytes" | "analysis_critical_disk_bytes" => {
                    config.analysis_critical_disk_bytes = Some(value.parse().map_err(|_| {
                        anyhow::anyhow!("invalid value for {key}: expected integer")
                    })?);
                }
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
    dirs::config_dir().map_or_else(
        || PathBuf::from(".dolrc"),
        |config_dir| config_dir.join("dol").join("config.yaml"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::DockerApiConfig;

    // ── Default values ───────────────────────────────────────────────────

    #[test]
    fn default_all_timeout_fields_are_none() {
        let config = DolConfig::default();
        assert!(config.api_timeout.is_none());
        assert!(config.api_quick_timeout.is_none());
        assert!(config.stats_timeout.is_none());
        assert!(config.events_timeout.is_none());
        assert!(config.webhook_timeout.is_none());
        assert!(config.restart_timeout.is_none());
    }

    #[test]
    fn default_non_timeout_fields() {
        let config = DolConfig::default();
        assert!(config.store.is_none());
        assert!(config.output.is_none());
        assert!(config.metrics_interval.is_none());
        assert!(config.snapshot_interval.is_none());
        assert!(config.host.is_none());
        assert!(config.theme.is_none());
    }

    // ── YAML serialization / deserialization ────────────────────────────

    #[test]
    fn yaml_roundtrip_empty() {
        let config = DolConfig::default();
        let yaml = serde_yaml::to_string(&config).expect("serialize");
        let deserialized: DolConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(config.store, deserialized.store);
        assert_eq!(config.api_timeout, deserialized.api_timeout);
        assert_eq!(config.stats_timeout, deserialized.stats_timeout);
    }

    #[test]
    fn yaml_roundtrip_with_timeouts() {
        let config = DolConfig {
            api_timeout: Some(60),
            api_quick_timeout: Some(15),
            stats_timeout: Some(20),
            events_timeout: Some(120),
            webhook_timeout: Some(5),
            restart_timeout: Some(45),
            ..DolConfig::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize");
        let deserialized: DolConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(deserialized.api_timeout, Some(60));
        assert_eq!(deserialized.api_quick_timeout, Some(15));
        assert_eq!(deserialized.stats_timeout, Some(20));
        assert_eq!(deserialized.events_timeout, Some(120));
        assert_eq!(deserialized.webhook_timeout, Some(5));
        assert_eq!(deserialized.restart_timeout, Some(45));
    }

    #[test]
    fn yaml_deserialize_from_string() {
        let yaml = r#"
api_timeout: 60
stats_timeout: 15
events_timeout: 30
webhook_timeout: 10
restart_timeout: 45
"#;
        let config: DolConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.api_timeout, Some(60));
        assert_eq!(config.stats_timeout, Some(15));
        assert_eq!(config.events_timeout, Some(30));
        assert_eq!(config.webhook_timeout, Some(10));
        assert_eq!(config.restart_timeout, Some(45));
        // Not set in YAML — should be None
        assert!(config.api_quick_timeout.is_none());
        // Non-timeout fields should not be affected
        assert!(config.store.is_none());
    }

    #[test]
    fn yaml_deserialize_mixed_fields() {
        let yaml = r#"
store: /tmp/dol.db
output: json
theme: light
api_timeout: 90
"#;
        let config: DolConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.store.as_deref(), Some("/tmp/dol.db"));
        assert_eq!(config.output.as_deref(), Some("json"));
        assert_eq!(config.theme.as_deref(), Some("light"));
        assert_eq!(config.api_timeout, Some(90));
        // Unset fields remain None
        assert!(config.stats_timeout.is_none());
        assert!(config.events_timeout.is_none());
    }

    // ── DockerApiConfig conversion ───────────────────────────────────────

    #[test]
    fn docker_api_config_from_default_config() {
        let config = DolConfig::default();
        let api_cfg = DockerApiConfig::from(&config);
        assert_eq!(api_cfg.call_timeout.as_secs(), 30);
        assert_eq!(api_cfg.quick_timeout.as_secs(), 10);
        assert_eq!(api_cfg.max_retries, 2);
        assert_eq!(api_cfg.retry_base_ms, 200);
    }

    #[test]
    fn docker_api_config_from_custom_config() {
        let config = DolConfig {
            api_timeout: Some(120),
            api_quick_timeout: Some(30),
            ..DolConfig::default()
        };
        let api_cfg = DockerApiConfig::from(&config);
        assert_eq!(api_cfg.call_timeout.as_secs(), 120);
        assert_eq!(api_cfg.quick_timeout.as_secs(), 30);
        // max_retries and retry_base_ms are not configurable from DolConfig
        assert_eq!(api_cfg.max_retries, 2);
        assert_eq!(api_cfg.retry_base_ms, 200);
    }

    #[test]
    fn docker_api_config_mixed_defaults() {
        // Only set api_timeout — others should use defaults
        let config = DolConfig {
            api_timeout: Some(45),
            ..DolConfig::default()
        };
        let api_cfg = DockerApiConfig::from(&config);
        assert_eq!(api_cfg.call_timeout.as_secs(), 45);
        // Not set in config, should fall back to default
        assert_eq!(api_cfg.quick_timeout.as_secs(), 10);
    }

    // ── TOML serialization ───────────────────────────────────────────────

    #[test]
    fn toml_roundtrip_with_timeouts() {
        let config = DolConfig {
            api_timeout: Some(60),
            stats_timeout: Some(15),
            ..DolConfig::default()
        };
        let toml_str = toml::to_string(&config).expect("serialize");
        let deserialized: DolConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(deserialized.api_timeout, Some(60));
        assert_eq!(deserialized.stats_timeout, Some(15));
        assert!(deserialized.events_timeout.is_none());
    }

    // ── ConfigAction::Set match arm verification ─────────────────────────
    // execute_config writes to disk, so we can't easily test it in unit tests.
    // Instead, we test the YAML -> DolConfig deserialization which is the
    // underlying mechanism — if a key round-trips through YAML, the config set
    // logic will work correctly when writing/reading from disk.

    #[test]
    fn config_set_all_timeout_keys_roundtrip_through_yaml() {
        // Simulate: for each timeout key, create a config with that key set,
        // serialize to YAML, deserialize back, and verify the value is preserved.
        // This validates that all 6 timeout keys are properly handled by serde.
        let cases = [
            ("api_timeout", 60u64),
            ("api_quick_timeout", 15u64),
            ("stats_timeout", 20u64),
            ("events_timeout", 120u64),
            ("webhook_timeout", 5u64),
            ("restart_timeout", 45u64),
        ];

        for (key, value) in &cases {
            let yaml = format!("{key}: {value}\n");
            let config: DolConfig = serde_yaml::from_str(&yaml)
                .unwrap_or_else(|e| panic!("failed to deserialize {key}: {e}"));

            match *key {
                "api_timeout" => assert_eq!(config.api_timeout, Some(*value)),
                "api_quick_timeout" => assert_eq!(config.api_quick_timeout, Some(*value)),
                "stats_timeout" => assert_eq!(config.stats_timeout, Some(*value)),
                "events_timeout" => assert_eq!(config.events_timeout, Some(*value)),
                "webhook_timeout" => assert_eq!(config.webhook_timeout, Some(*value)),
                "restart_timeout" => assert_eq!(config.restart_timeout, Some(*value)),
                _ => panic!("unexpected key: {key}"),
            }
        }
    }

    #[test]
    fn config_set_invalid_value_rejected_by_serde() {
        // Non-integer values for timeout fields should fail to deserialize.
        let yaml = "api_timeout: not_a_number\n";
        let result: Result<DolConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "expected deserialization error for invalid value"
        );
    }

    #[test]
    fn config_set_negative_value_rejected() {
        // Negative values should fail to deserialize as u64.
        let yaml = "stats_timeout: -5\n";
        let result: Result<DolConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "expected deserialization error for negative value"
        );
    }

    // ── SMTP config fields ────────────────────────────────────────────────

    #[test]
    fn default_smtp_fields_are_none() {
        let config = DolConfig::default();
        assert!(config.smtp_host.is_none());
        assert!(config.smtp_port.is_none());
        assert!(config.smtp_user.is_none());
        assert!(config.smtp_pass.is_none());
    }

    #[test]
    fn yaml_roundtrip_with_smtp() {
        let config = DolConfig {
            smtp_host: Some("smtp.gmail.com".to_owned()),
            smtp_port: Some(587),
            smtp_user: Some("user@gmail.com".to_owned()),
            smtp_pass: Some("app-password".to_owned()),
            ..DolConfig::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize");
        let deserialized: DolConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(deserialized.smtp_host.as_deref(), Some("smtp.gmail.com"));
        assert_eq!(deserialized.smtp_port, Some(587));
        assert_eq!(deserialized.smtp_user.as_deref(), Some("user@gmail.com"));
        assert_eq!(deserialized.smtp_pass.as_deref(), Some("app-password"));
    }

    #[test]
    fn yaml_deserialize_smtp_fields() {
        let yaml = r#"
smtp_host: smtp.gmail.com
smtp_port: 587
smtp_user: user@gmail.com
smtp_pass: app-password
"#;
        let config: DolConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.smtp_host.as_deref(), Some("smtp.gmail.com"));
        assert_eq!(config.smtp_port, Some(587));
        assert_eq!(config.smtp_user.as_deref(), Some("user@gmail.com"));
        assert_eq!(config.smtp_pass.as_deref(), Some("app-password"));
    }

    #[test]
    fn yaml_deserialize_smtp_defaults_when_omitted() {
        let yaml = "api_timeout: 30\n";
        let config: DolConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert!(config.smtp_host.is_none());
        assert!(config.smtp_port.is_none());
        assert!(config.smtp_user.is_none());
        assert!(config.smtp_pass.is_none());
    }

    #[test]
    fn config_set_all_smtp_keys_roundtrip_through_yaml() {
        // Simulate: for each SMTP key, create a config with that key set,
        // serialize to YAML, deserialize back, and verify the value is preserved.
        // This validates that all 4 SMTP keys are properly handled by serde.
        let cases = [
            ("smtp_host", "smtp.gmail.com"),
            ("smtp_port", "587"),
            ("smtp_user", "user@gmail.com"),
            ("smtp_pass", "app-password"),
        ];

        for &(key, value) in &cases {
            let yaml = format!("{key}: {value}\n");
            let config: DolConfig = serde_yaml::from_str(&yaml)
                .unwrap_or_else(|e| panic!("failed to deserialize {key}: {e}"));

            match key {
                "smtp_host" => assert_eq!(config.smtp_host.as_deref(), Some(value)),
                "smtp_port" => {
                    let expected: u16 = value.parse().unwrap();
                    assert_eq!(config.smtp_port, Some(expected));
                }
                "smtp_user" => assert_eq!(config.smtp_user.as_deref(), Some(value)),
                "smtp_pass" => assert_eq!(config.smtp_pass.as_deref(), Some(value)),
                _ => panic!("unexpected key: {key}"),
            }
        }
    }

    #[test]
    fn toml_roundtrip_with_smtp() {
        let config = DolConfig {
            smtp_host: Some("mail.example.com".to_owned()),
            smtp_port: Some(465),
            ..DolConfig::default()
        };
        let toml_str = toml::to_string(&config).expect("serialize");
        let deserialized: DolConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(deserialized.smtp_host.as_deref(), Some("mail.example.com"));
        assert_eq!(deserialized.smtp_port, Some(465));
        assert!(deserialized.smtp_user.is_none());
        assert!(deserialized.smtp_pass.is_none());
    }

    // ── Analysis threshold config fields ──────────────────────────────

    #[test]
    fn default_analysis_threshold_fields_are_none() {
        let config = DolConfig::default();
        assert!(config.analysis_high_cpu_percent.is_none());
        assert!(config.analysis_critical_cpu_percent.is_none());
        assert!(config.analysis_memory_pressure_ratio.is_none());
        assert!(config.analysis_critical_memory_ratio.is_none());
        assert!(config.analysis_restart_loop_count.is_none());
        assert!(config.analysis_deployment_error_threshold.is_none());
        assert!(config.analysis_leak_memory_increase_pct.is_none());
        assert!(config.analysis_leak_min_samples.is_none());
        assert!(config.analysis_high_network_bytes.is_none());
        assert!(config.analysis_critical_network_bytes.is_none());
        assert!(config.analysis_high_disk_bytes.is_none());
        assert!(config.analysis_critical_disk_bytes.is_none());
    }

    #[test]
    fn yaml_roundtrip_with_analysis_thresholds() {
        let config = DolConfig {
            analysis_high_cpu_percent: Some(85.0),
            analysis_critical_cpu_percent: Some(98.0),
            analysis_memory_pressure_ratio: Some(0.90),
            analysis_critical_memory_ratio: Some(0.99),
            analysis_restart_loop_count: Some(5),
            analysis_deployment_error_threshold: Some(10),
            analysis_leak_memory_increase_pct: Some(30.0),
            analysis_leak_min_samples: Some(5),
            analysis_high_network_bytes: Some(2_097_152),
            analysis_critical_network_bytes: Some(20_971_520),
            analysis_high_disk_bytes: Some(20_971_520),
            analysis_critical_disk_bytes: Some(209_715_200),
            ..DolConfig::default()
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize");
        let deserialized: DolConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(deserialized.analysis_high_cpu_percent, Some(85.0));
        assert_eq!(deserialized.analysis_critical_cpu_percent, Some(98.0));
        assert_eq!(deserialized.analysis_memory_pressure_ratio, Some(0.90));
        assert_eq!(deserialized.analysis_critical_memory_ratio, Some(0.99));
        assert_eq!(deserialized.analysis_restart_loop_count, Some(5));
        assert_eq!(deserialized.analysis_deployment_error_threshold, Some(10));
        assert_eq!(deserialized.analysis_leak_memory_increase_pct, Some(30.0));
        assert_eq!(deserialized.analysis_leak_min_samples, Some(5));
        assert_eq!(deserialized.analysis_high_network_bytes, Some(2_097_152));
        assert_eq!(
            deserialized.analysis_critical_network_bytes,
            Some(20_971_520)
        );
        assert_eq!(deserialized.analysis_high_disk_bytes, Some(20_971_520));
        assert_eq!(deserialized.analysis_critical_disk_bytes, Some(209_715_200));
    }

    #[test]
    fn yaml_deserialize_analysis_defaults_when_omitted() {
        let yaml = "api_timeout: 30\n";
        let config: DolConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert!(config.analysis_high_cpu_percent.is_none());
        assert!(config.analysis_critical_cpu_percent.is_none());
        assert!(config.analysis_restart_loop_count.is_none());
    }

    #[test]
    fn config_set_all_analysis_keys_roundtrip_through_yaml() {
        let cases = [
            ("analysis_high_cpu_percent", "85.0"),
            ("analysis_critical_cpu_percent", "98.0"),
            ("analysis_memory_pressure_ratio", "0.90"),
            ("analysis_critical_memory_ratio", "0.99"),
            ("analysis_restart_loop_count", "5"),
            ("analysis_deployment_error_threshold", "10"),
            ("analysis_leak_memory_increase_pct", "30.0"),
            ("analysis_leak_min_samples", "5"),
            ("analysis_high_network_bytes", "2097152"),
            ("analysis_critical_network_bytes", "20971520"),
            ("analysis_high_disk_bytes", "20971520"),
            ("analysis_critical_disk_bytes", "209715200"),
        ];

        for &(key, value) in &cases {
            let yaml = format!("{key}: {value}\n");
            let config: DolConfig = serde_yaml::from_str(&yaml)
                .unwrap_or_else(|e| panic!("failed to deserialize {key}: {e}"));

            match key {
                "analysis_high_cpu_percent" => {
                    assert_eq!(config.analysis_high_cpu_percent, Some(85.0));
                }
                "analysis_critical_cpu_percent" => {
                    assert_eq!(config.analysis_critical_cpu_percent, Some(98.0));
                }
                "analysis_memory_pressure_ratio" => {
                    assert_eq!(config.analysis_memory_pressure_ratio, Some(0.90));
                }
                "analysis_critical_memory_ratio" => {
                    assert_eq!(config.analysis_critical_memory_ratio, Some(0.99));
                }
                "analysis_restart_loop_count" => {
                    assert_eq!(config.analysis_restart_loop_count, Some(5));
                }
                "analysis_deployment_error_threshold" => {
                    assert_eq!(config.analysis_deployment_error_threshold, Some(10));
                }
                "analysis_leak_memory_increase_pct" => {
                    assert_eq!(config.analysis_leak_memory_increase_pct, Some(30.0));
                }
                "analysis_leak_min_samples" => {
                    assert_eq!(config.analysis_leak_min_samples, Some(5));
                }
                "analysis_high_network_bytes" => {
                    assert_eq!(config.analysis_high_network_bytes, Some(2_097_152));
                }
                "analysis_critical_network_bytes" => {
                    assert_eq!(config.analysis_critical_network_bytes, Some(20_971_520));
                }
                "analysis_high_disk_bytes" => {
                    assert_eq!(config.analysis_high_disk_bytes, Some(20_971_520));
                }
                "analysis_critical_disk_bytes" => {
                    assert_eq!(config.analysis_critical_disk_bytes, Some(209_715_200));
                }
                _ => panic!("unexpected key: {key}"),
            }
        }
    }

    #[test]
    fn toml_roundtrip_with_analysis_thresholds() {
        let config = DolConfig {
            analysis_high_cpu_percent: Some(90.0),
            analysis_restart_loop_count: Some(7),
            ..DolConfig::default()
        };
        let toml_str = toml::to_string(&config).expect("serialize");
        let deserialized: DolConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(deserialized.analysis_high_cpu_percent, Some(90.0));
        assert_eq!(deserialized.analysis_restart_loop_count, Some(7));
        assert!(deserialized.analysis_critical_cpu_percent.is_none());
    }

    #[test]
    fn config_set_smtp_invalid_port_rejected() {
        // Non-integer port value should fail to deserialize for smtp_port field.
        let yaml = "smtp_port: not_a_number\n";
        let result: Result<DolConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "expected deserialization error for invalid port"
        );
    }
}
