//! CLI argument parsing and command dispatch.
//!
//! Uses [`clap`] to define the `dol` command-line interface: subcommands
//! (config, repl, top, dashboard), flags (`--output`, `--store`, `--watch`,
//! etc.), and the positional DOL query string. The [`run`] function
//! dispatches execution based on the parsed arguments.
//!
//! # Example
//!
//! ```ignore
//! let cli = Cli::parse_from(["dol", "observe containers"]);
//! cli::run(cli).await?;
//! ```

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

use crate::{
    alerts::{self, AlertEvaluator},
    ast::Query,
    collector::{self, CollectorConfig},
    config::{self, ConfigAction, DolConfig},
    dashboard,
    docker::BollardDockerClient,
    events::{self, BollardEventSource},
    executor::{self, ExecutionResult, render_csv, render_jsonl, render_table, render_table_with_theme, Theme},
    export::{self, ExportFormat},
    metrics::{BollardMetricsCollector, MetricsCollector},
    parser, planner,
    sqlite_store::SqliteTelemetryStore,
    storage::TelemetryStore,
    ALERT_EVAL_INTERVAL,
    DEFAULT_METRICS_INTERVAL,
    DEFAULT_SNAPSHOT_INTERVAL,
};

#[derive(Debug, Parser)]
#[command(
    name = "dol",
    version,
    about = "Docker Observability Language — query, monitor, and analyze your Docker infrastructure",
    long_about = "DOL (Docker Observability Language) is a query language for Docker infrastructure.
Use SQL-like pipelines to observe containers, stream events, set up alerts,
analyze anomalies, and inspect historical data — all from a single CLI.

See the full language reference at: https://github.com/genc-murat/DockQL",
    subcommand_value_name = "COMMAND",
    subcommand_negates_reqs = true,
    after_help = r#"
EXAMPLES:

  Basic queries:
    dol "observe containers"                                   List all containers
    dol "observe containers | where state = running"           Filter running containers
    dol "observe containers | select name, image, status"      Select specific fields
    dol "observe containers | sort by memory desc | limit 5"   Top 5 by memory usage
    dol "events containers"                                    Stream Docker events live

  Advanced queries:
    dol "observe containers | group by image | count"          Count containers per image
    dol "alert when cpu > 80% for 2m then webhook http://..."  CPU alert with webhook
    dol "inspect container <name>"                             Inspect a single container
    dol "compose ls"                                           List compose projects
    dol "analyze containers find anomalies"                    Detect issues automatically
    dol "analyze containers explain"                           Full diagnostic summary

  Working with files and store:
    dol -f examples/ping.dol                                   Run query from file
    dol --explain "observe containers"                         Show query plan (dry run)
    dol --store ./dol.db --collect                             Start background collector

  Output and integration:
    dol "observe containers" --output json                     JSON output
    dol "observe containers" --output csv --export results.csv Export to CSV file
    dol "observe containers" --watch 5                         Re-run every 5 seconds
    dol "observe containers" --theme light                     Light theme for tables

  Interactive modes:
    dol repl              Interactive REPL with tab completion
    dol top               Live container monitor (top-like)
    dol dashboard         Multi-panel dashboard with events
    dol config view       Show current configuration
    dol config set theme light  Set default theme to light

For more: https://github.com/genc-murat/DockQL
"#
)]
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    /// DOL query to execute (positional).
    pub query: Option<String>,

    /// Read the DOL query from a .dol file.
    #[arg(short = 'f', long)]
    pub file: Option<String>,

    /// Output format: table, json, json-compact, csv, jsonl.
    #[arg(long, value_enum)]
    pub output: Option<OutputFormat>,

    /// Path to the `SQLite` telemetry store file.
    #[arg(long)]
    pub store: Option<String>,

    /// Run in background collection mode. Requires --store.
    #[arg(long)]
    pub collect: bool,

    /// Metrics collection interval in seconds (used with --collect).
    #[arg(long, default_value_t = DEFAULT_METRICS_INTERVAL)]
    pub metrics_interval: u64,

    /// Snapshot collection interval in seconds (used with --collect).
    #[arg(long, default_value_t = DEFAULT_SNAPSHOT_INTERVAL)]
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

    /// Timeout in seconds for each query execution (applies to watch, alert, events, and store queries).
    /// If the query takes longer than this, it will be aborted and logged.
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Export results to a file (format inferred from extension: .csv, .json, .jsonl, .table).
    #[arg(long)]
    pub export: Option<PathBuf>,

    /// Export format for external systems: influx, loki, prometheus (use with --export).
    #[arg(long)]
    pub export_format: Option<ExportFormat>,

    /// Push results to `InfluxDB` v1 HTTP write API (e.g. <http://localhost:8086/write?db=mydb>).
    #[arg(long)]
    pub export_influx: Option<String>,

    /// Push results to Grafana Loki HTTP push API (e.g. <http://localhost:3100>).
    #[arg(long)]
    pub export_grafana_loki: Option<String>,

    /// Push results to Prometheus Pushgateway (e.g. <http://localhost:9091>).
    #[arg(long)]
    pub export_prometheus: Option<String>,

    /// Remote Docker host (e.g. <tcp://192.168.1.100:2375>).
    #[arg(long)]
    pub host: Option<String>,

    /// Generate shell completion script.
    #[arg(long, value_enum)]
    pub completion: Option<clap_complete::Shell>,

    /// Compare current state with the last store snapshot (requires --store).
    #[arg(long)]
    pub diff: bool,

    /// Color theme for table output: dark (default) or light.
    /// Can also be set in config file with `theme: dark|light`.
    #[arg(long, value_enum)]
    pub theme: Option<Theme>,

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
    /// Interactive REPL shell with tab completion, history, and syntax-colored error messages.
    Repl,
    /// Live-updating TUI container monitor (top-like) with CPU, memory, and network stats.
    Top,
    /// Multi-panel dashboard with container list, live event stream, and resource usage gauges.
    Dashboard,
}

/// Output format: table, json, json-compact, csv, jsonl.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    /// Compact (minified) JSON without indentation or newlines.
    #[value(name = "json-compact")]
    JsonCompact,
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

#[allow(clippy::significant_drop_tightening)]
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let config = DolConfig::load();

    // Initialise global alert timeout config from loaded config.
    alerts::init_alert_timeouts(
        config.webhook_timeout.unwrap_or(10),
        config.restart_timeout.unwrap_or(30),
    );

    // Set DOCKER_HOST *before* any subcommand or query execution so that
    // all Docker clients pick up the correct host.
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

    // Resolve effective colour theme: CLI --theme > config theme > default dark
    let effective_theme = cli.theme.or_else(|| {
        config.theme.as_deref().and_then(|s| match s.to_lowercase().as_str() {
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        })
    }).unwrap_or(Theme::Dark);

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
        let docker_inner = BollardDockerClient::connect_with_config(&config)?;
        let metrics = BollardMetricsCollector::with_config(std::sync::Arc::new(docker_inner.clone()), &config);
        let docker = docker_inner;
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

    // Read query from --file if provided, otherwise use positional argument.
    let query = if let Some(ref file_path) = cli.file {
        std::fs::read_to_string(file_path)
            .map_err(|e| anyhow::anyhow!("failed to read file '{file_path}': {e}"))?
    } else {
        cli.query.as_deref().unwrap_or_default().to_owned()
    };
    let query = query.trim().to_owned();

    if query.is_empty() {
        anyhow::bail!("empty DOL query; pass a query such as `observe containers` or use `--file <path>`");
    }

    let parsed = parser::parse(&query)?;

    if cli.explain {
        let plan = planner::plan(&parsed.query);
        println!("{plan}");
        return Ok(());
    }

    let export_writer = if let Some(ref path) = cli.export {
        let file = std::fs::File::create(path)?;
        Some(Mutex::new(file))
    } else {
        None
    };

    let output_result = |result: &ExecutionResult| -> anyhow::Result<()> {
        // When --export-format is set, write in that format regardless of --output
        if let Some(export_fmt) = cli.export_format {                    if let Some(ref writer) = export_writer {                        use std::io::Write;
                        let mut file = writer.lock().expect("lock writer");
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
                let text = render_table_with_theme(result, effective_theme);
                if let Some(ref writer) = export_writer {                        use std::io::Write;
                        let mut file = writer.lock().expect("lock writer");
                        writeln!(file, "{text}")?;
                } else {
                    print!("{text}");
                }
            }
            OutputFormat::Json => {
                if let Ok(json) = serde_json::to_string_pretty(&result) {
                    if let Some(ref writer) = export_writer {
                        use std::io::Write;
                        let mut file = writer.lock().expect("lock writer");
                        file.write_all(json.as_bytes())?;
                    } else {
                        println!("{json}");
                    }
                }
            }
            OutputFormat::JsonCompact => {
                if let Ok(json) = serde_json::to_string(&result) {
                    if let Some(ref writer) = export_writer {
                        use std::io::Write;
                        let mut file = writer.lock().expect("lock writer");
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
                    let mut file = writer.lock().expect("lock writer");
                    writeln!(file, "{csv}")?;
                } else {
                    println!("{csv}");
                }
            }
            OutputFormat::Jsonl => {
                let jsonl = render_jsonl(result);
                if let Some(ref writer) = export_writer {
                    use std::io::Write;
                    let mut file = writer.lock().expect("lock writer");
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

    // ── Store (historical) queries with optional timeout ──────────────
    if needs_store(&parsed.query) {
        let store_path = cli.store.as_deref().or(config.store.as_deref()).ok_or_else(|| {
            anyhow::anyhow!(
                "this query requires historical data; provide --store <path> to use a telemetry store"
            )
        })?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let result = if let Some(secs) = cli.timeout {
            tokio::time::timeout(
                std::time::Duration::from_secs(secs),
                executor::execute_with_store(&parsed.query, &store),
            )
            .await
            .map_err(|_| anyhow::anyhow!("query timed out after {secs}s"))??
        } else {
            executor::execute_with_store(&parsed.query, &store).await?
        };
        output_result(&result)?;
        run_exports(&cli, &result).await?;
        return Ok(());
    }

    // ── Alert mode with optional timeout ──────────────────────────────
    if let Query::Alert(rule) = &parsed.query {
        // When --watch is set, alert is handled in the watch loop below with the specified interval.
        if cli.watch.is_none() {
            let alert_docker = BollardDockerClient::connect_with_config(&config)
                .ok()
                .map(std::sync::Arc::new);
            let mut evaluator = AlertEvaluator::new();
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(ALERT_EVAL_INTERVAL));

            let store: Option<Arc<Mutex<SqliteTelemetryStore>>> = cli
                .store
                .as_deref()
                .or(config.store.as_deref())
                .map(SqliteTelemetryStore::open)
                .transpose()?
                .map(|s| Arc::new(Mutex::new(s)));

            println!("Evaluating alert... (Ctrl+C to stop)");
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        break;
                    }
                    _ = interval.tick() => {
                        // Only the metrics.collect() call is blocking; the evaluator
                        // is in-memory computation. Spawn just the collect.
                        let samples = if let (Some(secs), Some(ad)) = (cli.timeout, alert_docker.as_ref()) {
                            let m = BollardMetricsCollector::with_config(std::sync::Arc::clone(ad), &config);
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(secs),
                                m.collect(),
                            )
                            .await
                            {
                                Ok(Ok(s)) => s,
                                Ok(Err(e)) => {
                                    eprintln!("Metrics collection error: {e}");
                                    continue;
                                }
                                Err(_) => {
                                    eprintln!("metrics collection timed out after {secs}s");
                                    continue;
                                }
                            }
                        } else if let Some(ref ad) = alert_docker {
                            match BollardMetricsCollector::with_config(std::sync::Arc::clone(ad), &config).collect().await {
                                Ok(s) => s,
                                Err(e) => {
                                    eprintln!("Metrics collection error: {e}");
                                    continue;
                                }
                            }
                        } else {
                            eprintln!("[metrics] Docker not connected — skipping collection");
                            continue;
                        };

                        if let Some(ref store) = store
                            && let Ok(mut s) = store.lock() {
                                for sample in &samples {
                                    let _ = s.write_metric(sample.clone());
                                }
                            }

                        match evaluator.evaluate_samples(rule, &samples, std::time::Instant::now()) {
                            Ok(events) => {
                                for event in events {
                                    match output_format {
                                        OutputFormat::Table => println!("{}", alerts::render_alert_event(&event)),
                                        OutputFormat::Json | OutputFormat::JsonCompact | OutputFormat::Jsonl => println!("{}", serde_json::to_string(&event)?),
                                        OutputFormat::Csv => println!("{},{},{:?}", event.container_name, event.message, event.action),
                                    }
                                }
                            }
                            Err(e) => eprintln!("Alert evaluation error: {e}"),
                        }
                    }
                }
            }
        }
        // If --watch is set, fall through to the watch loop below
    }

    // ── Events streaming with optional auto-stop timeout ──────────────
    if let Query::Events(events_query) = &parsed.query {
        let docker = std::sync::Arc::new(BollardDockerClient::connect_with_config(&config)?);
        let source = BollardEventSource::new(std::sync::Arc::clone(&docker));

        let store: Option<Arc<Mutex<SqliteTelemetryStore>>> = cli
            .store
            .as_deref()
            .or(config.store.as_deref())
            .map(SqliteTelemetryStore::open)
            .transpose()?
            .map(|s| Arc::new(Mutex::new(s)));

        let (event_callback_store, event_callback_fmt) = (store.clone(), output_format);
        let event_callback =
            move |row: crate::executor::Row| -> Result<(), crate::events::EventsError> {
                if let Some(ref store) = event_callback_store
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

                let result = ExecutionResult { rows: vec![row] };
                match event_callback_fmt {
                    OutputFormat::Table => {
                        println!("{}", render_table(&result));
                    }
                    OutputFormat::Json | OutputFormat::JsonCompact | OutputFormat::Jsonl => {
                        println!(
                            "{}",
                            serde_json::to_string(&result.rows[0])
                                .map_err(crate::events::EventsError::Json)?
                        );
                    }
                    OutputFormat::Csv => {
                        let mut columns: Vec<String> =
                            result.rows[0].fields.keys().cloned().collect();
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
                }
                Ok(())
            };

        if let Some(secs) = cli.timeout {
            tokio::time::timeout(
                std::time::Duration::from_secs(secs),
                events::stream_events(events_query, &source, &event_callback),
            )
            .await
            .map_err(|_| anyhow::anyhow!("events stream timed out after {secs}s"))??;
        } else {
            events::stream_events(events_query, &source, &event_callback).await?;
        }
        return Ok(());
    }

    // ── Alert state for --watch (stateful evaluator persists across iterations) ──
    let mut alert_evaluator = match &parsed.query {
        Query::Alert(_) => Some(AlertEvaluator::new()),
        _ => None,
    };
    let alert_rule: Option<crate::ast::AlertRule> = match &parsed.query {
        Query::Alert(r) => Some(r.clone()),
        _ => None,
    };
    let alert_store: Option<Arc<Mutex<SqliteTelemetryStore>>> = if alert_rule.is_some() {
        cli.store
            .as_deref()
            .or(config.store.as_deref())
            .map(SqliteTelemetryStore::open)
            .transpose()?
            .map(|s| Arc::new(Mutex::new(s)))
    } else {
        None
    };

    // ── Batch query with optional --watch ──────────────────────────────
    let docker = std::sync::Arc::new(BollardDockerClient::connect_with_config(&config)?);
    let metrics = BollardMetricsCollector::with_config(std::sync::Arc::clone(&docker), &config);

    if let Some(interval_secs) = cli.watch {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = interval.tick() => {
                    if let (Some(ev), Some(rule)) = (&mut alert_evaluator, &alert_rule) {
                        // ── Alert evaluation in --watch loop ──
                        let samples = if let Some(secs) = cli.timeout {
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(secs),
                                BollardMetricsCollector::with_config(std::sync::Arc::clone(&docker), &config).collect(),
                            )
                            .await
                            {
                                Ok(Ok(s)) => s,
                                Ok(Err(e)) => {
                                    eprintln!("Metrics collection error: {e}");
                                    continue;
                                }
                                Err(_) => {
                                    eprintln!("metrics collection timed out after {secs}s");
                                    continue;
                                }
                            }
                        } else {
                            match                        BollardMetricsCollector::with_config(std::sync::Arc::clone(&docker), &config).collect().await {
                                Ok(s) => s,
                                Err(e) => {
                                    eprintln!("Metrics collection error: {e}");
                                    continue;
                                }
                            }
                        };

                        if let Some(ref store) = alert_store
                            && let Ok(mut s) = store.lock() {
                                for sample in &samples {
                                    let _ = s.write_metric(sample.clone());
                                }
                            }

                        match ev.evaluate_samples(rule, &samples, std::time::Instant::now()) {
                            Ok(events) => {
                                for event in events {
                                    match output_format {
                                        OutputFormat::Table => println!("{}", alerts::render_alert_event(&event)),
                                        OutputFormat::Json | OutputFormat::JsonCompact | OutputFormat::Jsonl => println!("{}", serde_json::to_string(&event)?),
                                        OutputFormat::Csv => println!("{},{},{:?}", event.container_name, event.message, event.action),
                                    }
                                }
                            }
                            Err(e) => eprintln!("Alert evaluation error: {e}"),
                        }
                    } else {
                        // ── Batch query execution ──
                        let result = if let Some(secs) = cli.timeout {
                            let q = parsed.query.clone();                                    tokio::time::timeout(
                                std::time::Duration::from_secs(secs),
                                executor::execute_with_metrics(&q, docker.as_ref(), &metrics),
                            )
                            .await
                            .map_err(|_| anyhow::anyhow!("query timed out after {secs}s"))?
                        } else {
                            executor::execute_with_metrics(&parsed.query, docker.as_ref(), &metrics).await
                        };

                        match result {
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
        }
        return Ok(());
    }

    // ── Single batch query with optional timeout ──────────────────────
    let result = if let Some(secs) = cli.timeout {
        tokio::time::timeout(
            std::time::Duration::from_secs(secs),
            executor::execute_with_metrics(&parsed.query, docker.as_ref(), &metrics),
        )
        .await
        .map_err(|_| anyhow::anyhow!("query timed out after {secs}s"))?
    } else {
        executor::execute_with_metrics(&parsed.query, docker.as_ref(), &metrics).await
    }?;

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
async fn run_exports(cli: &Cli, result: &ExecutionResult) -> anyhow::Result<()> {
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
const fn needs_store(query: &Query) -> bool {
    match query {
        Query::Inspect(q) => q.at.is_some(),
        Query::Events(q) => q.time.is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Timeout for tests that expect run() to loop indefinitely (no Docker available).
    const TEST_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

    #[tokio::test]
    async fn rejects_empty_query() {
        let cli = Cli {
            query: Some("   ".to_owned()),
            file: None,
            output: None,
            store: None,
            collect: false,
            metrics_interval: DEFAULT_METRICS_INTERVAL,
            snapshot_interval: DEFAULT_SNAPSHOT_INTERVAL,
            store_stats: false,
            apply_retention: false,
            explain: false,
            watch: None,
            timeout: None,
            export: None,
            export_format: None,
            export_influx: None,
            export_grafana_loki: None,
            export_prometheus: None,
            host: None,
            completion: None,
            diff: false,
            theme: None,
            command: None,
        };

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("empty DOL query"));
    }

    #[tokio::test]
    async fn historical_query_requires_store_flag() {
        let cli = Cli {
            query: Some("inspect container api at \"2026-01-01 12:00:00\"".to_owned()),
            file: None,
            output: None,
            store: None,
            collect: false,
            metrics_interval: DEFAULT_METRICS_INTERVAL,
            snapshot_interval: DEFAULT_SNAPSHOT_INTERVAL,
            store_stats: false,
            apply_retention: false,
            explain: false,
            watch: None,
            timeout: None,
            export: None,
            export_format: None,
            export_influx: None,
            export_grafana_loki: None,
            export_prometheus: None,
            host: None,
            completion: None,
            diff: false,
            theme: None,
            command: None,
        };

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("--store"));
    }

    #[test]
    fn test_needs_store_inspect_at_returns_true() {
        let query = parser::parse("inspect container test at \"2026-01-01T00:00:00Z\"")
            .expect("parse")
            .query;
        assert!(needs_store(&query));
    }

    #[test]
    fn test_needs_store_events_time_returns_true() {
        let query = parser::parse(
            "events containers from \"2026-01-01T00:00:00Z\" to \"2026-01-02T00:00:00Z\"",
        )
        .expect("parse")
        .query;
        assert!(needs_store(&query));
    }

    #[test]
    fn test_needs_store_plain_observe_returns_false() {
        let query = parser::parse("observe containers").expect("parse").query;
        assert!(!needs_store(&query));
    }

    #[test]
    fn test_needs_store_alert_returns_false() {
        let query = parser::parse("alert when cpu > 80% for 30s then print \"alert\"")
            .expect("parse")
            .query;
        assert!(!needs_store(&query));
    }

    #[test]
    fn test_needs_store_plain_events_returns_false() {
        let query = parser::parse("events containers").expect("parse").query;
        assert!(!needs_store(&query));
    }

    #[test]
    fn test_apply_host_sequential() {
        let original = std::env::var("DOCKER_HOST").ok();

        // Test 1: CLI host sets env var
        unsafe { std::env::remove_var("DOCKER_HOST"); }
        apply_host(Some("tcp://192.168.1.100:2375"), None);
        assert_eq!(
            std::env::var("DOCKER_HOST").unwrap(),
            "tcp://192.168.1.100:2375"
        );

        // Test 2: CLI host takes precedence over config host
        apply_host(Some("tcp://cli:2375"), Some("tcp://config:2375"));
        assert_eq!(std::env::var("DOCKER_HOST").unwrap(), "tcp://cli:2375");

        // Test 3: Without CLI host, config host is used
        unsafe { std::env::remove_var("DOCKER_HOST"); }
        apply_host(None, Some("tcp://config:2375"));
        assert_eq!(std::env::var("DOCKER_HOST").unwrap(), "tcp://config:2375");

        // Test 4: No args does nothing
        unsafe { std::env::remove_var("DOCKER_HOST"); }
        apply_host(None, None);
        assert_eq!(std::env::var("DOCKER_HOST").unwrap_or_default(), "");

        // Restore original
        match original {
            Some(v) => unsafe { std::env::set_var("DOCKER_HOST", v); },
            None => unsafe { std::env::remove_var("DOCKER_HOST"); },
        }
    }

    #[test]
    fn test_output_format_value_enum() {
        // Verify the enum has all expected variants
        assert!(matches!(OutputFormat::Table, OutputFormat::Table));
        assert!(matches!(OutputFormat::Json, OutputFormat::Json));
        assert!(matches!(
            OutputFormat::JsonCompact,
            OutputFormat::JsonCompact
        ));
        assert!(matches!(OutputFormat::Csv, OutputFormat::Csv));
        assert!(matches!(OutputFormat::Jsonl, OutputFormat::Jsonl));
    }

    #[test]
    fn test_output_format_jsoncompact_is_distinct() {
        // JsonCompact must be a separate variant from Json
        match OutputFormat::JsonCompact {
            OutputFormat::Json => panic!("JsonCompact should not equal Json"),
            OutputFormat::JsonCompact => {}
            _ => {}
        }
    }

    #[tokio::test]
    async fn watch_with_alert_runs_without_panic() {
        let cli = Cli {
            query: Some(r#"alert when cpu > 80% for 30s then print "High""#.to_owned()),
            file: None,
            watch: Some(1),
            output: None,
            store: None,
            collect: false,
            metrics_interval: DEFAULT_METRICS_INTERVAL,
            snapshot_interval: DEFAULT_SNAPSHOT_INTERVAL,
            store_stats: false,
            apply_retention: false,
            explain: false,
            timeout: None,
            export: None,
            export_format: None,
            export_influx: None,
            export_grafana_loki: None,
            export_prometheus: None,
            host: None,
            completion: None,
            diff: false,
            theme: None,
            command: None,
        };

        // Watch loop runs indefinitely; use timeout to verify no panic
        let result = tokio::time::timeout(TEST_RUN_TIMEOUT, run(cli)).await;

        assert!(
            result.is_err(),
            "watch+alert loop should run until timeout (no docker)"
        );
    }

    // Flaky on Windows CI due to tokio::signal::ctrl_c() behaving differently
    // in non-TTY environments, causing the loop to exit early.
    #[cfg_attr(target_os = "windows", ignore = "flaky on Windows (ctrl_c resolves early in CI)")]
    #[tokio::test]
    async fn watch_with_alert_with_timeout() {
        let cli = Cli {
            query: Some(r#"alert when cpu > 80% for 30s then print "High""#.to_owned()),
            file: None,
            watch: Some(5),
            output: None,
            store: None,
            collect: false,
            metrics_interval: DEFAULT_METRICS_INTERVAL,
            snapshot_interval: DEFAULT_SNAPSHOT_INTERVAL,
            store_stats: false,
            apply_retention: false,
            explain: false,
            timeout: Some(1),
            export: None,
            export_format: None,
            export_influx: None,
            export_grafana_loki: None,
            export_prometheus: None,
            host: None,
            completion: None,
            diff: false,
            theme: None,
            command: None,
        };

        // With --timeout, the collect() call uses spawn_blocking inside the watch loop.
        // Without docker, metrics collection fails and prints error, loop continues.
        let result = tokio::time::timeout(TEST_RUN_TIMEOUT, run(cli)).await;

        assert!(
            result.is_err(),
            "watch+alert with timeout should still loop indefinitely"
        );
    }

    #[tokio::test]
    async fn alert_without_watch_runs_dedicated_loop() {
        let cli = Cli {
            query: Some(r#"alert when cpu > 80% for 30s then print "High""#.to_owned()),
            file: None,
            watch: None,
            output: None,
            store: None,
            collect: false,
            metrics_interval: DEFAULT_METRICS_INTERVAL,
            snapshot_interval: DEFAULT_SNAPSHOT_INTERVAL,
            store_stats: false,
            apply_retention: false,
            explain: false,
            timeout: None,
            export: None,
            export_format: None,
            export_influx: None,
            export_grafana_loki: None,
            export_prometheus: None,
            host: None,
            completion: None,
            diff: false,
            theme: None,
            command: None,
        };

        // Dedicated alert loop also runs indefinitely
        let result = tokio::time::timeout(TEST_RUN_TIMEOUT, run(cli)).await;

        assert!(
            result.is_err(),
            "dedicated alert loop should run until timeout (no docker)"
        );
    }
}
