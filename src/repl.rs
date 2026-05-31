use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, EditMode, Editor, Helper};

use crate::{
    ast::Query,
    config::DolConfig,
    docker::DockerCliClient,
    executor::{self, render_table},
    metrics::{DockerCliMetricsCollector, MetricsCollector},
    parser,
    sqlite_store::SqliteTelemetryStore,
};

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
        let        keywords = [
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
                ".help" => print_repl_help(),
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
                        println!("{:5}  {entry}", i + 1);
                    }
                }
                cmd if cmd.starts_with(".watch ") => {
                    let val = cmd.strip_prefix(".watch ").unwrap_or("").trim();
                    match val.parse::<u64>() {
                        Ok(secs) if secs > 0 => {
                            println!("Watch every {secs}s enabled");
                        }
                        _ => {
                            println!("Watch disabled");
                        }
                    }
                }
                cmd if cmd.starts_with(".export ") => {
                    let path = cmd.strip_prefix(".export ").unwrap_or("").trim();
                    println!("Export to: {path}");
                }
                cmd if cmd.starts_with(".output ") => {
                    let fmt = cmd.strip_prefix(".output ").unwrap_or("").trim();
                    println!("Output format: {fmt}");
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

        if let Err(e) = execute_dol_query(&trimmed, config).await {
            eprintln!("Error: {e}");
        }
    }

    if let Some(ref path) = history_file {
        let _ = rl.save_history(path);
    }

    Ok(())
}

fn print_repl_help() {
    println!("DOL REPL Commands:");
    println!("  .help              Show this help");
    println!("  .exit / .quit      Exit the REPL");
    println!("  .host [<addr>]     Show or set Docker host (e.g. tcp://192.168.1.100:2375)");
    println!("  .history           Show command history");
    println!("  .watch <secs>      Re-run last query every N seconds");
    println!("  .export <path>     Export results to file");
    println!("  .output <fmt>      Set output format (table, json, csv, jsonl)");
    println!();
    println!("DOL Queries:");
    println!("  observe containers");
    println!("  events containers where action = die");
    println!("  inspect container <name>");
    println!("  ... and any other DOL query");
}

async fn execute_dol_query(query: &str, config: &DolConfig) -> anyhow::Result<()> {
    let parsed = parser::parse(query)?;

    let needs_store = match &parsed.query {
        Query::Inspect(q) => q.at.is_some(),
        Query::Events(q) => q.time.is_some(),
        _ => false,
    };

    if needs_store {
        let store_path = config.store.as_deref().ok_or_else(|| {
            anyhow::anyhow!("this query requires historical data; set store in config")
        })?;
        let store = SqliteTelemetryStore::open(store_path)?;
        let result = executor::execute_with_store(&parsed.query, &store)?;
        println!("{}", render_table(&result));
        return Ok(());
    }

    if let Query::Alert(rule) = &parsed.query {
        let metrics = DockerCliMetricsCollector::default();
        let mut evaluator = crate::alerts::AlertEvaluator::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

        println!("Evaluating alert... (Ctrl+C to stop)");
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = interval.tick() => {
                    let samples = metrics.collect()?;
                    let events = evaluator.evaluate_samples(rule, &samples, std::time::Instant::now())?;
                    for event in events {
                        println!("{}", crate::alerts::render_alert_event(&event));
                    }
                }
            }
        }
        return Ok(());
    }

    if let Query::Events(events_query) = &parsed.query {
        let source = crate::events::DockerCliEventSource::default();
        return crate::events::stream_events(events_query, &source, |row| {
            let result = executor::ExecutionResult { rows: vec![row] };
            println!("{}", render_table(&result));
            Ok(())
        })
        .map_err(Into::into);
    }

    // Batch query.
    let docker = DockerCliClient::default();
    let metrics = DockerCliMetricsCollector::default();
    let result = executor::execute_with_metrics(&parsed.query, &docker, &metrics)?;
    println!("{}", render_table(&result));

    Ok(())
}
