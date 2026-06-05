//! Metrics collection from Docker containers.
//!
//! Defines the [`MetricsCollector`] trait with implementations:
//! - [`BollardMetricsCollector`] — live stats from bollard
//! - [`MockMetricsCollector`] — test fixtures
//! - [`NoopMetricsCollector`] — no-op for non-metric queries
//! - [`MetricRingBuffer`] — bounded circular buffer for windowed analysis
//!
//! # Example
//!
//! ```ignore
//! let collector = BollardMetricsCollector::with_config(Arc::new(docker), &config);
//! let samples = collector.collect().await?;
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::time::timeout;

use crate::config::DolConfig;
use crate::docker::{DockerClient, DockerError, MetricSample};

pub trait MetricsCollector {
    fn collect(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<MetricSample>, MetricsError>> + Send;
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("{0}")]
    Docker(#[from] DockerError),
    #[error("invalid docker stats field `{field}`: {value}")]
    InvalidStatsField { field: &'static str, value: String },
    #[error("tokio runtime error: {0}")]
    Runtime(String),
}

/// A metrics collector that uses a `DockerClient` (bollard) to fetch container stats.
///
/// Collectors wrap an `Arc<C>` so they can be passed by `Clone` into
/// `tokio::task::spawn_blocking` and other `'static` contexts.
#[derive(Debug, Clone)]
pub struct BollardMetricsCollector<C> {
    docker: Arc<C>,
    stats_timeout: Duration,
}

impl<C> BollardMetricsCollector<C>
where
    C: DockerClient + Send + Sync + 'static,
{
    /// Create a new collector with the default stats timeout (10s).
    pub const fn new(docker: Arc<C>) -> Self {
        Self {
            docker,
            stats_timeout: Duration::from_secs(10),
        }
    }

    /// Create a new collector with a specific stats timeout from config.
    #[must_use]
    pub fn with_config(docker: Arc<C>, config: &DolConfig) -> Self {
        Self {
            docker,
            stats_timeout: Duration::from_secs(config.stats_timeout.unwrap_or(10)),
        }
    }

    /// Override the per-container stats call timeout.
    pub fn set_stats_timeout(&mut self, timeout: Duration) {
        self.stats_timeout = timeout;
    }
}

impl<C> MetricsCollector for BollardMetricsCollector<C>
where
    C: DockerClient + Send + Sync + 'static,
{
    async fn collect(&self) -> Result<Vec<MetricSample>, MetricsError> {
        let containers = self
            .docker
            .list_containers()
            .await
            .map_err(MetricsError::Docker)?;

        let mut samples = Vec::new();
        for container in &containers {
            let id_short = &container.id[..12.min(container.id.len())];
            let id = id_short.to_owned();
            match timeout(self.stats_timeout, self.docker.container_stats(&id)).await {
                Ok(Ok(sample)) => samples.push(sample),
                Ok(Err(e)) => {
                    eprintln!("[metrics] failed to get stats for {}: {e}", container.name);
                }
                Err(_) => {
                    eprintln!(
                        "[metrics] stats call timed out after {}s for {}",
                        self.stats_timeout.as_secs(),
                        container.name
                    );
                }
            }
        }
        Ok(samples)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MockMetricsCollector {
    pub samples: Vec<MetricSample>,
}

impl MetricsCollector for MockMetricsCollector {
    async fn collect(&self) -> Result<Vec<MetricSample>, MetricsError> {
        Ok(self.samples.clone())
    }
}

#[derive(Debug, Clone, Default)]
pub struct NoopMetricsCollector;

impl MetricsCollector for NoopMetricsCollector {
    async fn collect(&self) -> Result<Vec<MetricSample>, MetricsError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MetricRingBuffer {
    samples: VecDeque<MetricSample>,
    capacity: usize,
}

impl MetricRingBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, sample: MetricSample) {
        if self.capacity == 0 {
            return;
        }

        while self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    #[must_use]
    pub fn latest_by_container(&self) -> HashMap<String, MetricSample> {
        let mut latest = HashMap::new();
        for sample in &self.samples {
            latest.insert(sample.container_id.clone(), sample.clone());
            latest.insert(sample.container_name.clone(), sample.clone());
        }
        latest
    }
}

/// Parse a percentage string like "85.42%" into a float.
#[allow(clippy::result_unit_err)]
pub fn parse_percent(value: &str) -> Result<f64, ()> {
    value.trim().trim_end_matches('%').parse().map_err(|_| ())
}

/// Parse a usage pair string like "128MiB / 1GiB" into (used, limit) bytes.
#[allow(clippy::result_unit_err)]
pub fn parse_usage_pair(value: &str) -> Result<(Option<u64>, Option<u64>), ()> {
    let mut parts = value.split('/').map(str::trim);
    let left = parts.next().ok_or(())?;
    let right = parts.next();

    Ok((
        parse_byte_quantity(left)?,
        right.map(parse_byte_quantity).transpose()?.flatten(),
    ))
}

#[allow(clippy::result_unit_err)]
pub fn parse_byte_quantity(value: &str) -> Result<Option<u64>, ()> {
    let value = value.trim();
    if value.is_empty() || value == "--" {
        return Ok(None);
    }

    let split_at = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let number = value[..split_at].trim().parse::<f64>().map_err(|_| ())?;
    let unit = value[split_at..].trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "b" => 1.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(()),
    };

    Ok(Some((number * multiplier).round() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_percent() {
        assert_eq!(parse_percent("85.42%"), Ok(85.42));
    }

    #[test]
    fn parses_byte_quantities() {
        assert_eq!(parse_byte_quantity("1KiB"), Ok(Some(1024)));
        assert_eq!(parse_byte_quantity("1.5MB"), Ok(Some(1_500_000)));
        assert_eq!(parse_byte_quantity("--"), Ok(None));
    }

    #[test]
    fn parses_usage_pairs() {
        assert_eq!(
            parse_usage_pair("12.5MiB / 1GiB"),
            Ok((Some(13_107_200), Some(1_073_741_824)))
        );
    }

    #[tokio::test]
    async fn mock_collector_works() {
        let collector = MockMetricsCollector {
            samples: vec![MetricSample {
                container_id: "abc".to_owned(),
                container_name: "api".to_owned(),
                timestamp: "2026-01-01T12:00:00Z".to_owned(),
                cpu_percent: Some(50.0),
                memory_usage_bytes: Some(128),
                memory_limit_bytes: Some(1024),
                network_rx_bytes: None,
                network_tx_bytes: None,
                disk_read_bytes: None,
                disk_write_bytes: None,
            }],
        };
        let samples = collector.collect().await.unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].container_name, "api");
    }

    #[tokio::test]
    async fn noop_collector_returns_empty() {
        let samples = NoopMetricsCollector.collect().await.unwrap();
        assert!(samples.is_empty());
    }

    #[test]
    fn ring_buffer_keeps_latest_samples_by_container() {
        let mut buffer = MetricRingBuffer::new(1);
        buffer.push(MetricSample {
            container_id: "abc".to_owned(),
            container_name: "api".to_owned(),
            timestamp: "t1".to_owned(),
            cpu_percent: Some(10.0),
            memory_usage_bytes: None,
            memory_limit_bytes: None,
            network_rx_bytes: None,
            network_tx_bytes: None,
            disk_read_bytes: None,
            disk_write_bytes: None,
        });
        buffer.push(MetricSample {
            container_id: "abc".to_owned(),
            container_name: "api".to_owned(),
            timestamp: "t2".to_owned(),
            cpu_percent: Some(20.0),
            memory_usage_bytes: None,
            memory_limit_bytes: None,
            network_rx_bytes: None,
            network_tx_bytes: None,
            disk_read_bytes: None,
            disk_write_bytes: None,
        });

        let latest = buffer.latest_by_container();

        assert_eq!(latest["abc"].cpu_percent, Some(20.0));
        assert_eq!(latest["api"].cpu_percent, Some(20.0));
    }
}
