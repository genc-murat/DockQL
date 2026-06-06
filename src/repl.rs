//! Interactive REPL (Read-Eval-Print Loop).
//!
//! Provides a [`rustyline`]-based shell with tab completion, command
//! history, and syntax-coloured error messages. Supports all DOL query
//! types plus meta-commands (`.help`, `.exit`, `.host`, `.watch`, etc.).
//!
//! # Example
//!
//! ```ignore
//! repl::run_repl(&config).await?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, EditMode, Editor, Helper};

use crate::{
    ALERT_EVAL_INTERVAL, ANSI_BOLD, ANSI_FG_RED, ANSI_RESET,
    ast::Query,
    config::DolConfig,
    docker::BollardDockerClient,
    eval::EvalError,
    executor::{self, render_csv, render_jsonl, render_table},
    metrics::{BollardMetricsCollector, MetricsCollector},
    parser,
    sqlite_store::SqliteTelemetryStore,
};

// ── REPL state ─────────────────────────────────────────────────

/// Tracks persistent state across REPL commands.
#[derive(Default)]
struct ReplState {
    /// The last DOL query that was successfully parsed and executed.
    last_query: Option<String>,
    /// If set, re-run `last_query` every N seconds after executing a query.
    watch_interval_secs: Option<u64>,
    /// If set, write query results to this file path.
    export_path: Option<String>,
    /// Output format for displaying / exporting results: "table", "json", "csv", or "jsonl".
    output_format: String,
}

impl ReplState {
    fn render_result(&self, result: &executor::ExecutionResult) -> String {
        match self.output_format.as_str() {
            "json" => serde_json::to_string_pretty(result).unwrap_or_default(),
            "csv" => render_csv(result),
            "jsonl" => render_jsonl(result),
            _ => render_table(result),
        }
    }

    fn export_result(&self, result: &executor::ExecutionResult) -> anyhow::Result<()> {
        let path = match &self.export_path {
            Some(p) => p.clone(),
            None => return Ok(()),
        };
        let text = self.render_result(result);
        std::fs::write(&path, &text)?;
        println!("   Exported {} rows to {path}", result.rows.len());
        Ok(())
    }
}

#[derive(Default)]
struct DolHelper;

impl Helper for DolHelper {}

impl Validator for DolHelper {
    fn validate(
        &self,
        _ctx: &mut rustyline::validate::ValidationContext,
    ) -> rustyline::Result<rustyline::validate::ValidationResult> {
        Ok(rustyline::validate::ValidationResult::Valid(None))
    }
}

impl Completer for DolHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> Result<(usize, Vec<Pair>), ReadlineError> {
        let keywords = [
            "observe",
            "events",
            "inspect",
            "analyze",
            "alert",
            "fields",
            "containers",
            "images",
            "networks",
            "volumes",
            "container",
            "image",
            "network",
            "volume",
            "where",
            "select",
            "sort",
            "by",
            "limit",
            "group",
            "having",
            "offset",
            "distinct",
            "fill",
            "let",
            "set",
            "if",
            "then",
            "else",
            "case",
            "when",
            "end",
            "and",
            "or",
            "not",
            "in",
            "matches",
            "contains",
            "starts_with",
            "ends_with",
            "between",
            "row_number",
            "rank",
            "lag",
            "lead",
            "debug",
            "assert",
            "last",
            "at",
            "from",
            "to",
            "asc",
            "desc",
            ".help",
            ".exit",
            ".host",
            ".watch",
            ".export",
            ".output",
            ".history",
            "logs",
            "ls",
            "compose",
            "services",
            "health",
            "stats",
            "ps",
            "port",
            "config",
            "find",
            "anomalies",
            "correlate",
            "explain",
            "print",
            "webhook",
            "restart",
        ];

        let line_before = &line[..pos];
        let word = line_before.split_whitespace().last().unwrap_or("");
        let partial = word.to_lowercase();

        let mut pairs = Vec::new();
        for kw in &keywords {
            if kw.starts_with(&partial) {
                pairs.push(Pair {
                    display: kw.to_string(),
                    replacement: kw.to_string(),
                });
            }
        }
        Ok((pos - word.len(), pairs))
    }
}

impl Hinter for DolHelper {
    type Hint = String;
}

impl Highlighter for DolHelper {}

pub async fn run_repl(config: &DolConfig) -> anyhow::Result<()> {
    // Initialise global alert timeout config from loaded config.
    crate::alerts::init_alert_timeouts(
        config.webhook_timeout.unwrap_or(10),
        config.restart_timeout.unwrap_or(30),
    );

    // Apply config host on startup so all Docker clients use it.
    let mut current_host = config.host.clone().unwrap_or_default();
    if !current_host.is_empty() {
        // SAFETY: single-threaded startup — no concurrent env access.
        unsafe {
            std::env::set_var("DOCKER_HOST", &current_host);
        }
    }

    let h = Config::builder()
        .history_ignore_space(true)
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .build();

    let mut rl = Editor::<DolHelper, DefaultHistory>::with_config(h)?;
    rl.set_helper(Some(DolHelper));

    let history_file = dirs::data_dir().map(|d| d.join("dol").join("repl_history.txt"));

    if let Some(ref path) = history_file {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = rl.load_history(path);
    }

    let mut state = ReplState {
        output_format: "table".to_owned(),
        ..ReplState::default()
    };

    println!("DOL REPL — type .help for commands, Ctrl+C or .exit to quit");
    if current_host.is_empty() {
        println!("   Connected to: local Docker socket");
    } else {
        println!("   Connected to: {current_host}");
    }

    loop {
        let input = match rl.readline("dol> ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                eprintln!("Readline error: {e}");
                break;
            }
        };

        let trimmed = input.trim().to_owned();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('.') {
            match trimmed.as_str() {
                ".exit" | ".quit" => break,
                ".help" => print_repl_help(&state),
                cmd if cmd.starts_with(".host") => {
                    let val = cmd.strip_prefix(".host").unwrap_or("").trim();
                    if val.is_empty() {
                        if current_host.is_empty() {
                            println!("Docker host: local socket");
                        } else {
                            println!("Docker host: {current_host}");
                        }
                    } else {
                        current_host = val.to_owned();
                        // SAFETY: REPL is single-threaded for user input.
                        unsafe {
                            std::env::set_var("DOCKER_HOST", &current_host);
                        }
                        println!("Docker host set to: {current_host}");
                    }
                }
                ".history" => {
                    for (i, entry) in rl.history().iter().enumerate() {
                        println!("{i:5}  {entry}");
                    }
                }
                cmd if cmd.starts_with(".watch") => {
                    let val = cmd.strip_prefix(".watch").unwrap_or("").trim();
                    if val.is_empty() {
                        // No argument: toggle off or show current state
                        match state.watch_interval_secs {
                            Some(secs) => {
                                println!("Watch disabled (was every {secs}s)");
                                state.watch_interval_secs = None;
                            }
                            None => {
                                println!("No watch interval set. Use `.watch <secs>` to enable.");
                            }
                        }
                    } else {
                        match val.parse::<u64>() {
                            Ok(secs) if secs > 0 => {
                                state.watch_interval_secs = Some(secs);
                                println!(
                                    "Watch every {secs}s enabled (will activate on next query)"
                                );
                            }
                            _ => {
                                println!(
                                    "Invalid interval: {val}. Use a positive number (seconds)."
                                );
                            }
                        }
                    }
                }
                cmd if cmd.starts_with(".export") => {
                    let val = cmd.strip_prefix(".export").unwrap_or("").trim();
                    if val.is_empty() || val == "off" {
                        state.export_path = None;
                        println!("Export disabled");
                    } else {
                        state.export_path = Some(val.to_owned());
                        println!("Export to: {val}");
                    }
                }
                cmd if cmd.starts_with(".output") => {
                    let val = cmd.strip_prefix(".output").unwrap_or("").trim();
                    match val {
                        "table" | "json" | "csv" | "jsonl" => {
                            state.output_format = val.to_owned();
                            println!("Output format set to: {val}");
                        }
                        "" => {
                            println!("Current output format: {}", state.output_format);
                        }
                        _ => {
                            println!("Unknown format: {val}. Valid: table, json, csv, jsonl");
                        }
                    }
                }
                _ => {
                    println!("Unknown command: {trimmed}");
                    println!("Type .help for available commands");
                }
            }
            rl.add_history_entry(trimmed.as_str())?;
            if let Some(ref path) = history_file {
                let _ = rl.save_history(path);
            }
            continue;
        }

        rl.add_history_entry(trimmed.as_str())?;
        if let Some(ref path) = history_file {
            let _ = rl.save_history(path);
        }

        // Save as last query for .watch
        state.last_query = Some(trimmed.clone());

        // Determine the query to run: if watch is active, use last_query
        let query_to_run = trimmed.clone();

        // ── Execute query ──
        if let Err(e) = execute_and_output(&query_to_run, config, &state).await {
            let msg = format_error_color(&e);
            eprintln!("{msg}");
        }

        // ── Watch loop ──
        if let Some(interval) = state.watch_interval_secs {
            println!("Watching every {interval}s (Ctrl+C to stop)...");
            let watch_query = state.last_query.clone().unwrap_or_default();
            run_watch_loop(&watch_query, config, &state, interval).await;
        }
    }

    if let Some(ref path) = history_file {
        let _ = rl.save_history(path);
    }

    Ok(())
}

/// Run the last query repeatedly every `interval` seconds until Ctrl+C.
async fn run_watch_loop(query: &str, config: &DolConfig, state: &ReplState, interval: u64) {
    let mut tick = tokio::time::interval(Duration::from_secs(interval));
    // Tick immediately so the first iteration runs without waiting.
    tick.tick().await;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!();
                break;
            }
            _ = tick.tick() => {
                if let Err(e) = execute_and_output(query, config, state).await {
                    let msg = format_error_color(&e);
                    eprintln!("{msg}");
                }
            }
        }
    }
}

/// Parse and execute a DOL query, then display the results according to
/// the current `ReplState` (output format, export path).
async fn execute_and_output(
    query: &str,
    config: &DolConfig,
    state: &ReplState,
) -> anyhow::Result<()> {
    let parsed = parser::parse(query)?;

    match &parsed.query {
        Query::Inspect(q) if q.at.is_some() => {
            let store_path = config.store.as_deref().ok_or_else(|| {
                anyhow::anyhow!("historical query requires a store; set `store` in config")
            })?;
            let store = SqliteTelemetryStore::open(store_path)?;
            let result = executor::execute_with_store(&parsed.query, &store).await?;
            let output = state.render_result(&result);
            println!("{output}");
            state.export_result(&result)?;
            return Ok(());
        }
        Query::Events(q) if q.time.is_some() => {
            let store_path = config.store.as_deref().ok_or_else(|| {
                anyhow::anyhow!("historical query requires a store; set `store` in config")
            })?;
            let store = SqliteTelemetryStore::open(store_path)?;
            let result = executor::execute_with_store(&parsed.query, &store).await?;
            let output = state.render_result(&result);
            println!("{output}");
            state.export_result(&result)?;
            return Ok(());
        }
        _ => {}
    }

    match &parsed.query {
        Query::Alert(rule) => {
            let docker = BollardDockerClient::connect_with_config(config)?;
            let docker = Arc::new(docker);
            let metrics = BollardMetricsCollector::with_config(Arc::clone(&docker), config);
            let mut evaluator = crate::alerts::AlertEvaluator::new();
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(ALERT_EVAL_INTERVAL));

            println!("Evaluating alert... (Ctrl+C to stop)");
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = interval.tick() => {
                        let samples = metrics.collect().await?;
                        let events = evaluator.evaluate_samples(rule, &samples, std::time::Instant::now())?;
                        for event in events {
                            println!("{}", crate::alerts::render_alert_event(&event));
                        }
                    }
                }
            }
            return Ok(());
        }
        Query::Events(events_query) => {
            let docker = Arc::new(BollardDockerClient::connect_with_config(config)?);
            let source = crate::events::BollardEventSource::new(Arc::clone(&docker));
            return crate::events::stream_events(events_query, &source, move |row| {
                let result = executor::ExecutionResult { rows: vec![row] };
                // In streaming mode, use the current state for rendering
                let text = match state.output_format.as_str() {
                    "json" => serde_json::to_string_pretty(&result).unwrap_or_default(),
                    "csv" => render_csv(&result),
                    "jsonl" => {
                        let jsonl = render_jsonl(&result);
                        if jsonl.is_empty() {
                            String::new()
                        } else {
                            jsonl + "\n"
                        }
                    }
                    _ => render_table(&result),
                };
                if !text.is_empty() {
                    print!("{text}");
                }
                Ok(())
            })
            .await
            .map_err(Into::into);
        }
        Query::Logs(logs_query) => {
            let docker = Arc::new(BollardDockerClient::connect_with_config(config)?);
            let source = crate::events::BollardLogSource::new(Arc::clone(&docker));
            return crate::events::stream_logs(logs_query, &source, move |row| {
                let result = executor::ExecutionResult { rows: vec![row] };
                let text = match state.output_format.as_str() {
                    "json" => serde_json::to_string_pretty(&result).unwrap_or_default(),
                    "csv" => render_csv(&result),
                    "jsonl" => {
                        let jsonl = render_jsonl(&result);
                        if jsonl.is_empty() {
                            String::new()
                        } else {
                            jsonl + "\n"
                        }
                    }
                    _ => render_table(&result),
                };
                if !text.is_empty() {
                    print!("{text}");
                }
                Ok(())
            })
            .await
            .map_err(Into::into);
        }
        Query::Compose(compose_query)
            if compose_query.target == crate::ast::ComposeTarget::Networks
                && !compose_query.pipeline.is_empty() =>
        {
            let docker = Arc::new(BollardDockerClient::connect_with_config(config)?);
            return crate::events::stream_compose_networks(compose_query, docker, move |row| {
                let result = executor::ExecutionResult { rows: vec![row] };
                let text = match state.output_format.as_str() {
                    "json" => serde_json::to_string_pretty(&result).unwrap_or_default(),
                    "csv" => render_csv(&result),
                    "jsonl" => {
                        let jsonl = render_jsonl(&result);
                        if jsonl.is_empty() {
                            String::new()
                        } else {
                            jsonl + "\n"
                        }
                    }
                    _ => render_table(&result),
                };
                if !text.is_empty() {
                    print!("{text}");
                }
                Ok(())
            })
            .await
            .map_err(Into::into);
        }
        Query::Compose(compose_query)
            if compose_query.target == crate::ast::ComposeTarget::Logs =>
        {
            let docker = Arc::new(BollardDockerClient::connect_with_config(config)?);
            return crate::events::stream_compose_logs(compose_query, docker, move |row| {
                let result = executor::ExecutionResult { rows: vec![row] };
                let text = match state.output_format.as_str() {
                    "json" => serde_json::to_string_pretty(&result).unwrap_or_default(),
                    "csv" => render_csv(&result),
                    "jsonl" => {
                        let jsonl = render_jsonl(&result);
                        if jsonl.is_empty() {
                            String::new()
                        } else {
                            jsonl + "\n"
                        }
                    }
                    _ => render_table(&result),
                };
                if !text.is_empty() {
                    print!("{text}");
                }
                Ok(())
            })
            .await
            .map_err(Into::into);
        }
        _ => {}
    }

    // Batch query.
    let docker = Arc::new(BollardDockerClient::connect_with_config(config)?);
    let metrics = BollardMetricsCollector::with_config(Arc::clone(&docker), config);
    let result = executor::execute_with_metrics(&parsed.query, docker, &metrics).await?;
    let output = state.render_result(&result);
    println!("{output}");
    state.export_result(&result)?;

    Ok(())
}

fn print_repl_help(state: &ReplState) {
    println!("DOL REPL Commands:");
    println!("  .help              Show this help");
    println!("  .exit / .quit      Exit the REPL");
    println!("  .host [<addr>]     Show or set Docker host (e.g. tcp://192.168.1.100:2375)");
    println!("  .history           Show command history");
    println!("  .watch [<secs>]    Re-run last query every N seconds (no arg to disable)");
    println!("  .export <path>     Export results to file (.export off to disable)");
    println!("  .output <fmt>      Set output format: table, json, csv, jsonl");
    println!();
    println!("Current settings:");
    println!(
        "  output: {}  |  export: {}  |  watch: {}",
        state.output_format,
        state.export_path.as_deref().unwrap_or("off"),
        state
            .watch_interval_secs
            .map_or("off".to_owned(), |s| format!("{s}s"))
    );
    println!();
    println!("DOL Queries:");
    println!("  observe containers");
    println!("  events containers where action = die");
    println!("  logs container <name>");
    println!("  logs container <name> tail 50");
    println!("  compose <project> logs service <name>");
    println!("  compose <project> logs service <name> tail 50");
    println!("  compose <project> networks");
    println!("  compose <project> events");
    println!("  Streaming compose targets: logs, networks — add a pipeline for live streaming");
    println!("  inspect container <name>");
    println!("  ... and any other DOL query");
}

/// Format an anyhow error with ANSI colours, checking for known error types.
fn format_error_color(err: &anyhow::Error) -> String {
    if let Some(parse_err) = err.downcast_ref::<parser::ParseError>() {
        return parse_err.format_color();
    }
    if let Some(eval_err) = err.downcast_ref::<EvalError>() {
        return eval_err.format_color();
    }
    // Fallback: bold red prefix + Display
    format!(
        "{bold}{red}error{reset}: {msg}",
        bold = ANSI_BOLD,
        red = ANSI_FG_RED,
        reset = ANSI_RESET,
        msg = err,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repl_state_defaults() {
        let state = ReplState::default();
        assert!(state.last_query.is_none());
        assert!(state.watch_interval_secs.is_none());
        assert!(state.export_path.is_none());
        assert_eq!(state.output_format, ""); // Default::default() for String is ""
    }

    #[test]
    fn test_repl_state_render_table_default() {
        let state = ReplState {
            output_format: "table".to_owned(),
            ..ReplState::default()
        };
        let result = executor::ExecutionResult { rows: vec![] };
        let output = state.render_result(&result);
        assert_eq!(output, "No rows");
    }

    #[test]
    fn test_repl_state_render_json() {
        let state = ReplState {
            output_format: "json".to_owned(),
            ..ReplState::default()
        };
        let result = executor::ExecutionResult { rows: vec![] };
        let output = state.render_result(&result);
        // to_string_pretty produces "rows": [...] (no space before colon)
        assert!(
            output.contains(r#""rows""#),
            "expected JSON output to contain 'rows' field, got: {output}"
        );
    }

    #[test]
    fn test_repl_state_export_none_does_nothing() {
        let state = ReplState::default();
        let result = executor::ExecutionResult { rows: vec![] };
        // Should not panic when export_path is None
        assert!(state.export_result(&result).is_ok());
    }

    #[test]
    fn test_print_repl_help_runs_without_panic() {
        let state = ReplState {
            output_format: "table".to_owned(),
            ..ReplState::default()
        };
        print_repl_help(&state);
    }

    #[test]
    fn test_execute_and_output_rejects_invalid_query() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let config = DolConfig::default();
        let state = ReplState {
            output_format: "table".to_owned(),
            ..ReplState::default()
        };
        let result = rt.block_on(execute_and_output("invalid query here", &config, &state));
        assert!(result.is_err());
    }

    #[test]
    fn test_docker_host_env_propagates_to_streaming_compose_networks() {
        let rt = tokio::runtime::Runtime::new().unwrap();

        // Save original DOCKER_HOST.
        let original = std::env::var("DOCKER_HOST").ok();

        // Simulate `.host tcp://127.0.0.1:1` — set an unreachable address.
        unsafe {
            std::env::set_var("DOCKER_HOST", "tcp://127.0.0.1:1");
        }

        let config = DolConfig::default();
        let state = ReplState {
            output_format: "table".to_owned(),
            ..ReplState::default()
        };

        // Run a compose networks STREAMING query (with pipeline).
        // The streaming dispatch will call connect_with_config → bollard reads DOCKER_HOST
        // → tries to connect to tcp://127.0.0.1:1 → fails with connection error.
        let result = rt.block_on(execute_and_output(
            "compose myapp networks | where action = connect",
            &config,
            &state,
        ));

        // Must fail (no real Docker at 127.0.0.1:1).
        assert!(
            result.is_err(),
            "expected connection error when DOCKER_HOST points to unreachable address"
        );
        let err = result.unwrap_err().to_string();
        // The error should be a Docker connection error, NOT a parse error.
        // Parse errors mention "parse error" — connection errors mention "connection",
        // "refused", "timeout", or just the bollard error.
        assert!(
            !err.contains("parse error"),
            "expected a connection/runtime error, but got a parse error: {err}"
        );

        // Restore original DOCKER_HOST.
        match original {
            Some(v) => unsafe {
                std::env::set_var("DOCKER_HOST", v);
            },
            None => unsafe {
                std::env::remove_var("DOCKER_HOST");
            },
        }
    }
}
