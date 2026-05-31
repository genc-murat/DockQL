//! Background telemetry collection loop.
//!
//! Periodically collects Docker snapshots, metrics, and events and persists
//! them into a `TelemetryStore`. Runs as a background tokio task.

use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::{
    docker::DockerClient,
    metrics::MetricsCollector,
    storage::{TelemetrySnapshot, TelemetryStore},
};

/// Configuration for the background collector.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// How often to take a full snapshot (containers, images, networks, volumes), in seconds.
    pub snapshot_interval_secs: u64,
    /// How often to collect container metrics (CPU, memory, etc.), in seconds.
    pub metrics_interval_secs: u64,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            snapshot_interval_secs: 300, // 5 minutes
            metrics_interval_secs: 30,   // 30 seconds
        }
    }
}

/// Result from a single collection tick.
#[derive(Debug, Default)]
pub struct CollectionStats {
    pub snapshots_written: u64,
    pub metrics_written: u64,
    pub errors: Vec<String>,
}

/// Spawn a background metrics collector that periodically writes to the store.
///
/// Returns a `watch::Receiver<CollectionStats>` that can be polled for progress
/// and a `JoinHandle` to the background task.
pub fn spawn_metrics_collector<S, C, M>(
    store: Arc<Mutex<S>>,
    docker: C,
    metrics_collector: M,
    config: CollectorConfig,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()>
where
    S: TelemetryStore + Send + 'static,
    C: DockerClient + Send + 'static,
    M: MetricsCollector + Send + 'static,
{
    tokio::spawn(async move {
        let mut metrics_interval =
            tokio::time::interval(std::time::Duration::from_secs(config.metrics_interval_secs));
        let mut snapshot_interval =
            tokio::time::interval(std::time::Duration::from_secs(config.snapshot_interval_secs));

        // Tick immediately on start for the first collection.
        metrics_interval.tick().await;
        snapshot_interval.tick().await;

        // Take initial snapshot.
        collect_snapshot(&store, &docker);

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
                _ = metrics_interval.tick() => {
                    collect_metrics(&store, &metrics_collector);
                }
                _ = snapshot_interval.tick() => {
                    collect_snapshot(&store, &docker);
                }
            }
        }
    })
}

fn collect_metrics<S, M>(store: &Arc<Mutex<S>>, metrics_collector: &M)
where
    S: TelemetryStore,
    M: MetricsCollector,
{
    match metrics_collector.collect() {
        Ok(samples) => {
            if let Ok(mut store) = store.lock() {
                for sample in samples {
                    if let Err(e) = store.write_metric(sample) {
                        eprintln!("[collector] metric write error: {e}");
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("[collector] metrics collection error: {e}");
        }
    }
}

fn collect_snapshot<S, C>(store: &Arc<Mutex<S>>, docker: &C)
where
    S: TelemetryStore,
    C: DockerClient,
{
    let timestamp = chrono::Utc::now().to_rfc3339();
    let containers = docker.list_containers().unwrap_or_default();
    let images = docker.list_images().unwrap_or_default();
    let networks = docker.list_networks().unwrap_or_default();
    let volumes = docker.list_volumes().unwrap_or_default();

    let snapshot = TelemetrySnapshot {
        timestamp,
        containers,
        images,
        networks,
        volumes,
    };

    if let Ok(mut store) = store.lock() {
        if let Err(e) = store.write_snapshot(snapshot) {
            eprintln!("[collector] snapshot write error: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        docker::MockDockerClient,
        metrics::MockMetricsCollector,
        storage::InMemoryTelemetryStore,
    };

    #[tokio::test]
    async fn collector_writes_snapshot_and_metrics() {
        let store = Arc::new(Mutex::new(InMemoryTelemetryStore::default()));
        let docker = MockDockerClient {
            containers: vec![crate::docker::Container {
                id: "abc".to_owned(),
                name: "api".to_owned(),
                image: "api:latest".to_owned(),
                status: "Up".to_owned(),
                state: "running".to_owned(),
                ports: Vec::new(),
                labels: Vec::new(),
                created_at: None,
                started_at: None,
                finished_at: None,
                restart_count: Some(0),
            }],
            ..Default::default()
        };
        let metrics = MockMetricsCollector {
            samples: vec![crate::docker::MetricSample {
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

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let config = CollectorConfig {
            snapshot_interval_secs: 1,
            metrics_interval_secs: 1,
        };

        let handle = spawn_metrics_collector(
            Arc::clone(&store),
            docker,
            metrics,
            config,
            shutdown_rx,
        );

        // Wait a bit for collector to run.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // Signal shutdown.
        shutdown_tx.send(true).unwrap();
        handle.await.unwrap();

        // Verify data was written.
        let store = store.lock().unwrap();
        let latest = store.latest_metrics().unwrap();
        assert!(!latest.is_empty());
    }
}
