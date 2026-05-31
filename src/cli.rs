use std::sync::{Arc, Mutex};

use clap::{Parser, ValueEnum};

use crate::{
    alerts::{self, AlertEvaluator},
    ast::Query,
    collector::{self, CollectorConfig},
    docker::DockerCliClient,
    events::{self, DockerCliEventSource},
    executor::{self, render_table},
    metrics::{DockerCliMetricsCollector, MetricsCollector},
    parser,
    sqlite_store::SqliteTelemetryStore,
    storage::TelemetryStore,
};

#[derive(Debug, Parser)]
#[command(
    name = "dol",
    version,
    about = "Docker Observability Language command line interface"
)]
pub struct Cli {
    /// DOL query to execute.
    pub query: Option<String>,

    /// Emit machine-readable JSON.
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub output: OutputFormat,

    /// Path to the SQLite telemetry store file.
    /// When provided, enables historical queries (inspect ... at, events ... from ... to)
    /// and background data collection.
    #[arg(long)]
    pub store: Option<String>,

    /// Run in background collection mode.
    /// Continuously collects Docker metrics and snapshots into the store.
    /// Requires --store.
    #[arg(long)]
    pub collect: bool,

    /// Metrics collection interval in seconds (used with --collect).
    #[arg(long, default_value_t = 30)]
    pub metrics_interval: u64,

    /// Snapshot collection interval in seconds (used with --collect).
    #[arg(long, default_value_t = 300)]
    pub snapshot_interval: u64,

    /// Show telemetry store statistics.
    #[arg(long)]
    pub store_stats: bool,

    /// Run retention cleanup on the store, removing data older than the configured thresholds.
    #[arg(long)]
    pub apply_retention: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    // Handle --store-stats mode.
    if cli.store_stats {
        let store_path = cli
            .store
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--store-stats requires --store <path>"))?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let stats = store.stats()?;
        println!("Telemetry Store Statistics:");
        println!("  Metrics:   {}", stats.metric_count);
        println!("  Events:    {}", stats.event_count);
        println!("  Snapshots: {}", stats.snapshot_count);
        return Ok(());
    }

    // Handle --apply-retention mode.
    if cli.apply_retention {
        let store_path = cli
            .store
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--apply-retention requires --store <path>"))?;
        let mut store = SqliteTelemetryStore::open(store_path)?;
        let stats = store.apply_retention()?;
        println!("Retention cleanup complete:");
        println!("  Metrics deleted:   {}", stats.metrics_deleted);
        println!("  Events deleted:    {}", stats.events_deleted);
        println!("  Snapshots deleted: {}", stats.snapshots_deleted);
        return Ok(());
    }

    // Handle --collect mode (background data collection daemon).
    if cli.collect {
        let store_path = cli
            .store
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--collect requires --store <path>"))?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let store = Arc::new(Mutex::new(store));
        let docker = DockerCliClient::default();
        let metrics = DockerCliMetricsCollector::default();
        let config = CollectorConfig {
            snapshot_interval_secs: cli.snapshot_interval,
            metrics_interval_secs: cli.metrics_interval,
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        println!(
            "Starting background collector (metrics every {}s, snapshots every {}s)...",
            config.metrics_interval_secs, config.snapshot_interval_secs
        );
        println!("Press Ctrl+C to stop.");

        let handle = collector::spawn_metrics_collector(
            Arc::clone(&store),
            docker,
            metrics,
            config,
            shutdown_rx,
        );

        tokio::signal::ctrl_c().await?;
        println!("\nShutting down collector...");
        shutdown_tx.send(true)?;
        handle.await?;

        // Show final stats.
        if let Ok(store) = store.lock() {
            let stats = store.stats()?;
            println!("Final store statistics:");
            println!("  Metrics:   {}", stats.metric_count);
            println!("  Events:    {}", stats.event_count);
            println!("  Snapshots: {}", stats.snapshot_count);
        }

        return Ok(());
    }

    // Regular query execution.
    let query = cli.query.as_deref().unwrap_or_default().trim();

    if query.is_empty() {
        anyhow::bail!("empty DOL query; pass a query such as `observe containers`");
    }

    let parsed = parser::parse(query)?;

    // Check if this is a historical query that needs the store.
    if needs_store(&parsed.query) {
        let store_path = cli.store.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "this query requires historical data; provide --store <path> to use a telemetry store"
            )
        })?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let result = executor::execute_with_store(&parsed.query, &store)?;

        match cli.output {
            OutputFormat::Table => {
                println!("{}", render_table(&result));
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        }

        return Ok(());
    }

    if let Query::Alert(rule) = &parsed.query {
        let metrics = DockerCliMetricsCollector::default();
        let mut evaluator = AlertEvaluator::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

        // If we have a store, also persist metrics during alert evaluation.
        let store: Option<Arc<Mutex<SqliteTelemetryStore>>> = cli
            .store
            .as_deref()
            .map(|path| SqliteTelemetryStore::open(path))
            .transpose()?
            .map(|s| Arc::new(Mutex::new(s)));

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
                _ = interval.tick() => {
                    let samples = metrics.collect()?;

                    // Persist metrics to store if available.
                    if let Some(ref store) = store {
                        if let Ok(mut s) = store.lock() {
                            for sample in &samples {
                                let _ = s.write_metric(sample.clone());
                            }
                        }
                    }

                    let events = evaluator.evaluate_samples(rule, &samples, std::time::Instant::now())?;
                    for event in events {
                        match cli.output {
                            OutputFormat::Table => println!("{}", alerts::render_alert_event(&event)),
                            OutputFormat::Json => println!("{}", serde_json::to_string(&event)?),
                        }
                    }
                }
            }
        }

        return Ok(());
    }

    if let Query::Events(events_query) = &parsed.query {
        let source = DockerCliEventSource::default();

        // If we have a store, persist events as they stream.
        let store: Option<Arc<Mutex<SqliteTelemetryStore>>> = cli
            .store
            .as_deref()
            .map(|path| SqliteTelemetryStore::open(path))
            .transpose()?
            .map(|s| Arc::new(Mutex::new(s)));

        return events::stream_events(events_query, &source, |row| {
            // Persist to store if available.
            if let Some(ref store) = store {
                if let Ok(mut s) = store.lock() {
                    // We can reconstruct a DockerEvent from the row fields.
                    if let (Some(time), Some(action)) = (
                        row.fields.get("time").and_then(|v| v.as_str()),
                        row.fields.get("action").and_then(|v| v.as_str()),
                    ) {
                        let event = crate::docker::DockerEvent {
                            time: time.to_owned(),
                            event_type: row
                                .fields
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("container")
                                .to_owned(),
                            action: action.to_owned(),
                            actor_id: row
                                .fields
                                .get("actor_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned(),
                            container: row
                                .fields
                                .get("container")
                                .and_then(|v| v.as_str())
                                .map(str::to_owned),
                            image: row
                                .fields
                                .get("image")
                                .and_then(|v| v.as_str())
                                .map(str::to_owned),
                            attributes: Vec::new(),
                        };
                        let _ = s.write_event(event);
                    }
                }
            }

            match cli.output {
                OutputFormat::Table => {
                    println!(
                        "{}",
                        render_table(&executor::ExecutionResult { rows: vec![row] })
                    );
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string(&row).map_err(events::EventsError::Json)?
                    );
                }
            }
            Ok(())
        })
        .map_err(Into::into);
    }

    let docker = DockerCliClient::default();
    let metrics = DockerCliMetricsCollector::default();
    let result = executor::execute_with_metrics(&parsed.query, &docker, &metrics)?;

    match cli.output {
        OutputFormat::Table => {
            println!("{}", render_table(&result));
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}

/// Determine if a query needs a telemetry store for historical data.
fn needs_store(query: &Query) -> bool {
    match query {
        Query::Inspect(q) => q.at.is_some(),
        Query::Events(q) => q.time.is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_empty_query() {
        let cli = Cli {
            query: Some("   ".to_owned()),
            output: OutputFormat::Table,
            store: None,
            collect: false,
            metrics_interval: 30,
            snapshot_interval: 300,
            store_stats: false,
            apply_retention: false,
        };

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("empty DOL query"));
    }

    #[tokio::test]
    async fn historical_query_requires_store_flag() {
        let cli = Cli {
            query: Some("inspect container api at \"2026-01-01 12:00:00\"".to_owned()),
            output: OutputFormat::Table,
            store: None,
            collect: false,
            metrics_interval: 30,
            snapshot_interval: 300,
            store_stats: false,
            apply_retention: false,
        };

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("--store"));
    }
}
