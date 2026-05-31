use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

use crate::{
    alerts::{self, AlertEvaluator},
    ast::Query,
    collector::{self, CollectorConfig},
    config::{self, ConfigAction, DolConfig},
    dashboard,
    docker::DockerCliClient,
    events::{self, DockerCliEventSource},
    executor::{self, render_csv, render_jsonl, render_table},
    export::{self, ExportFormat},
    metrics::{DockerCliMetricsCollector, MetricsCollector},
    parser, planner,
    sqlite_store::SqliteTelemetryStore,
    storage::TelemetryStore,
};

#[derive(Debug, Parser)]
#[command(
    name = "dol",
    version,
    about = "Docker Observability Language command line interface",
    subcommand_value_name = "COMMAND",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    /// DOL query to execute (positional).
    pub query: Option<String>,

    /// Output format: table, json, csv, jsonl.
    #[arg(long, value_enum)]
    pub output: Option<OutputFormat>,

    /// Path to the SQLite telemetry store file.
    #[arg(long)]
    pub store: Option<String>,

    /// Run in background collection mode. Requires --store.
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

    /// Run retention cleanup on the store.
    #[arg(long)]
    pub apply_retention: bool,

    /// Show the query execution plan without running it.
    #[arg(long)]
    pub explain: bool,

    /// Re-run the query every N seconds (batch queries only).
    #[arg(long)]
    pub watch: Option<u64>,

    /// Export results to a file (format inferred from extension: .csv, .json, .jsonl, .table).
    #[arg(long)]
    pub export: Option<PathBuf>,

    /// Export format for external systems: influx, loki, prometheus (use with --export).
    #[arg(long)]
    pub export_format: Option<ExportFormat>,

    /// Push results to InfluxDB v1 HTTP write API (e.g. http://localhost:8086/write?db=mydb).
    #[arg(long)]
    pub export_influx: Option<String>,

    /// Push results to Grafana Loki HTTP push API (e.g. http://localhost:3100).
    #[arg(long)]
    pub export_grafana_loki: Option<String>,

    /// Push results to Prometheus Pushgateway (e.g. http://localhost:9091).
    #[arg(long)]
    pub export_prometheus: Option<String>,

    /// Remote Docker host (e.g. tcp://192.168.1.100:2375).
    #[arg(long)]
    pub host: Option<String>,

    /// Generate shell completion script.
    #[arg(long, value_enum)]
    pub completion: Option<clap_complete::Shell>,

    /// Compare current state with the last store snapshot (requires --store).
    #[arg(long)]
    pub diff: bool,

    /// Subcommand (config, repl).
    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
    /// Manage DOL configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Interactive REPL shell.
    Repl,
    /// Live-updating TUI container monitor (top-like).
    Top,
    /// Multi-panel dashboard with containers and events.
    Dashboard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Csv,
    Jsonl,
}

/// Resolve the effective Docker host from CLI `--host` or config file,
/// and set `DOCKER_HOST` so that all `docker` CLI subprocesses use it.
/// CLI `--host` takes precedence over config `host`.
fn apply_host(cli_host: Option<&str>, config_host: Option<&str>) {
    let host = cli_host.or(config_host);
    if let Some(host) = host {
        // SAFETY: setting DOCKER_HOST from user-provided --host flag or config
        unsafe {
            std::env::set_var("DOCKER_HOST", host);
        }
    }
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let config = DolConfig::load();

    // Set DOCKER_HOST *before* any subcommand or query execution so that
    // DockerCliClient, DockerCliMetricsCollector, DockerCliEventSource,
    // and all direct `docker` CLI calls pick up the correct host.
    apply_host(cli.host.as_deref(), config.host.as_deref());

    // Handle subcommands.
    if let Some(cmd) = &cli.command {
        match cmd {
            CliCommand::Config { action } => {
                return config::execute_config(action.clone());
            }
            CliCommand::Repl => {
                return crate::repl::run_repl(&config).await;
            }
            CliCommand::Top => {
                return dashboard::run_top(&config).await;
            }
            CliCommand::Dashboard => {
                return dashboard::run_dashboard(&config).await;
            }
        }
    }

    let output_format = cli.output.unwrap_or(OutputFormat::Table);

    if let Some(shell) = cli.completion {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    // Handle --store-stats mode.
    if cli.store_stats {
        let store_path = cli
            .store
            .as_deref()
            .or(config.store.as_deref())
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
            .or(config.store.as_deref())
            .ok_or_else(|| anyhow::anyhow!("--apply-retention requires --store <path>"))?;
        let mut store = SqliteTelemetryStore::open(store_path)?;
        let stats = store.apply_retention()?;
        println!("Retention cleanup complete:");
        println!("  Metrics deleted:   {}", stats.metrics_deleted);
        println!("  Events deleted:    {}", stats.events_deleted);
        println!("  Snapshots deleted: {}", stats.snapshots_deleted);
        return Ok(());
    }

    // Handle --collect mode.
    if cli.collect {
        let store_path = cli
            .store
            .as_deref()
            .or(config.store.as_deref())
            .ok_or_else(|| anyhow::anyhow!("--collect requires --store <path>"))?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let store = Arc::new(Mutex::new(store));
        let docker = DockerCliClient::default();
        let metrics = DockerCliMetricsCollector::default();
        let config_cfg = CollectorConfig {
            snapshot_interval_secs: cli.snapshot_interval,
            metrics_interval_secs: cli.metrics_interval,
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        println!(
            "Starting background collector (metrics every {}s, snapshots every {}s)...",
            config_cfg.metrics_interval_secs, config_cfg.snapshot_interval_secs
        );
        println!("Press Ctrl+C to stop.");

        let handle = collector::spawn_metrics_collector(
            Arc::clone(&store),
            docker,
            metrics,
            config_cfg,
            shutdown_rx,
        );

        tokio::signal::ctrl_c().await?;
        println!("\nShutting down collector...");
        shutdown_tx.send(true)?;
        handle.await?;

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

    if cli.explain {
        let plan = planner::plan(&parsed.query).map_err(|e| anyhow::anyhow!("plan error: {e}"))?;
        println!("{plan}");
        return Ok(());
    }

    let export_writer = if let Some(ref path) = cli.export {
        let file = std::fs::File::create(path)?;
        Some(Mutex::new(file))
    } else {
        None
    };

    let output_result = |result: &executor::ExecutionResult| -> anyhow::Result<()> {
        // When --export-format is set, write in that format regardless of --output
        if let Some(export_fmt) = cli.export_format {
            if let Some(ref writer) = export_writer {
                use std::io::Write;
                let mut file = writer.lock().unwrap();
                match export_fmt {
                    ExportFormat::Influx => {
                        let text = export::format_as_influx(result, "containers");
                        file.write_all(text.as_bytes())?;
                    }
                    ExportFormat::Loki => {
                        let text = export::format_as_loki(result)?;
                        file.write_all(text.as_bytes())?;
                    }
                    ExportFormat::Prometheus => {
                        let text = export::format_as_prometheus(result);
                        file.write_all(text.as_bytes())?;
                    }
                }
            }
            return Ok(());
        }

        match output_format {
            OutputFormat::Table => {
                let text = render_table(result);
                if let Some(ref writer) = export_writer {
                    use std::io::Write;
                    let mut file = writer.lock().unwrap();
                    writeln!(file, "{text}")?;
                } else {
                    println!("{text}");
                }
            }
            OutputFormat::Json => {
                if let Ok(json) = serde_json::to_string_pretty(&result) {
                    if let Some(ref writer) = export_writer {
                        use std::io::Write;
                        let mut file = writer.lock().unwrap();
                        file.write_all(json.as_bytes())?;
                    } else {
                        println!("{json}");
                    }
                }
            }
            OutputFormat::Csv => {
                let csv = render_csv(result);
                if let Some(ref writer) = export_writer {
                    use std::io::Write;
                    let mut file = writer.lock().unwrap();
                    writeln!(file, "{csv}")?;
                } else {
                    println!("{csv}");
                }
            }
            OutputFormat::Jsonl => {
                let jsonl = render_jsonl(result);
                if let Some(ref writer) = export_writer {
                    use std::io::Write;
                    let mut file = writer.lock().unwrap();
                    file.write_all(jsonl.as_bytes())?;
                    if !jsonl.is_empty() {
                        writeln!(file)?;
                    }
                } else {
                    println!("{jsonl}");
                }
            }
        }
        Ok(())
    };

    // Check if this is a historical query that needs the store.
    if needs_store(&parsed.query) {
        let store_path = cli.store.as_deref().or(config.store.as_deref()).ok_or_else(|| {
            anyhow::anyhow!(
                "this query requires historical data; provide --store <path> to use a telemetry store"
            )
        })?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let result = executor::execute_with_store(&parsed.query, &store)?;
        output_result(&result)?;
        run_exports(&cli, &result).await?;
        return Ok(());
    }

    if let Query::Alert(rule) = &parsed.query {
        let metrics = DockerCliMetricsCollector::default();
        let mut evaluator = AlertEvaluator::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

        let store: Option<Arc<Mutex<SqliteTelemetryStore>>> = cli
            .store
            .as_deref()
            .or(config.store.as_deref())
            .map(SqliteTelemetryStore::open)
            .transpose()?
            .map(|s| Arc::new(Mutex::new(s)));

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
                _ = interval.tick() => {
                    let samples = metrics.collect()?;

                    if let Some(ref store) = store
                        && let Ok(mut s) = store.lock() {
                            for sample in &samples {
                                let _ = s.write_metric(sample.clone());
                            }
                        }

                    let events = evaluator.evaluate_samples(rule, &samples, std::time::Instant::now())?;
                    for event in events {
                        match output_format {
                            OutputFormat::Table => println!("{}", alerts::render_alert_event(&event)),
                            OutputFormat::Json | OutputFormat::Jsonl => println!("{}", serde_json::to_string(&event)?),
                            OutputFormat::Csv => println!("{},{},{:?}", event.container_name, event.message, event.action),
                        }
                    }
                }
            }
        }

        return Ok(());
    }

    if let Query::Events(events_query) = &parsed.query {
        let source = DockerCliEventSource::default();

        let store: Option<Arc<Mutex<SqliteTelemetryStore>>> = cli
            .store
            .as_deref()
            .or(config.store.as_deref())
            .map(SqliteTelemetryStore::open)
            .transpose()?
            .map(|s| Arc::new(Mutex::new(s)));

        return events::stream_events(events_query, &source, |row| {
            if let Some(ref store) = store
                && let Ok(mut s) = store.lock()
                && let (Some(time), Some(action)) = (
                    row.fields.get("time").and_then(|v| v.as_str()),
                    row.fields.get("action").and_then(|v| v.as_str()),
                )
            {
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

            let result = executor::ExecutionResult { rows: vec![row] };
            match output_format {
                OutputFormat::Table => {
                    println!("{}", render_table(&result));
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string(&result.rows[0])
                            .map_err(events::EventsError::Json)?
                    );
                }
                OutputFormat::Csv => {
                    let mut columns: Vec<String> = result.rows[0].fields.keys().cloned().collect();
                    columns.sort();
                    let vals: Vec<String> = columns
                        .iter()
                        .map(|c| {
                            result.rows[0]
                                .fields
                                .get(c)
                                .map(crate::eval::render_json_cell)
                                .unwrap_or_default()
                        })
                        .collect();
                    println!("{}", vals.join(","));
                }
                OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::to_string(&result.rows[0])
                            .map_err(events::EventsError::Json)?
                    );
                }
            }
            Ok(())
        })
        .map_err(Into::into);
    }

    let docker = DockerCliClient::default();
    let metrics = DockerCliMetricsCollector::default();

    if let Some(interval_secs) = cli.watch {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = interval.tick() => {
                    match executor::execute_with_metrics(&parsed.query, &docker, &metrics) {
                        Ok(ref result) => {
                            if let Err(e) = output_result(result) {
                                eprintln!("Error writing output: {e}");
                            }
                            if let Err(e) = run_exports(&cli, result).await {
                                eprintln!("Export error: {e}");
                            }
                        }
                        Err(e) => eprintln!("Error: {e}"),
                    }
                }
            }
        }
        return Ok(());
    }

    let result = executor::execute_with_metrics(&parsed.query, &docker, &metrics)?;

    if cli.diff {
        let store_path = cli
            .store
            .as_deref()
            .or(config.store.as_deref())
            .ok_or_else(|| anyhow::anyhow!("--diff requires --store <path>"))?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let diff_output = executor::render_diff(&result, &store)?;
        println!("{diff_output}");
        return Ok(());
    }

    output_result(&result)?;
    run_exports(&cli, &result).await?;

    Ok(())
}

/// Push query results to configured external export targets.
async fn run_exports(cli: &Cli, result: &executor::ExecutionResult) -> anyhow::Result<()> {
    if let Some(ref url) = cli.export_influx {
        eprintln!(
            "Pushing {} rows to InfluxDB at {url} ...",
            result.rows.len()
        );
        export::push_to_influxdb(url, result).await?;
    }
    if let Some(ref url) = cli.export_grafana_loki {
        eprintln!(
            "Pushing {} rows to Grafana Loki at {url} ...",
            result.rows.len()
        );
        export::push_to_loki(url, result).await?;
    }
    if let Some(ref url) = cli.export_prometheus {
        eprintln!(
            "Pushing {} rows to Prometheus Pushgateway at {url} ...",
            result.rows.len()
        );
        export::push_to_prometheus(url, result).await?;
    }
    Ok(())
}
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
            output: None,
            store: None,
            collect: false,
            metrics_interval: 30,
            snapshot_interval: 300,
            store_stats: false,
            apply_retention: false,
            explain: false,
            watch: None,
            export: None,
            export_format: None,
            export_influx: None,
            export_grafana_loki: None,
            export_prometheus: None,
            host: None,
            completion: None,
            diff: false,
            command: None,
        };

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("empty DOL query"));
    }

    #[tokio::test]
    async fn historical_query_requires_store_flag() {
        let cli = Cli {
            query: Some("inspect container api at \"2026-01-01 12:00:00\"".to_owned()),
            output: None,
            store: None,
            collect: false,
            metrics_interval: 30,
            snapshot_interval: 300,
            store_stats: false,
            apply_retention: false,
            explain: false,
            watch: None,
            export: None,
            export_format: None,
            export_influx: None,
            export_grafana_loki: None,
            export_prometheus: None,
            host: None,
            completion: None,
            diff: false,
            command: None,
        };

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("--store"));
    }
}
