//! # DOL — Docker Observability Language
//!
//! A SQL-like query language for Docker infrastructure. Query containers,
//! stream events, set up alerts, analyze anomalies, and inspect historical
//! data — all from a single CLI.
//!
//! ## Quick start
//!
//! ```ignore
//! use dol::{Cli, cli};
//! let cli = Cli::parse_from(["dol", "observe containers"]);
//! cli::run(cli).await?;
//! ```
//!
//! ## Crate structure
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`parser`] | DOL query parser (tokenizer → AST) |
//! | [`ast`]    | Abstract syntax tree types |
//! | [`semantic`] | Type-checking and field validation |
//! | [`planner`]  | Logical query plan generation |
//! | [`executor`] | Batch query execution |
//! | [`eval`]    | Expression evaluation engine |
//! | [`docker`]  | Docker client abstraction (bollard) |
//! | [`metrics`] | Metrics collection |
//! | [`events`]  | Event streaming |
//! | [`alerts`]  | Alert rule evaluation |
//! | [`storage`] / [`sqlite_store`] | Telemetry persistence |
//! | [`cli`]     | CLI argument parsing and dispatch |
//! | [`repl`]    | Interactive REPL |
//! | [`dashboard`] | TUI dashboard and top |
//! | [`config`]  | Configuration management |
//! | [`export`]  | InfluxDB / Loki / Prometheus output |
//! | [`analyze`] | Anomaly detection and diagnostics |
//! | [`collector`] | Background telemetry collection |

#![doc(
    html_logo_url = "https://genc-murat.github.io/DockQL/logo.svg",
    html_favicon_url = "https://genc-murat.github.io/DockQL/logo.svg"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_lines,
    clippy::future_not_send,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod alerts;
pub mod analyze;
pub mod ast;
pub mod cli;
pub mod collector;
pub mod config;
pub mod dashboard;
pub mod docker;
pub mod eval;
pub mod events;
pub mod executor;
pub mod export;
pub mod metrics;
pub mod parser;
pub mod planner;
pub mod repl;
pub mod semantic;
pub mod sqlite_store;
pub mod storage;

pub use cli::Cli;
pub use cli::OutputFormat;
pub use export::ExportFormat;

/// Shared helper: wrap a `String` (or `&str`) into a `serde_json::Value::String`.
pub(crate) fn json_string(value: impl Into<String>) -> serde_json::Value {
    serde_json::Value::String(value.into())
}

/// Map a `CollectionTarget` to its single-letter prefix alias used in join queries.
pub(crate) const fn target_alias(target: ast::CollectionTarget) -> &'static str {
    match target {
        ast::CollectionTarget::Containers => "c",
        ast::CollectionTarget::Images => "i",
        ast::CollectionTarget::Networks => "n",
        ast::CollectionTarget::Volumes => "v",
    }
}

// ═══════════════════════════════════════════════════════════════════
// ANSI escape code constants
// ═══════════════════════════════════════════════════════════════════

/// ANSI reset — clears all formatting.
pub const ANSI_RESET: &str = "\x1b[0m";

/// ANSI style modifiers.
pub const ANSI_BOLD: &str = "\x1b[1m";
pub const ANSI_DIM: &str = "\x1b[2m";
pub const ANSI_ITALIC: &str = "\x1b[3m";
pub const ANSI_UNDERLINE: &str = "\x1b[4m";

/// ANSI foreground (text) colors — standard 3/4-bit palette.
pub const ANSI_FG_BLACK: &str = "\x1b[30m";
pub const ANSI_FG_RED: &str = "\x1b[31m";
pub const ANSI_FG_GREEN: &str = "\x1b[32m";
pub const ANSI_FG_YELLOW: &str = "\x1b[33m";
pub const ANSI_FG_BLUE: &str = "\x1b[34m";
pub const ANSI_FG_MAGENTA: &str = "\x1b[35m";
pub const ANSI_FG_CYAN: &str = "\x1b[36m";
pub const ANSI_FG_WHITE: &str = "\x1b[37m";

/// ANSI foreground colors — bright (90–97) palette.
pub const ANSI_FG_DARK_GRAY: &str = "\x1b[90m";
pub const ANSI_FG_LIGHT_RED: &str = "\x1b[91m";
pub const ANSI_FG_LIGHT_GREEN: &str = "\x1b[92m";
pub const ANSI_FG_LIGHT_YELLOW: &str = "\x1b[93m";
pub const ANSI_FG_LIGHT_BLUE: &str = "\x1b[94m";
pub const ANSI_FG_LIGHT_MAGENTA: &str = "\x1b[95m";
pub const ANSI_FG_LIGHT_CYAN: &str = "\x1b[96m";

/// ANSI background colors — standard 3/4-bit palette.
pub(crate) const ANSI_BG_BLACK: &str = "\x1b[40m";
pub(crate) const ANSI_BG_RED: &str = "\x1b[41m";
pub(crate) const ANSI_BG_GREEN: &str = "\x1b[42m";
pub(crate) const ANSI_BG_YELLOW: &str = "\x1b[43m";
pub(crate) const ANSI_BG_BLUE: &str = "\x1b[44m";
pub(crate) const ANSI_BG_MAGENTA: &str = "\x1b[45m";
pub(crate) const ANSI_BG_CYAN: &str = "\x1b[46m";
pub(crate) const ANSI_BG_WHITE: &str = "\x1b[47m";

/// ANSI background colors — bright (100–107) palette.
pub(crate) const ANSI_BG_DARK_GRAY: &str = "\x1b[100m";
pub(crate) const ANSI_BG_LIGHT_RED: &str = "\x1b[101m";
pub(crate) const ANSI_BG_LIGHT_GREEN: &str = "\x1b[102m";
pub(crate) const ANSI_BG_LIGHT_YELLOW: &str = "\x1b[103m";
pub(crate) const ANSI_BG_LIGHT_BLUE: &str = "\x1b[104m";
pub(crate) const ANSI_BG_LIGHT_MAGENTA: &str = "\x1b[105m";
pub(crate) const ANSI_BG_LIGHT_CYAN: &str = "\x1b[106m";

/// Bold red — used for critical/error states.
pub(crate) const ANSI_BOLD_RED: &str = "\x1b[31;1m";

/// Compound: bold + cyan — dark-theme title.
pub(crate) const ANSI_TITLE_DARK: &str = "\x1b[1;36m";
/// Compound: bold + underline + white — dark-theme header.
pub(crate) const ANSI_HEADER_DARK: &str = "\x1b[1;4;37m";
/// Compound: bold + blue — light-theme title.
pub(crate) const ANSI_TITLE_LIGHT: &str = "\x1b[1;34m";
/// Compound: bold + underline + black — light-theme header.
pub(crate) const ANSI_HEADER_LIGHT: &str = "\x1b[1;4;30m";

// ═══════════════════════════════════════════════════════════════════
// Threshold / byte-size constants
// ═══════════════════════════════════════════════════════════════════

/// 1 GB expressed in bytes — used for memory thresholding and byte formatting.
pub const ONE_GB: u64 = 1_073_741_824;

/// 512 MB expressed in bytes — used for memory thresholding.
pub const HALF_GB: u64 = 536_870_912;

/// CPU percentage above which the value is coloured red (>80%).
pub const CPU_RED_THRESHOLD: f64 = 80.0;

/// CPU percentage above which the value is coloured yellow (>50%).
pub const CPU_YELLOW_THRESHOLD: f64 = 50.0;

/// Alert evaluation loop interval in seconds.
pub(crate) const ALERT_EVAL_INTERVAL: u64 = 5;

/// Default metrics collection interval in seconds (used with --collect).
pub(crate) const DEFAULT_METRICS_INTERVAL: u64 = 30;

/// Default snapshot collection interval in seconds (used with --collect).
pub(crate) const DEFAULT_SNAPSHOT_INTERVAL: u64 = 300;
