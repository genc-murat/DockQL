use serde_json::{Number, Value as JsonValue};
use std::collections::{BTreeMap, HashMap, HashSet};
use thiserror::Error;

/// Color theme for the rendered table output.
///
/// - `Dark` (default): light text on dark background, DarkGray alternating rows.
/// - `Light`: dark text on light background, no row background tint.
#[derive(Clone, Copy, Debug, Default, PartialEq, clap::ValueEnum)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, Table};
use ratatui::widgets::Row as RatatuiRow;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::{
    analyze::{self, AnalyzeError},
    ast::{
        AggregateExpr, CollectionTarget, DurationUnit, Expression, LogsQuery, ObserveQuery,
        PipelineNode, Query, SortDirection,
    },
    docker::{Container, DockerClient, DockerError, Image, MetricSample, Network, Volume},
    eval::{self, EvalError},
    metrics::{MetricsCollector, MetricsError, NoopMetricsCollector},
    storage::{self, TelemetryError, TelemetryStore},
};

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct ExecutionResult {
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize)]
pub struct Row {
    pub fields: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("{0}")]
    Docker(DockerError),
    #[error("{0}")]
    Metrics(MetricsError),
    #[error("{0}")]
    Telemetry(TelemetryError),
    #[error("{0}")]
    Analyze(AnalyzeError),
    #[error("unsupported query for batch executor: {0}")]
    UnsupportedQuery(&'static str),
    #[error("unsupported pipeline node for batch executor: {0}")]
    UnsupportedPipeline(&'static str),
    #[error("{0}")]
    Eval(#[from] EvalError),
    #[error("no store snapshot available for {0}")]
    SnapshotNotFound(&'static str),
}

impl From<DockerError> for ExecutorError {
    fn from(error: DockerError) -> Self {
        Self::Docker(error)
    }
}

impl From<MetricsError> for ExecutorError {
    fn from(error: MetricsError) -> Self {
        Self::Metrics(error)
    }
}

impl From<TelemetryError> for ExecutorError {
    fn from(error: TelemetryError) -> Self {
        Self::Telemetry(error)
    }
}

impl From<AnalyzeError> for ExecutorError {
    fn from(error: AnalyzeError) -> Self {
        Self::Analyze(error)
    }
}

pub fn execute<C>(query: &Query, docker: &C) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    execute_with_metrics(query, docker, &NoopMetricsCollector)
}

pub fn execute_with_metrics<C, M>(
    query: &Query,
    docker: &C,
    metrics: &M,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    crate::semantic::validate_semantics(query)?;
    match query {
        Query::Observe(query) => execute_observe(query, docker, metrics),
        Query::Events(_) => Err(ExecutorError::UnsupportedQuery("events")),
        Query::Inspect(_) => Err(ExecutorError::UnsupportedQuery("inspect")),
        Query::Compose(query) => execute_compose(query, docker, metrics),
        Query::Logs(query) => execute_logs(query, docker),
        Query::Ping => execute_ping(docker),
        Query::Analyze(query) => {
            analyze::execute_analyze(query, docker, metrics).map_err(ExecutorError::Analyze)
        }
        Query::Alert(_) => Err(ExecutorError::UnsupportedQuery("alert")),
        Query::Fields(target) => execute_fields(*target),
    }
}

pub fn execute_with_store<S>(query: &Query, store: &S) -> Result<ExecutionResult, ExecutorError>
where
    S: TelemetryStore + ?Sized,
{
    crate::semantic::validate_semantics(query)?;
    match query {
        Query::Inspect(query) if query.at.is_some() => {
            storage::inspect_at(query, store).map_err(Into::into)
        }
        Query::Events(query) if query.time.is_some() => {
            storage::historical_events(query, store).map_err(Into::into)
        }
        Query::Observe(query) if query.time.is_some() => historical_observe(query, store),
        Query::Analyze(query) => {
            analyze::execute_analyze_with_store(query, store).map_err(ExecutorError::Analyze)
        }
        Query::Inspect(_) => Err(ExecutorError::UnsupportedQuery("inspect")),
        Query::Events(_) => Err(ExecutorError::UnsupportedQuery("events")),
        Query::Observe(_) => Err(ExecutorError::UnsupportedQuery("observe historical")),
        Query::Compose(query) => historical_compose(query, store),
        Query::Logs(_) => Err(ExecutorError::UnsupportedQuery("logs")),
        Query::Ping => Err(ExecutorError::UnsupportedQuery("ping")),
        Query::Alert(_) => Err(ExecutorError::UnsupportedQuery("alert")),
        Query::Fields(target) => execute_fields(*target),
    }
}

fn historical_observe<S>(query: &ObserveQuery, store: &S) -> Result<ExecutionResult, ExecutorError>
where
    S: TelemetryStore + ?Sized,
{
    use crate::ast::TimeSelector;

    let timestamp = match &query.time {
        Some(TimeSelector::Last(dur)) => {
            let secs = match dur.unit {
                DurationUnit::Seconds => dur.value as i64,
                DurationUnit::Minutes => dur.value as i64 * 60,
                DurationUnit::Hours => dur.value as i64 * 3600,
                DurationUnit::Days => dur.value as i64 * 86400,
            };
            let now = chrono::Utc::now();
            (now - chrono::Duration::seconds(secs)).to_rfc3339()
        }
        Some(TimeSelector::Range { from, to: _ }) => from.clone(),
        None => return Err(ExecutorError::UnsupportedQuery("observe historical")),
    };

    let snapshot = store
        .snapshot_at_or_before(&timestamp)
        .map_err(ExecutorError::Telemetry)?
        .ok_or(ExecutorError::SnapshotNotFound("historical_observe"))?;

    let mut rows: Vec<Row> = match query.target {
        CollectionTarget::Containers => snapshot
            .containers
            .into_iter()
            .map(|c| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "snapshot_at".into(),
                    JsonValue::String(snapshot.timestamp.clone()),
                );
                fields.insert("id".into(), json_string(c.id));
                fields.insert("name".into(), json_string(c.name));
                fields.insert("image".into(), json_string(c.image));
                fields.insert("status".into(), json_string(c.status));
                fields.insert("state".into(), json_string(c.state));
                fields.insert(
                    "restart_count".into(),
                    c.restart_count.map(json_u64).unwrap_or(JsonValue::Null),
                );
                Row { fields }
            })
            .collect(),
        CollectionTarget::Images => snapshot
            .images
            .into_iter()
            .map(|img| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "snapshot_at".into(),
                    JsonValue::String(snapshot.timestamp.clone()),
                );
                fields.insert("id".into(), json_string(img.id));
                fields.insert("repository".into(), json_string(img.repository));
                fields.insert("tag".into(), json_string(img.tag));
                fields.insert("size".into(), json_string(img.size));
                Row { fields }
            })
            .collect(),
        CollectionTarget::Networks => snapshot
            .networks
            .into_iter()
            .map(|n| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "snapshot_at".into(),
                    JsonValue::String(snapshot.timestamp.clone()),
                );
                fields.insert("id".into(), json_string(n.id));
                fields.insert("name".into(), json_string(n.name));
                fields.insert("driver".into(), json_string(n.driver));
                Row { fields }
            })
            .collect(),
        CollectionTarget::Volumes => snapshot
            .volumes
            .into_iter()
            .map(|v| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "snapshot_at".into(),
                    JsonValue::String(snapshot.timestamp.clone()),
                );
                fields.insert("name".into(), json_string(v.name));
                fields.insert("driver".into(), json_string(v.driver));
                Row { fields }
            })
            .collect(),
    };

    if let Some(filter) = &query.filter {
        rows = filter_rows(rows, filter)?;
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn historical_compose<S>(
    query: &crate::ast::ComposeQuery,
    store: &S,
) -> Result<ExecutionResult, ExecutorError>
where
    S: TelemetryStore + ?Sized,
{
    let latest = store
        .all_snapshots()
        .map_err(ExecutorError::Telemetry)?
        .into_iter()
        .last()
        .ok_or(ExecutorError::SnapshotNotFound("historical_compose"))?;

    let compose_project_label = query.project.clone();

    let mut rows: Vec<Row> = match query.target {
        crate::ast::ComposeTarget::Containers
        | crate::ast::ComposeTarget::Services
        | crate::ast::ComposeTarget::Health
        | crate::ast::ComposeTarget::Ps
        | crate::ast::ComposeTarget::Stats => latest
            .containers
            .into_iter()
            .filter(|c| {
                has_compose_label(
                    &c.labels,
                    "com.docker.compose.project",
                    &compose_project_label,
                )
            })
            .map(|c| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "snapshot_at".into(),
                    JsonValue::String(latest.timestamp.clone()),
                );
                fields.insert("id".into(), json_string(c.id));
                fields.insert("name".into(), json_string(c.name));
                fields.insert("image".into(), json_string(c.image));
                fields.insert("status".into(), json_string(c.status));
                fields.insert("state".into(), json_string(c.state));
                fields.insert(
                    "restart_count".into(),
                    c.restart_count.map(json_u64).unwrap_or(JsonValue::Null),
                );
                if matches!(
                    query.target,
                    crate::ast::ComposeTarget::Services
                        | crate::ast::ComposeTarget::Health
                        | crate::ast::ComposeTarget::Ps
                        | crate::ast::ComposeTarget::Stats
                ) {
                    let service = extract_label_value(&c.labels, "com.docker.compose.service")
                        .map(JsonValue::String)
                        .unwrap_or(JsonValue::Null);
                    fields.insert("service".to_owned(), service);
                }
                Row { fields }
            })
            .collect(),
        crate::ast::ComposeTarget::Networks => latest
            .networks
            .into_iter()
            .filter(|n| {
                has_compose_label(
                    &n.labels,
                    "com.docker.compose.project",
                    &compose_project_label,
                )
            })
            .map(|n| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "snapshot_at".into(),
                    JsonValue::String(latest.timestamp.clone()),
                );
                network_row_fields(n, &mut fields);
                Row { fields }
            })
            .collect(),
        crate::ast::ComposeTarget::Volumes => latest
            .volumes
            .into_iter()
            .filter(|v| {
                has_compose_label(
                    &v.labels,
                    "com.docker.compose.project",
                    &compose_project_label,
                )
            })
            .map(|v| {
                let mut fields = BTreeMap::new();
                fields.insert(
                    "snapshot_at".into(),
                    JsonValue::String(latest.timestamp.clone()),
                );
                volume_row_fields(v, &mut fields);
                Row { fields }
            })
            .collect(),
        _ => {
            return Err(ExecutorError::UnsupportedQuery(
                "compose target not supported for historical queries",
            ));
        }
    };

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

/// Return only columns that have at least one non-null, non-empty value across all rows.
/// This hides columns that would appear blank and waste horizontal space.
fn filter_display_columns(result: &ExecutionResult) -> Vec<String> {
    let columns: Vec<String> = result.rows[0].fields.keys().cloned().collect();
    columns
        .into_iter()
        .filter(|col| {
            result.rows.iter().any(|row| match row.fields.get(col) {
                Some(JsonValue::Null) | None => false,
                Some(JsonValue::String(s)) => !s.is_empty(),
                Some(JsonValue::Array(a)) => !a.is_empty(),
                Some(JsonValue::Object(o)) => !o.is_empty(),
                _ => true, // numbers, booleans are always kept
            })
        })
        .collect()
}

/// Truncate a cell value to at most `max_width` characters, appending `…` when cut.
fn truncate_cell(value: &str, max_width: usize) -> String {
    if value.len() > max_width {
        let cut = max_width.saturating_sub(1);
        if cut == 0 {
            return "…".to_owned();
        }
        format!("{}…", &value[..cut])
    } else {
        value.to_owned()
    }
}

/// Infer a human-readable entity name from the column set.
fn infer_entity_type(columns: &[String]) -> &'static str {
    if columns.iter().any(|c| c == "repository") {
        "Images"
    } else if columns.iter().any(|c| c == "ports") {
        "Containers"
    } else if columns.iter().any(|c| c == "mountpoint") {
        "Volumes"
    } else if columns.iter().any(|c| c == "driver") {
        "Networks"
    } else {
        "Results"
    }
}

/// Build a visual size bar string for a cell value like "93.6MB" or "28.7GB".
/// Uses log10 scaling so that values across orders of magnitude (MB vs GB vs TB)
/// produce proportionally meaningful bars.  If all values cluster near the minimum
/// a `▏` tick is shown to indicate non-zero size.
fn size_bar(value_str: &str, min_bytes: f64, max_bytes: f64, max_width: usize) -> String {
    if max_bytes <= 0.0 || value_str.is_empty() {
        return String::new();
    }
    if let Some(bytes) = crate::eval::json_as_f64(&serde_json::Value::String(value_str.to_owned()))
    {
        if bytes <= 0.0 {
            return String::new();
        }
        let log_max = max_bytes.max(1.0).log10();
        let log_min = min_bytes.max(1.0).log10();
        let log_val = bytes.log10();
        // Normalise within [log_min, log_max] so the smallest value gets ~0 and
        // the largest gets max_width.  Clamp to handle edge cases gracefully.
        let range = (log_max - log_min).max(0.001);
        let fraction = ((log_val - log_min) / range).clamp(0.0, 1.0);
        let filled = (fraction * max_width as f64).round() as usize;
        let filled = filled.min(max_width);
        if filled == 0 && fraction > 0.0 {
            return "▏".to_owned();
        }
        "█".repeat(filled)
    } else {
        String::new()
    }
}

/// Render a table using ratatui's Table widget with box-drawing borders,
/// colored headers, and styled cells. Uses an in-memory TestBackend.
pub fn render_table_ratatui(result: &ExecutionResult) -> String {
    render_table_ratatui_with_theme(result, Theme::Dark)
}

/// Render a table with the given theme.  Selects ratatui if the terminal is
/// wide enough, otherwise falls back to the colour-coded plain-text render.
pub fn render_table_with_theme(result: &ExecutionResult, theme: Theme) -> String {
    render_table_ratatui_with_theme(result, theme)
}

fn render_table_ratatui_with_theme(result: &ExecutionResult, theme: Theme) -> String {
    if result.rows.is_empty() {
        return "No rows".to_owned();
    }

    // Filter out all-null/all-empty columns
    let columns = filter_display_columns(result);
    let col_count = columns.len() as u16;
    let entity = infer_entity_type(&columns);
    const MAX_COL_WIDTH: usize = 30;

    // Calculate column widths based on header and data
    let mut col_widths: Vec<usize> = columns.iter().map(|c| c.len().min(MAX_COL_WIDTH)).collect();
    for row in &result.rows {
        for (i, col) in columns.iter().enumerate() {
            let val = row
                .fields
                .get(col)
                .map(eval::render_json_cell)
                .unwrap_or_default();
            let truncated = truncate_cell(&val, MAX_COL_WIDTH);
            col_widths[i] = col_widths[i].max(truncated.len());
        }
    }
    // Add padding: min width of 6, cap at MAX_COL_WIDTH
    for w in &mut col_widths {
        *w = (*w + 2).clamp(6, MAX_COL_WIDTH + 2);
    }

    // Pre-compute size bar data for the "size" column if present (log10 scaling)
    let size_col_idx = columns.iter().position(|c| c == "size");
    let (size_min_bytes, size_max_bytes, size_bar_width) = if let Some(si) = size_col_idx {
        let mut min_b = f64::MAX;
        let mut max_b = 0.0_f64;
        for row in &result.rows {
            if let Some(val) = row.fields.get("size") {
                let s = eval::render_json_cell(val);
                if let Some(b) = eval::json_as_f64(&serde_json::Value::String(s)) {
                    if b > max_b { max_b = b; }
                    if b < min_b { min_b = b; }
                }
            }
        }
        let bw = 8;
        col_widths[si] += bw + 1;
        (if min_b == f64::MAX { 0.0 } else { min_b }, max_b, bw)
    } else {
        (0.0, 0.0, 0)
    };

    // Total width: sum of column widths + left/right border (2) + column spacings
    let col_spacing: u16 = 1;
    let spacing_total: u16 = if col_count > 1 {
        (col_count - 1) * col_spacing
    } else {
        0
    };
    let total_width: u16 = col_widths.iter().sum::<usize>() as u16
        + 2  // left + right border
        + spacing_total;

    // Use terminal width if available, otherwise fall back to calculated width
    let term_width = crossterm::terminal::size()
        .map(|(w, _)| w)
        .unwrap_or(120);

    // If the table is too wide for the terminal, fall back to the simpler
    // colored render which handles overflow better (no fixed borders).
    if total_width > term_width.saturating_sub(2) {
        return render_table_colored_with_theme(result, theme);
    }

    let width = total_width.max(20);
    let row_count = result.rows.len() as u16;
    let height = row_count + 5;
    let height = height.clamp(5, 200);

    // Theme-specific styles
    // Light theme: no alternating row background (white-on-white is invisible),
    // use blue/dark headings readable on light terminals.
    let (alt_row_style, header_fg, title_fg, border_fg) = match theme {
        Theme::Dark => (
            Style::default().bg(Color::DarkGray),
            Color::Cyan,
            Color::Cyan,
            Color::DarkGray,
        ),
        Theme::Light => (
            Style::default(), // no alternating bg for light theme
            Color::Blue,
            Color::Blue,
            Color::Blue,
        ),
    };

    let backend = TestBackend::new(width, height);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => return render_table_colored_with_theme(result, theme),
    };

    let _ = terminal.draw(|f| {
        let area = Rect::new(0, 0, width, height);

        // ── Build data rows with alternating background ──
        let ratatui_rows: Vec<RatatuiRow> = result
            .rows
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                let is_even = idx % 2 == 0;
                let row_base = match (theme, is_even) {
                    (Theme::Light, _) => Style::default(),
                    (Theme::Dark, true) => Style::default(),
                    (Theme::Dark, false) => alt_row_style,
                };

                let cells: Vec<Cell> = columns
                    .iter()
                    .map(|col| {
                        let val = row
                            .fields
                            .get(col)
                            .map(eval::render_json_cell)
                            .unwrap_or_default();
                        let val = truncate_cell(&val, MAX_COL_WIDTH);

                        // Compute base style from value semantics
                        let base_style = match col.as_str() {
                            "state" | "status" => {
                                Style::default().fg(ansi_state_color_ratatui(&val))
                            }
                            "cpu" => Style::default().fg(threshold_color_ratatui_val(&val)),
                            "memory" => Style::default().fg(memory_color_ratatui(&val)),
                            _ => Style::default(),
                        };
                        let cell_style = base_style.patch(row_base);

                        // For the "size" column, augment with a visual bar
                        if col.as_str() == "size" && size_max_bytes > 0.0 && !val.is_empty() {
                            let bar = size_bar(&val, size_min_bytes, size_max_bytes, size_bar_width);
                            if !bar.is_empty() {
                                let combined = format!("{val} {bar}");
                                Cell::from(Span::styled(combined, cell_style))
                            } else {
                                Cell::from(Span::styled(val, cell_style))
                            }
                        } else {
                            Cell::from(Span::styled(val, cell_style))
                        }
                    })
                    .collect();
                RatatuiRow::new(cells).height(1)
            })
            .collect();

        // ── Build header cells ──
        let header_cells: Vec<Cell> = columns
            .iter()
            .map(|col| {
                let header_style = Style::default()
                    .fg(header_fg)
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED);
                Cell::from(Span::styled(col.to_uppercase(), header_style))
            })
            .collect();

        let constraints: Vec<Constraint> = col_widths
            .iter()
            .map(|w| Constraint::Length(*w as u16))
            .collect();

        let title_text = format!(" {} ", entity);

        let table = Table::new(ratatui_rows, constraints)
            .header(RatatuiRow::new(header_cells).height(1))
            .block(
                Block::default()
                    .title(
                        ratatui::text::Line::from(
                            ratatui::text::Span::styled(
                                title_text,
                                Style::default()
                                    .fg(title_fg)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        )
                    )
                    .title_alignment(ratatui::layout::Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_fg)),
            )
            .column_spacing(1);

        f.render_widget(table, area);
    });

    let buffer = terminal.backend().buffer();
    buffer_to_ansi_string(buffer)
}

/// Convert a ratatui Buffer to an ANSI-colored string by iterating cells.
fn buffer_to_ansi_string(buffer: &ratatui::buffer::Buffer) -> String {
    use std::fmt::Write;
    let area = buffer.area();
    let mut output = String::new();
    let mut prev_style: Option<Style> = None;
    let mut has_content = false;

    for y in 0..area.height {
        let mut line = String::new();
        let mut line_has_content = false;

        for x in 0..area.width {
            let idx = (y * area.width + x) as usize;
            let cell = &buffer.content()[idx];
            let symbol = cell.symbol();

            if !symbol.is_empty() && symbol != " " {
                line_has_content = true;
            }

            // Apply style if changed
            let style = cell.style();
            if prev_style != Some(style) {
                if style != Style::default() {
                    let _ = write!(line, "{}", style_to_ansi(style));
                } else {
                    line.push_str("\x1b[0m");
                }
                prev_style = Some(style);
            }

            line.push_str(symbol);
        }

        if line_has_content {
            // Trim trailing whitespace but preserve ANSI codes at the end
            let trimmed = line.trim_end_matches(' ');
            output.push_str(trimmed);
            output.push('\n');
            has_content = true;
        }
    }

    if has_content {
        output.push_str("\x1b[0m");
    }
    output
}

/// Convert a full ratatui::style::Style to ANSI escape codes.
fn style_to_ansi(style: Style) -> String {
    let mut codes = String::new();

    // Reset first if there's any style change
    if style != Style::default() {
        codes.push_str("\x1b[0m");
    }

    // Bold
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push_str("\x1b[1m");
    }
    // Dim
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push_str("\x1b[2m");
    }
    // Italic
    if style.add_modifier.contains(Modifier::ITALIC) {
        codes.push_str("\x1b[3m");
    }
    // Underline
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        codes.push_str("\x1b[4m");
    }

    // Foreground (extended 256-color support)
    if let Some(fg) = style.fg {
        match fg {
            Color::Black => codes.push_str("\x1b[30m"),
            Color::Red => codes.push_str("\x1b[31m"),
            Color::Green => codes.push_str("\x1b[32m"),
            Color::Yellow => codes.push_str("\x1b[33m"),
            Color::Blue => codes.push_str("\x1b[34m"),
            Color::Magenta => codes.push_str("\x1b[35m"),
            Color::Cyan => codes.push_str("\x1b[36m"),
            Color::White => codes.push_str("\x1b[37m"),
            Color::DarkGray => codes.push_str("\x1b[90m"),
            Color::LightRed => codes.push_str("\x1b[91m"),
            Color::LightGreen => codes.push_str("\x1b[92m"),
            Color::LightYellow => codes.push_str("\x1b[93m"),
            Color::LightBlue => codes.push_str("\x1b[94m"),
            Color::LightMagenta => codes.push_str("\x1b[95m"),
            Color::LightCyan => codes.push_str("\x1b[96m"),
            _ => {}
        }
    }

    // Background (extended 256-color support)
    if let Some(bg) = style.bg {
        match bg {
            Color::Black => codes.push_str("\x1b[40m"),
            Color::Red => codes.push_str("\x1b[41m"),
            Color::Green => codes.push_str("\x1b[42m"),
            Color::Yellow => codes.push_str("\x1b[43m"),
            Color::Blue => codes.push_str("\x1b[44m"),
            Color::Magenta => codes.push_str("\x1b[45m"),
            Color::Cyan => codes.push_str("\x1b[46m"),
            Color::White => codes.push_str("\x1b[47m"),
            Color::DarkGray => codes.push_str("\x1b[100m"),
            Color::LightRed => codes.push_str("\x1b[101m"),
            Color::LightGreen => codes.push_str("\x1b[102m"),
            Color::LightYellow => codes.push_str("\x1b[103m"),
            Color::LightBlue => codes.push_str("\x1b[104m"),
            Color::LightMagenta => codes.push_str("\x1b[105m"),
            Color::LightCyan => codes.push_str("\x1b[106m"),
            _ => {}
        }
    }

    codes
}

fn ansi_state_color_ratatui(value: &str) -> Color {
    match value {
        "running" => Color::Green,
        "exited" | "dead" => Color::Red,
        "restarting" | "paused" => Color::Yellow,
        "created" => Color::Cyan,
        "critical" => Color::LightRed,
        "warning" => Color::Yellow,
        "ok" | "healthy" => Color::Green,
        _ => Color::White,
    }
}

fn threshold_color_ratatui_val(value_str: &str) -> Color {
    let cleaned = value_str.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.');
    if let Ok(val) = cleaned.parse::<f64>() {
        if val > 80.0 {
            Color::Red
        } else if val > 50.0 {
            Color::Yellow
        } else {
            Color::Green
        }
    } else {
        Color::White
    }
}

fn memory_color_ratatui(value_str: &str) -> Color {
    if let Ok(bytes) = value_str.parse::<f64>() {
        if bytes > 1_073_741_824.0 {
            Color::Red
        } else if bytes > 536_870_912.0 {
            Color::Yellow
        } else {
            Color::Green
        }
    } else {
        Color::White
    }
}

pub fn render_table(result: &ExecutionResult) -> String {
    if result.rows.is_empty() {
        return "No rows".to_owned();
    }

    let columns = result.rows[0].fields.keys().cloned().collect::<Vec<_>>();
    let mut widths = columns
        .iter()
        .map(|column| column.len())
        .collect::<Vec<_>>();
    let rendered_rows = result
        .rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let value = row
                        .fields
                        .get(column)
                        .map(eval::render_json_cell)
                        .unwrap_or_default();
                    widths[index] = widths[index].max(value.len());
                    value
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut lines = Vec::new();
    lines.push(render_table_line(&columns, &widths));
    lines.push(
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  "),
    );
    lines.extend(
        rendered_rows
            .iter()
            .map(|row| render_table_line(row, &widths)),
    );
    lines.join("\n")
}

fn has_compose_label(labels: &[String], label_key: &str, label_value: &str) -> bool {
    labels.iter().any(|label| {
        let parts: Vec<&str> = label.splitn(2, '=').collect();
        parts.len() == 2 && parts[0] == label_key && parts[1] == label_value
    })
}

fn extract_label_value(labels: &[String], label_key: &str) -> Option<String> {
    labels.iter().find_map(|label| {
        let parts: Vec<&str> = label.splitn(2, '=').collect();
        if parts.len() == 2 && parts[0] == label_key {
            Some(parts[1].to_owned())
        } else {
            None
        }
    })
}

fn execute_compose<C, M>(
    query: &crate::ast::ComposeQuery,
    docker: &C,
    metrics: &M,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    if matches!(query.target, crate::ast::ComposeTarget::Projects) {
        return execute_compose_projects(query, docker);
    }

    if matches!(query.target, crate::ast::ComposeTarget::Config) {
        return execute_compose_config(query, docker);
    }

    let compose_project_label = query.project.clone();

    let mut rows: Vec<Row> = match query.target {
        crate::ast::ComposeTarget::Containers
        | crate::ast::ComposeTarget::Services
        | crate::ast::ComposeTarget::Health
        | crate::ast::ComposeTarget::Ps
        | crate::ast::ComposeTarget::Stats => {
            let samples = latest_metrics_by_container(metrics.collect()?);
            docker
                .list_containers()?
                .into_iter()
                .filter(|c| {
                    has_compose_label(
                        &c.labels,
                        "com.docker.compose.project",
                        &compose_project_label,
                    )
                })
                .map(|container| {
                    let service = match query.target {
                        crate::ast::ComposeTarget::Services
                        | crate::ast::ComposeTarget::Health
                        | crate::ast::ComposeTarget::Ps
                        | crate::ast::ComposeTarget::Stats => {
                            extract_label_value(&container.labels, "com.docker.compose.service")
                                .map(JsonValue::String)
                                .unwrap_or(JsonValue::Null)
                        }
                        _ => JsonValue::Null,
                    };
                    let mut row = container_row(container, &samples);
                    if matches!(
                        query.target,
                        crate::ast::ComposeTarget::Services
                            | crate::ast::ComposeTarget::Health
                            | crate::ast::ComposeTarget::Ps
                            | crate::ast::ComposeTarget::Stats
                    ) {
                        row.fields.insert("service".to_owned(), service);
                    }
                    row
                })
                .collect()
        }
        crate::ast::ComposeTarget::Images => {
            let containers = docker.list_containers()?;
            let all_images = docker.list_images()?;
            let project_image_ids: std::collections::HashSet<String> = containers
                .iter()
                .filter(|c| {
                    has_compose_label(
                        &c.labels,
                        "com.docker.compose.project",
                        &compose_project_label,
                    )
                })
                .map(|c| c.image.clone())
                .collect();

            all_images
                .into_iter()
                .filter(|img| {
                    project_image_ids.contains(&img.id)
                        || project_image_ids.iter().any(|pid| {
                            img.repository == *pid
                                || format!("{}:{}", img.repository, img.tag) == *pid
                        })
                        || project_image_ids.contains(&format!("{}:{}", img.repository, img.tag))
                })
                .map(|img| {
                    let mut row = image_row(img);
                    row.fields.insert(
                        "service".to_owned(),
                        JsonValue::String("unknown".to_owned()),
                    );
                    row
                })
                .collect()
        }
        crate::ast::ComposeTarget::Events => {
            return Err(ExecutorError::UnsupportedQuery(
                "compose events requires streaming; use events pipeline",
            ));
        }
        crate::ast::ComposeTarget::Port => {
            return execute_compose_port(query, docker);
        }
        crate::ast::ComposeTarget::Logs => {
            return execute_compose_logs(query, docker);
        }
        crate::ast::ComposeTarget::Networks => docker
            .list_networks()?
            .into_iter()
            .filter(|n| {
                has_compose_label(
                    &n.labels,
                    "com.docker.compose.project",
                    &compose_project_label,
                )
            })
            .map(network_row)
            .collect(),
        crate::ast::ComposeTarget::Volumes => docker
            .list_volumes()?
            .into_iter()
            .filter(|v| {
                has_compose_label(
                    &v.labels,
                    "com.docker.compose.project",
                    &compose_project_label,
                )
            })
            .map(volume_row)
            .collect(),
        crate::ast::ComposeTarget::Projects | crate::ast::ComposeTarget::Config => {
            unreachable!("handled above")
        }
    };

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn execute_compose_projects<C>(
    query: &crate::ast::ComposeQuery,
    docker: &C,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    let containers = docker.list_containers()?;
    let networks = docker.list_networks()?;
    let volumes = docker.list_volumes()?;

    let mut projects: BTreeMap<String, BTreeMap<&str, u64>> = BTreeMap::new();

    for container in &containers {
        if let Some(project) =
            extract_label_value(&container.labels, "com.docker.compose.project")
        {
            let entry = projects.entry(project).or_default();
            entry.insert("containers", entry.get("containers").unwrap_or(&0) + 1);
            if container.state == "running" {
                entry.insert("running", entry.get("running").unwrap_or(&0) + 1);
            } else {
                entry.insert("stopped", entry.get("stopped").unwrap_or(&0) + 1);
            }
        }
    }

    for network in &networks {
        if let Some(project) =
            extract_label_value(&network.labels, "com.docker.compose.project")
        {
            let entry = projects.entry(project).or_default();
            entry.insert("networks", entry.get("networks").unwrap_or(&0) + 1);
        }
    }

    for volume in &volumes {
        if let Some(project) =
            extract_label_value(&volume.labels, "com.docker.compose.project")
        {
            let entry = projects.entry(project).or_default();
            entry.insert("volumes", entry.get("volumes").unwrap_or(&0) + 1);
        }
    }

    let mut rows: Vec<Row> = projects
        .into_iter()
        .map(|(project, counts)| {
            let containers = counts.get("containers").copied().unwrap_or(0);
            let running = counts.get("running").copied().unwrap_or(0);
            let stopped = counts.get("stopped").copied().unwrap_or(0);
            let networks = counts.get("networks").copied().unwrap_or(0);
            let volumes = counts.get("volumes").copied().unwrap_or(0);
            Row::from_fields([
                ("project", json_string(project)),
                ("containers", json_u64(containers)),
                ("running", json_u64(running)),
                ("stopped", json_u64(stopped)),
                ("networks", json_u64(networks)),
                ("volumes", json_u64(volumes)),
                (
                    "status",
                    if running == containers && containers > 0 {
                        json_string("running".to_owned())
                    } else if running == 0 {
                        json_string("stopped".to_owned())
                    } else {
                        json_string("partial".to_owned())
                    },
                ),
            ])
        })
        .collect();

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn execute_compose_port<C>(
    query: &crate::ast::ComposeQuery,
    docker: &C,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    let service_name = query.service.as_deref().unwrap_or("");
    let port_number = query.port_number.unwrap_or(0);

    let containers = docker.list_containers()?;
    let matching: Vec<_> = containers
        .into_iter()
        .filter(|c| {
            has_compose_label(&c.labels, "com.docker.compose.project", &query.project)
                && extract_label_value(&c.labels, "com.docker.compose.service")
                    .as_deref()
                    .map(|s| s == service_name)
                    .unwrap_or(false)
        })
        .collect();

    let mut rows = Vec::new();
    for container in matching {
        let id_short = &container.id[..12.min(container.id.len())];
        if let Ok(full) = docker.inspect_container(id_short)
            && let Some(_ports_str) = full.ports.first()
        {
            let _ = (service_name, port_number);
            rows.push(Row::from_fields([
                ("service", json_string(service_name.to_owned())),
                ("container", json_string(full.name.clone())),
                ("ports", json_string_array(full.ports.clone())),
            ]));
        }
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn execute_compose_logs<C>(
    query: &crate::ast::ComposeQuery,
    docker: &C,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    let service_name = query.service.as_deref().unwrap_or("");
    let tail = query.tail.unwrap_or(100) as usize;

    let containers = docker.list_containers()?;
    let matching: Vec<_> = containers
        .into_iter()
        .filter(|c| {
            has_compose_label(&c.labels, "com.docker.compose.project", &query.project)
                && extract_label_value(&c.labels, "com.docker.compose.service")
                    .as_deref()
                    .map(|s| s == service_name)
                    .unwrap_or(false)
        })
        .collect();

    let mut rows = Vec::new();
    for container in matching {
        let lines = docker.container_logs(&container.id, tail)?;
        for (i, line) in lines.into_iter().enumerate() {
            let mut fields = BTreeMap::new();
            fields.insert("line".to_owned(), JsonValue::Number(Number::from(i + 1)));
            fields.insert("message".to_owned(), JsonValue::String(line));
            fields.insert("service".to_owned(), JsonValue::String(service_name.to_owned()));
            fields.insert("container".to_owned(), JsonValue::String(container.name.clone()));
            rows.push(Row { fields });
        }
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn execute_compose_config<C>(
    query: &crate::ast::ComposeQuery,
    docker: &C,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    let config_target = query.config_target.unwrap_or(crate::ast::ConfigTarget::All);

    let containers = docker.list_containers()?;
    let networks = docker.list_networks()?;
    let volumes = docker.list_volumes()?;

    let project_containers: Vec<_> = containers
        .into_iter()
        .filter(|c| {
            has_compose_label(&c.labels, "com.docker.compose.project", &query.project)
        })
        .collect();

    let project_networks: Vec<_> = networks
        .into_iter()
        .filter(|n| {
            has_compose_label(&n.labels, "com.docker.compose.project", &query.project)
        })
        .collect();

    let project_volumes: Vec<_> = volumes
        .into_iter()
        .filter(|v| {
            has_compose_label(&v.labels, "com.docker.compose.project", &query.project)
        })
        .collect();

    let mut rows = Vec::new();

    if matches!(
        config_target,
        crate::ast::ConfigTarget::Services | crate::ast::ConfigTarget::All
    ) {
        let mut seen_services: std::collections::HashSet<String> = std::collections::HashSet::new();
        for container in &project_containers {
            if let Some(svc) =
                extract_label_value(&container.labels, "com.docker.compose.service")
                && seen_services.insert(svc.clone())
            {
                let labels = &container.labels;
                let depends_on: Vec<String> = labels
                    .iter()
                    .filter(|l| l.starts_with("com.docker.compose.depends_on"))
                    .cloned()
                    .collect();
                rows.push(Row::from_fields([
                    ("name", json_string(svc)),
                    ("image", json_string(container.image.clone())),
                    ("state", json_string(container.state.clone())),
                    ("status", json_string(container.status.clone())),
                    ("ports", json_string_array(container.ports.clone())),
                    (
                        "restart_count",
                        container
                            .restart_count
                            .map(json_u64)
                            .unwrap_or(JsonValue::Null),
                    ),
                    (
                        "health",
                        container
                            .health
                            .clone()
                            .map(JsonValue::String)
                            .unwrap_or(JsonValue::Null),
                    ),
                    ("depends_on", json_string_array(depends_on)),
                ]));
            }
        }
    }

    if matches!(
        config_target,
        crate::ast::ConfigTarget::Networks | crate::ast::ConfigTarget::All
    ) {
        for network in &project_networks {
            let net_name = extract_label_value(&network.labels, "com.docker.compose.network")
                .unwrap_or_else(|| network.name.clone());
            rows.push(Row::from_fields([
                ("name", json_string(net_name)),
                ("driver", json_string(network.driver.clone())),
                ("scope", json_string(network.scope.clone())),
                (
                    "containers",
                    json_u64(network.containers.len() as u64),
                ),
            ]));
        }
    }

    if matches!(
        config_target,
        crate::ast::ConfigTarget::Volumes | crate::ast::ConfigTarget::All
    ) {
        for volume in &project_volumes {
            let vol_name = extract_label_value(&volume.labels, "com.docker.compose.volume")
                .unwrap_or_else(|| volume.name.clone());
            rows.push(Row::from_fields([
                ("name", json_string(vol_name)),
                ("driver", json_string(volume.driver.clone())),
                (
                    "mountpoint",
                    json_option_string(volume.mountpoint.clone()),
                ),
                ("scope", json_option_string(volume.scope.clone())),
            ]));
        }
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn execute_logs<C>(query: &LogsQuery, docker: &C) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    let tail = query.tail.unwrap_or(100) as usize;
    let lines = docker.container_logs(&query.container, tail)?;

    let mut rows: Vec<Row> = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let mut fields = BTreeMap::new();
            fields.insert("line".to_owned(), JsonValue::Number(Number::from(i + 1)));
            fields.insert("message".to_owned(), JsonValue::String(line));
            fields.insert(
                "container".to_owned(),
                JsonValue::String(query.container.clone()),
            );
            Row { fields }
        })
        .collect();

    if let Some(filter) = &query.filter {
        rows = filter_rows(rows, filter)?;
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn execute_ping<C>(docker: &C) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
{
    let reachable = docker.ping()?;
    let mut fields = BTreeMap::new();
    fields.insert(
        "status".to_owned(),
        JsonValue::String(if reachable { "ok" } else { "error" }.to_owned()),
    );
    fields.insert(
        "message".to_owned(),
        JsonValue::String(if reachable {
            "Docker daemon is reachable".to_owned()
        } else {
            "Cannot connect to Docker daemon".to_owned()
        }),
    );
    Ok(ExecutionResult {
        rows: vec![Row { fields }],
    })
}

fn execute_fields(target: CollectionTarget) -> Result<ExecutionResult, ExecutorError> {
    let fields = field_descriptions(target);
    let rows = fields
        .into_iter()
        .map(|(field, kind)| {
            let mut map = BTreeMap::new();
            map.insert("field".to_owned(), JsonValue::String(field));
            map.insert("type".to_owned(), JsonValue::String(kind));
            Row { fields: map }
        })
        .collect();
    Ok(ExecutionResult { rows })
}

fn field_descriptions(target: CollectionTarget) -> Vec<(String, String)> {
    match target {
        CollectionTarget::Containers => vec![
            ("id".into(), "string".into()),
            ("name".into(), "string".into()),
            ("image".into(), "string".into()),
            ("status".into(), "string".into()),
            ("state".into(), "string".into()),
            ("ports".into(), "array".into()),
            ("labels".into(), "array".into()),
            ("compose_project".into(), "string".into()),
            ("created_at".into(), "string".into()),
            ("started_at".into(), "string".into()),
            ("finished_at".into(), "string".into()),
            ("restart_count".into(), "integer".into()),
            ("cpu".into(), "float".into()),
            ("memory".into(), "integer".into()),
            ("memory_limit".into(), "integer".into()),
            ("network_rx".into(), "integer".into()),
            ("network_tx".into(), "integer".into()),
            ("disk_read".into(), "integer".into()),
            ("disk_write".into(), "integer".into()),
        ],
        CollectionTarget::Images => vec![
            ("id".into(), "string".into()),
            ("repository".into(), "string".into()),
            ("name".into(), "string".into()),
            ("tag".into(), "string".into()),
            ("digest".into(), "string".into()),
            ("size".into(), "string".into()),
            ("created_at".into(), "string".into()),
            ("labels".into(), "array".into()),
        ],
        CollectionTarget::Networks => vec![
            ("id".into(), "string".into()),
            ("name".into(), "string".into()),
            ("driver".into(), "string".into()),
            ("scope".into(), "string".into()),
            ("containers".into(), "array".into()),
            ("labels".into(), "array".into()),
        ],
        CollectionTarget::Volumes => vec![
            ("name".into(), "string".into()),
            ("driver".into(), "string".into()),
            ("mountpoint".into(), "string".into()),
            ("scope".into(), "string".into()),
            ("labels".into(), "array".into()),
        ],
    }
}

fn target_alias(target: CollectionTarget) -> &'static str {
    match target {
        CollectionTarget::Containers => "c",
        CollectionTarget::Images => "i",
        CollectionTarget::Networks => "n",
        CollectionTarget::Volumes => "v",
    }
}

fn execute_observe<C, M>(
    query: &ObserveQuery,
    docker: &C,
    metrics: &M,
) -> Result<ExecutionResult, ExecutorError>
where
    C: DockerClient + ?Sized,
    M: MetricsCollector + ?Sized,
{
    let mut rows = match query.target {
        CollectionTarget::Containers => {
            let samples = latest_metrics_by_container(metrics.collect()?);
            docker
                .list_containers()?
                .into_iter()
                .map(|container| container_row(container, &samples))
                .collect::<Vec<_>>()
        }
        CollectionTarget::Images => docker
            .list_images()?
            .into_iter()
            .map(image_row)
            .collect::<Vec<_>>(),
        CollectionTarget::Networks => docker
            .list_networks()?
            .into_iter()
            .map(network_row)
            .collect::<Vec<_>>(),
        CollectionTarget::Volumes => docker
            .list_volumes()?
            .into_iter()
            .map(volume_row)
            .collect::<Vec<_>>(),
    };

    if let Some(join) = &query.join {
        let right_alias = target_alias(join.right);
        let right_rows: Vec<Row> = match join.right {
            CollectionTarget::Containers => {
                let samples = latest_metrics_by_container(metrics.collect()?);
                docker
                    .list_containers()?
                    .into_iter()
                    .map(|container| container_row(container, &samples))
                    .collect::<Vec<_>>()
            }
            CollectionTarget::Images => docker
                .list_images()?
                .into_iter()
                .map(image_row)
                .collect::<Vec<_>>(),
            CollectionTarget::Networks => docker
                .list_networks()?
                .into_iter()
                .map(network_row)
                .collect::<Vec<_>>(),
            CollectionTarget::Volumes => docker
                .list_volumes()?
                .into_iter()
                .map(volume_row)
                .collect::<Vec<_>>(),
        };

        let left_alias = target_alias(query.target);
        let mut joined = Vec::new();
        for left_row in rows {
            for right_row in &right_rows {
                let left_val =
                    eval::eval_expr(&left_row.fields, &join.left_key).unwrap_or(JsonValue::Null);
                let right_val =
                    eval::eval_expr(&right_row.fields, &join.right_key).unwrap_or(JsonValue::Null);
                if eval::compare_json_values(&left_val, &right_val) == std::cmp::Ordering::Equal {
                    let mut merged_fields = BTreeMap::new();
                    for (k, v) in &left_row.fields {
                        merged_fields.insert(format!("{left_alias}.{k}"), v.clone());
                    }
                    for (k, v) in &right_row.fields {
                        merged_fields.insert(format!("{right_alias}.{k}"), v.clone());
                    }
                    joined.push(Row {
                        fields: merged_fields,
                    });
                }
            }
        }
        rows = joined;
    }

    if let Some(filter) = &query.filter {
        rows = filter_rows(rows, filter)?;
    }

    for node in &query.pipeline {
        rows = apply_pipeline_node(rows, node)?;
    }

    Ok(ExecutionResult { rows })
}

fn apply_pipeline_node(mut rows: Vec<Row>, node: &PipelineNode) -> Result<Vec<Row>, ExecutorError> {
    match node {
        PipelineNode::Where(expression) => filter_rows(rows, expression),
        PipelineNode::Select(fields) => rows
            .into_iter()
            .map(|row| select_fields(row, fields))
            .collect(),
        PipelineNode::SortBy { fields } => {
            sort_rows(&mut rows, fields)?;
            Ok(rows)
        }
        PipelineNode::Limit(limit) => {
            rows.truncate(*limit as usize);
            Ok(rows)
        }
        PipelineNode::GroupBy { fields, aggregates } => Ok(group_rows(rows, fields, aggregates)),
        PipelineNode::Having(expr) => filter_rows(rows, expr),
        PipelineNode::Distinct => {
            let mut seen = HashSet::new();
            rows.retain(|row| {
                let key: Vec<String> = row.fields.values().map(eval::render_json_cell).collect();
                seen.insert(key)
            });
            Ok(rows)
        }
        PipelineNode::Offset(offset) => {
            let skip = *offset as usize;
            if skip < rows.len() {
                rows = rows.split_off(skip);
            } else {
                rows.clear();
            }
            Ok(rows)
        }
        PipelineNode::Alert(message) => {
            for row in &rows {
                let output = row
                    .fields
                    .iter()
                    .map(|(k, v)| format!("{k}={}", eval::render_json_cell(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                eprintln!("[ALERT] {message}: {output}");
            }
            Ok(rows)
        }
        PipelineNode::If {
            condition,
            then_branch,
            else_branch,
        } => {
            let empty = Vec::new();
            let mut result = Vec::new();
            for row in rows {
                let matched = eval::evaluate_expression(&row.fields, condition)?;
                let branch = if matched {
                    then_branch
                } else {
                    else_branch.as_ref().unwrap_or(&empty)
                };
                let mut current = vec![row];
                for node in branch {
                    current = apply_pipeline_node(current, node)?;
                }
                result.extend(current);
            }
            Ok(result)
        }
        PipelineNode::Set { field, value } => {
            for row in &mut rows {
                let json_value = eval::evaluate_set_value(&row.fields, value)?;
                row.fields.insert(field.clone(), json_value);
            }
            Ok(rows)
        }
        PipelineNode::Fill { field, default } => {
            for row in &mut rows {
                if !row.fields.contains_key(field)
                    || row.fields.get(field) == Some(&JsonValue::Null)
                    || matches!(row.fields.get(field), Some(JsonValue::String(s)) if s.is_empty())
                {
                    let value = eval::eval_expr(&row.fields, default)?;
                    row.fields.insert(field.clone(), value);
                }
            }
            Ok(rows)
        }
        PipelineNode::Let { name, value } => {
            let value = eval::eval_expr(&BTreeMap::new(), value)?;
            for row in &mut rows {
                row.fields.insert(name.clone(), value.clone());
            }
            Ok(rows)
        }
    }
}

fn filter_rows(rows: Vec<Row>, expression: &Expression) -> Result<Vec<Row>, ExecutorError> {
    rows.into_iter()
        .map(|row| eval::evaluate_expression(&row.fields, expression).map(|keep| (row, keep)))
        .filter_map(|result| match result {
            Ok((row, true)) => Some(Ok(row)),
            Ok((_, false)) => None,
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

fn select_fields(row: Row, fields: &[String]) -> Result<Row, ExecutorError> {
    let mut selected = BTreeMap::new();

    for field in fields {
        if let Some(value) = row.fields.get(field) {
            selected.insert(field.clone(), value.clone());
        } else if let Some(label_key) = field.strip_prefix("label.") {
            if let Some(JsonValue::Array(items)) = row.fields.get("labels") {
                for item in items {
                    if let JsonValue::String(entry) = item
                        && let Some(eq_pos) = entry.find('=')
                    {
                        let key = &entry[..eq_pos];
                        let val = &entry[eq_pos + 1..];
                        if key == label_key {
                            selected.insert(field.clone(), JsonValue::String(val.to_owned()));
                            break;
                        }
                    }
                }
            }
        } else {
            return Err(EvalError::UnsupportedField {
                field: field.clone(),
            }
            .into());
        }
    }

    Ok(Row { fields: selected })
}

fn sort_rows(rows: &mut [Row], fields: &[(String, SortDirection)]) -> Result<(), ExecutorError> {
    fn resolve_sort_value(row: &Row, field: &str) -> Option<JsonValue> {
        if let Some(v) = row.fields.get(field) {
            return Some(v.clone());
        }
        if let Some(label_key) = field.strip_prefix("label.")
            && let Some(JsonValue::Array(items)) = row.fields.get("labels")
        {
            for item in items {
                if let JsonValue::String(entry) = item
                    && let Some(eq_pos) = entry.find('=')
                {
                    let key = &entry[..eq_pos];
                    let val = &entry[eq_pos + 1..];
                    if key == label_key {
                        return Some(JsonValue::String(val.to_owned()));
                    }
                }
            }
        }
        None
    }

    // Validate all sort fields exist
    for (field, _) in fields {
        if rows
            .iter()
            .any(|row| resolve_sort_value(row, field).is_none())
        {
            return Err(EvalError::UnsupportedField {
                field: field.to_owned(),
            }
            .into());
        }
    }

    rows.sort_by(|left, right| {
        for (field, direction) in fields {
            let left_val = resolve_sort_value(left, field).unwrap_or(JsonValue::Null);
            let right_val = resolve_sort_value(right, field).unwrap_or(JsonValue::Null);
            let ordering = eval::compare_json_values(&left_val, &right_val);
            let ordering = match direction {
                SortDirection::Asc => ordering,
                SortDirection::Desc => ordering.reverse(),
            };
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        std::cmp::Ordering::Equal
    });
    Ok(())
}

fn container_row(container: Container, samples: &HashMap<String, MetricSample>) -> Row {
    let sample = samples
        .get(&container.id)
        .or_else(|| samples.get(&container.name));

    let compose_project = container.labels.iter().find_map(|label| {
        let parts: Vec<&str> = label.splitn(2, '=').collect();
        if parts.len() == 2 && parts[0] == "com.docker.compose.project" {
            Some(parts[1].to_owned())
        } else {
            None
        }
    });

    Row::from_fields([
        ("id", json_string(container.id)),
        ("name", json_string(container.name)),
        ("image", json_string(container.image)),
        ("status", json_string(container.status)),
        ("state", json_string(container.state)),
        ("ports", json_string_array(container.ports)),
        ("labels", json_string_array(container.labels)),
        (
            "compose_project",
            compose_project
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
        ("created_at", json_option_string(container.created_at)),
        ("started_at", json_option_string(container.started_at)),
        ("finished_at", json_option_string(container.finished_at)),
        (
            "restart_count",
            container
                .restart_count
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "cpu",
            sample
                .and_then(|sample| sample.cpu_percent)
                .map(json_f64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "memory",
            sample
                .and_then(|sample| sample.memory_usage_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "memory_limit",
            sample
                .and_then(|sample| sample.memory_limit_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "network_rx",
            sample
                .and_then(|sample| sample.network_rx_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "network_tx",
            sample
                .and_then(|sample| sample.network_tx_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "disk_read",
            sample
                .and_then(|sample| sample.disk_read_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "disk_write",
            sample
                .and_then(|sample| sample.disk_write_bytes)
                .map(json_u64)
                .unwrap_or(JsonValue::Null),
        ),
        (
            "health",
            container
                .health
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        ),
    ])
}

fn image_row(image: Image) -> Row {
    Row::from_fields([
        ("id", json_string(image.id)),
        ("repository", json_string(image.repository.clone())),
        ("name", json_string(image.repository)),
        ("tag", json_string(image.tag)),
        ("digest", json_option_string(image.digest)),
        ("size", json_string(image.size)),
        ("created_at", json_option_string(image.created_at)),
        ("labels", json_string_array(image.labels)),
    ])
}

fn network_row(network: Network) -> Row {
    let mut fields = BTreeMap::new();
    network_row_fields(network, &mut fields);
    Row { fields }
}

fn network_row_fields(network: Network, fields: &mut BTreeMap<String, JsonValue>) {
    fields.insert("id".into(), json_string(network.id));
    fields.insert("name".into(), json_string(network.name));
    fields.insert("driver".into(), json_string(network.driver));
    fields.insert("scope".into(), json_string(network.scope));
    fields.insert("containers".into(), json_string_array(network.containers));
    fields.insert("labels".into(), json_string_array(network.labels));
}

fn volume_row(volume: Volume) -> Row {
    let mut fields = BTreeMap::new();
    volume_row_fields(volume, &mut fields);
    Row { fields }
}

fn volume_row_fields(volume: Volume, fields: &mut BTreeMap<String, JsonValue>) {
    fields.insert("name".into(), json_string(volume.name));
    fields.insert("driver".into(), json_string(volume.driver));
    fields.insert("mountpoint".into(), json_option_string(volume.mountpoint));
    fields.insert("scope".into(), json_option_string(volume.scope));
    fields.insert("labels".into(), json_string_array(volume.labels));
}

impl Row {
    fn from_fields<const N: usize>(fields: [(&str, JsonValue); N]) -> Self {
        Self {
            fields: fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        }
    }
}

fn render_table_line(values: &[String], widths: &[usize]) -> String {
    values
        .iter()
        .zip(widths)
        .map(|(value, width)| format!("{value:<width$}"))
        .collect::<Vec<_>>()
        .join("  ")
}

pub fn render_csv(result: &ExecutionResult) -> String {
    if result.rows.is_empty() {
        return String::new();
    }

    let columns: Vec<String> = result.rows[0].fields.keys().cloned().collect();
    let mut buf = Vec::new();
    {
        let mut writer = csv::Writer::from_writer(&mut buf);

        // Write header
        writer
            .write_record(&columns)
            .expect("csv header write should succeed");

        // Write data rows
        for row in &result.rows {
            let values: Vec<String> = columns
                .iter()
                .map(|col| {
                    row.fields
                        .get(col)
                        .map(eval::render_json_cell)
                        .unwrap_or_default()
                })
                .collect();
            writer
                .write_record(&values)
                .expect("csv row write should succeed");
        }

        writer.flush().expect("csv flush should succeed");
    }

    String::from_utf8(buf).unwrap_or_default()
}

pub fn render_jsonl(result: &ExecutionResult) -> String {
    result
        .rows
        .iter()
        .filter_map(|row| serde_json::to_string(&row).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

fn ansi_state_color(value: &str) -> &'static str {
    match value {
        "running" => "\x1b[32m",
        "exited" | "dead" => "\x1b[31m",
        "restarting" | "paused" => "\x1b[33m",
        "created" => "\x1b[36m",
        "critical" => "\x1b[31;1m",
        "warning" => "\x1b[33m",
        "ok" | "healthy" => "\x1b[32m",
        _ => "\x1b[0m",
    }
}

/// Return ANSI green/yellow/red based on a numeric value threshold.
/// Green < 50%, yellow 50–80%, red > 80%.
fn ansi_threshold_color(value_str: &str) -> &'static str {
    // Strip trailing % or other non-numeric suffixes
    let cleaned = value_str.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.');
    if let Ok(val) = cleaned.parse::<f64>() {
        if val > 80.0 {
            "\x1b[31m" // red
        } else if val > 50.0 {
            "\x1b[33m" // yellow
        } else {
            "\x1b[32m" // green
        }
    } else {
        "\x1b[0m"
    }
}

const ANSI_RESET: &str = "\x1b[0m";

pub fn render_table_colored(result: &ExecutionResult) -> String {
    render_table_colored_with_theme(result, Theme::Dark)
}

fn render_table_colored_with_theme(result: &ExecutionResult, theme: Theme) -> String {
    if result.rows.is_empty() {
        return "No rows".to_owned();
    }

    // Filter out all-null/all-empty columns
    let columns = filter_display_columns(result);
    let entity = infer_entity_type(&columns);
    const MAX_COL_WIDTH: usize = 30;
    let mut widths = columns
        .iter()
        .map(|column| column.len().min(MAX_COL_WIDTH))
        .collect::<Vec<_>>();

    // Pre-compute size bar data for the "size" column if present
    // Uses log10 scaling to handle values across orders of magnitude (MB, GB, TB)
    let size_col_idx = columns.iter().position(|c| c == "size");
    let (size_min_bytes, size_max_bytes, size_bar_width) = if let Some(si) = size_col_idx {
        let mut min_b = f64::MAX;
        let mut max_b = 0.0_f64;
        for row in &result.rows {
            if let Some(val) = row.fields.get("size") {
                let s = eval::render_json_cell(val);
                if let Some(b) = eval::json_as_f64(&serde_json::Value::String(s)) {
                    if b > max_b {
                        max_b = b;
                    }
                    if b < min_b {
                        min_b = b;
                    }
                }
            }
        }
        let bw = 8; // fixed reasonable width for the bar
        widths[si] += bw + 1;  // make room for the bar + space
        (if min_b == f64::MAX { 0.0 } else { min_b }, max_b, bw)
    } else {
        (0.0, 0.0, 0)
    };

    let (row_alt_bg, title_color_code, header_color_code) = match theme {
        Theme::Dark => ("\x1b[100m", "\x1b[1;36m", "\x1b[1;4;37m"),
        // Light theme: no alternating row background (white-on-white is invisible),
        // use blue/dark text which is readable on light terminals.
        Theme::Light => ("", "\x1b[1;34m", "\x1b[1;4;30m"),
    };

    let rendered_rows: Vec<Vec<(String, &'static str, &'static str)>> = result
        .rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let is_even = idx % 2 == 0;
            let row_bg = if is_even { "" } else { row_alt_bg };

            columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let mut value = row
                        .fields
                        .get(column)
                        .map(eval::render_json_cell)
                        .unwrap_or_default();
                    widths[index] = widths[index].max(value.len().min(MAX_COL_WIDTH));
                    value = truncate_cell(&value, MAX_COL_WIDTH);

                    // Determine per-cell foreground color
                    let cell_color: &'static str = match column.as_str() {
                        "cpu" => ansi_threshold_color(&value),
                        "memory" | "mem" => {
                            if let Ok(bytes) = value.parse::<f64>() {
                                if bytes > 1_073_741_824.0 {
                                    "\x1b[31m"
                                } else if bytes > 536_870_912.0 {
                                    "\x1b[33m"
                                } else {
                                    "\x1b[32m"
                                }
                            } else {
                                ""
                            }
                        }
                        "state" | "status" => ansi_state_color(&value),
                        _ => "",
                    };

                    // For the "size" column, augment with a visual bar
                    if column.as_str() == "size" && size_max_bytes > 0.0 && !value.is_empty() {
                        let bar = size_bar(&value, size_min_bytes, size_max_bytes, size_bar_width);
                        if !bar.is_empty() {
                            value = format!("{value} {bar}");
                        }
                    }

                    (value, cell_color, row_bg)
                })
                .collect()
        })
        .collect();

    let mut lines = Vec::new();

    // ── Title ──
    lines.push(format!(
        "{title_color_code} {} {ANSI_RESET}",
        entity
    ));

    // ── Header ──
    lines.push(format!(
        "{header_color_code}{}{ANSI_RESET}",
        render_table_line(
            &columns.to_vec(),
            &widths
        )
    ));

    // ── Separator ──
    lines.push(
        widths
            .iter()
            .map(|width| format!("\x1b[2m{}\x1b[0m", "─".repeat(*width)))
            .collect::<Vec<_>>()
            .join("  "),
    );

    // ── Data rows ──
    for row in &rendered_rows {
        let line = row
            .iter()
            .zip(&widths)
            .map(|((value, fg_color, bg_color), width)| {
                if fg_color.is_empty() && bg_color.is_empty() {
                    format!("{value:<width$}")
                } else {
                    format!("{bg_color}{fg_color}{value:<width$}{ANSI_RESET}")
                }
            })
            .collect::<Vec<_>>()
            .join("  ");
        lines.push(line);
    }

    // ── Footer ──
    lines.push(format!(
        "\x1b[2m{} {}{ANSI_RESET}",
        result.rows.len(),
        entity.to_lowercase()
    ));

    lines.join("\n")
}

pub fn render_diff<S>(current: &ExecutionResult, store: &S) -> Result<String, ExecutorError>
where
    S: crate::storage::TelemetryStore + ?Sized,
{
    let now = chrono::Utc::now().to_rfc3339();
    let snapshot = store
        .snapshot_at_or_before(&now)
        .map_err(ExecutorError::Telemetry)?
        .ok_or(ExecutorError::SnapshotNotFound("diff"))?;

    let prev_ids: HashSet<&str> = snapshot.containers.iter().map(|c| c.id.as_str()).collect();
    let curr_ids: HashSet<&str> = current
        .rows
        .iter()
        .filter_map(|r| r.fields.get("id").and_then(|v| v.as_str()))
        .collect();

    let added: Vec<&str> = curr_ids.difference(&prev_ids).copied().collect();
    let removed: Vec<&str> = prev_ids.difference(&curr_ids).copied().collect();
    let changed: Vec<&str> = curr_ids.intersection(&prev_ids).copied().collect();

    let mut lines = Vec::new();

    if !added.is_empty() {
        lines.push(format!(
            "\x1b[32mAdded containers ({}):\x1b[0m",
            added.len()
        ));
        for id in &added {
            if let Some(row) = current
                .rows
                .iter()
                .find(|r| r.fields.get("id").and_then(|v| v.as_str()) == Some(id))
            {
                let name = row
                    .fields
                    .get("name")
                    .map(eval::render_json_cell)
                    .unwrap_or_default();
                lines.push(format!("  \x1b[32m+ {name} ({id})\x1b[0m"));
            }
        }
    }

    if !removed.is_empty() {
        lines.push(format!(
            "\x1b[31mRemoved containers ({}):\x1b[0m",
            removed.len()
        ));
        for id in &removed {
            if let Some(c) = snapshot.containers.iter().find(|c| c.id == *id) {
                lines.push(format!("  \x1b[31m- {name} ({id})\x1b[0m", name = c.name));
            }
        }
    }

    if !changed.is_empty() {
        lines.push(format!("Changed containers ({}):", changed.len()));
        for id in &changed {
            if let Some(row) = current
                .rows
                .iter()
                .find(|r| r.fields.get("id").and_then(|v| v.as_str()) == Some(id))
            {
                let name = row
                    .fields
                    .get("name")
                    .map(eval::render_json_cell)
                    .unwrap_or_default();
                let state = row
                    .fields
                    .get("state")
                    .map(eval::render_json_cell)
                    .unwrap_or_default();
                if let Some(prev_c) = snapshot.containers.iter().find(|c| c.id == *id)
                    && prev_c.state != state
                {
                    lines.push(format!(
                        "  ~ {name}: {prev} -> {state}",
                        prev = prev_c.state
                    ));
                }
            }
        }
    }

    if added.is_empty() && removed.is_empty() {
        lines.push("No changes detected.".to_owned());
    }

    Ok(lines.join("\n"))
}

fn json_string(value: String) -> JsonValue {
    JsonValue::String(value)
}

fn json_option_string(value: Option<String>) -> JsonValue {
    value.map(JsonValue::String).unwrap_or(JsonValue::Null)
}

fn json_string_array(values: Vec<String>) -> JsonValue {
    JsonValue::Array(values.into_iter().map(JsonValue::String).collect())
}

fn json_u64(value: u64) -> JsonValue {
    JsonValue::Number(Number::from(value))
}

fn json_f64(value: f64) -> JsonValue {
    Number::from_f64(value)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

fn latest_metrics_by_container(samples: Vec<MetricSample>) -> HashMap<String, MetricSample> {
    let mut latest = HashMap::new();
    for sample in samples {
        latest.insert(sample.container_id.clone(), sample.clone());
        latest.insert(sample.container_name.clone(), sample);
    }
    latest
}

fn group_rows(rows: Vec<Row>, group_fields: &[String], aggregates: &[AggregateExpr]) -> Vec<Row> {
    // Helper struct to accumulate state for each group
    struct GroupAcc {
        row: Row,
        count: u64,
        sums: HashMap<String, f64>,
        avg_sums: HashMap<String, f64>,
        avg_counts: HashMap<String, u64>,
        mins: HashMap<String, f64>,
        maxs: HashMap<String, f64>,
        min_inits: HashSet<String>,
        max_inits: HashSet<String>,
    }

    let mut groups: BTreeMap<Vec<String>, GroupAcc> = BTreeMap::new();

    for row in rows {
        let key: Vec<String> = group_fields
            .iter()
            .map(|f| {
                row.fields
                    .get(f)
                    .map(eval::render_json_cell)
                    .unwrap_or_default()
            })
            .collect();

        let acc = groups.entry(key).or_insert_with(|| {
            let mut fields = BTreeMap::new();
            for f in group_fields {
                if let Some(v) = row.fields.get(f) {
                    fields.insert(f.clone(), v.clone());
                }
            }
            GroupAcc {
                row: Row { fields },
                count: 0,
                sums: HashMap::new(),
                avg_sums: HashMap::new(),
                avg_counts: HashMap::new(),
                mins: HashMap::new(),
                maxs: HashMap::new(),
                min_inits: HashSet::new(),
                max_inits: HashSet::new(),
            }
        });

        acc.count += 1;

        // Accumulate aggregate values
        for agg in aggregates {
            let alias = &agg.alias;
            if let Some(v) = row.fields.get(&agg.field)
                && let Some(num) = eval::json_as_f64(v)
            {
                match agg.function.as_str() {
                        "sum" => {
                            *acc.sums.entry(alias.clone()).or_insert(0.0) += num;
                        }
                        "avg" => {
                            *acc.avg_sums.entry(alias.clone()).or_insert(0.0) += num;
                            *acc.avg_counts.entry(alias.clone()).or_insert(0) += 1;
                        }
                        "min" => {
                            if !acc.min_inits.contains(alias) || num < acc.mins[alias] {
                                acc.mins.insert(alias.clone(), num);
                                acc.min_inits.insert(alias.clone());
                            }
                        }
                        "max"
                            if !acc.max_inits.contains(alias) || num > acc.maxs[alias] =>
                        {
                            acc.maxs.insert(alias.clone(), num);
                            acc.max_inits.insert(alias.clone());
                        }
                        _ => {}
                    }
            }
        }
    }

    // Build result rows with aggregate values
    groups
        .into_values()
        .map(|acc| {
            let mut row = acc.row;
            if aggregates.is_empty() {
                // Backward compatible: add default count column
                row.fields.insert(
                    "count".to_owned(),
                    JsonValue::Number(Number::from(acc.count)),
                );
            } else {
                for agg in aggregates {
                    let alias = &agg.alias;
                    match agg.function.as_str() {
                        "count" => {
                            row.fields
                                .insert(alias.clone(), JsonValue::Number(Number::from(acc.count)));
                        }
                        "sum" => {
                            let val = acc.sums.get(alias).copied().unwrap_or(0.0);
                            row.fields.insert(alias.clone(), json_f64(val));
                        }
                        "avg" => {
                            let cnt = acc.avg_counts.get(alias).copied().unwrap_or(0);
                            let val = if cnt > 0 {
                                acc.avg_sums.get(alias).copied().unwrap_or(0.0) / cnt as f64
                            } else {
                                0.0
                            };
                            row.fields.insert(alias.clone(), json_f64(val));
                        }
                        "min" => {
                            if acc.min_inits.contains(alias) {
                                if let Some(val) = acc.mins.get(alias) {
                                    row.fields.insert(alias.clone(), json_f64(*val));
                                }
                            } else {
                                row.fields.insert(alias.clone(), JsonValue::Null);
                            }
                        }
                        "max" => {
                            if acc.max_inits.contains(alias) {
                                if let Some(val) = acc.maxs.get(alias) {
                                    row.fields.insert(alias.clone(), json_f64(*val));
                                }
                            } else {
                                row.fields.insert(alias.clone(), JsonValue::Null);
                            }
                        }
                        _ => {}
                    }
                }
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{
        ast::{ComposeTarget, ConfigTarget, Query},
        docker::MockDockerClient,
        metrics::MockMetricsCollector,
        parser,
    };

    use super::*;

    #[test]
    fn observes_containers_from_mock_client() {
        let client = mock_client();
        let parsed = parser::parse("observe containers").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
    }

    #[test]
    fn filters_containers() {
        let client = mock_client();
        let parsed =
            parser::parse("observe containers where state = running").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
    }

    #[test]
    fn applies_pipeline_select_sort_and_limit() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | sort by restart_count desc | limit 1 | select name, restart_count",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields.keys().cloned().collect::<Vec<_>>(),
            vec!["name".to_owned(), "restart_count".to_owned()]
        );
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("worker".to_owned())
        );
    }

    #[test]
    fn filters_containers_by_cpu_metric() {
        let client = mock_client();
        let metrics = MockMetricsCollector {
            samples: vec![
                MetricSample {
                    container_id: "abc".to_owned(),
                    container_name: "api".to_owned(),
                    timestamp: "2026-05-31T02:00:00Z".to_owned(),
                    cpu_percent: Some(87.5),
                    memory_usage_bytes: Some(128),
                    memory_limit_bytes: Some(1024),
                    network_rx_bytes: Some(10),
                    network_tx_bytes: Some(20),
                    disk_read_bytes: Some(30),
                    disk_write_bytes: Some(40),
                },
                MetricSample {
                    container_id: "def".to_owned(),
                    container_name: "worker".to_owned(),
                    timestamp: "2026-05-31T02:00:00Z".to_owned(),
                    cpu_percent: Some(12.0),
                    memory_usage_bytes: Some(64),
                    memory_limit_bytes: Some(1024),
                    network_rx_bytes: Some(1),
                    network_tx_bytes: Some(2),
                    disk_read_bytes: Some(3),
                    disk_write_bytes: Some(4),
                },
            ],
        };
        let parsed = parser::parse("observe containers | where cpu > 80% | select name, cpu")
            .expect("query should parse");

        let result =
            execute_with_metrics(&parsed.query, &client, &metrics).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
        assert_eq!(result.rows[0].fields["cpu"], json_f64(87.5));
    }

    #[test]
    fn observes_images() {
        let client = mock_client();
        let parsed =
            parser::parse("observe images | select repository, tag").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["repository"],
            JsonValue::String("postgres".to_owned())
        );
    }

    #[test]
    fn groups_by_state_with_sum() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | group by state with sum(restart_count) as total_restarts",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let running = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("running".to_owned()))
            .expect("running group");
        let exited = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("exited".to_owned()))
            .expect("exited group");
        assert_eq!(running.fields["total_restarts"], json_f64(0.0));
        assert_eq!(exited.fields["total_restarts"], json_f64(4.0));
    }

    #[test]
    fn groups_by_state_with_avg_cpu() {
        let client = mock_client();
        let metrics = MockMetricsCollector {
            samples: vec![
                MetricSample {
                    container_id: "abc".to_owned(),
                    container_name: "api".to_owned(),
                    timestamp: "2026-05-31T02:00:00Z".to_owned(),
                    cpu_percent: Some(50.0),
                    memory_usage_bytes: Some(128),
                    memory_limit_bytes: Some(1024),
                    network_rx_bytes: Some(10),
                    network_tx_bytes: Some(20),
                    disk_read_bytes: Some(30),
                    disk_write_bytes: Some(40),
                },
                MetricSample {
                    container_id: "def".to_owned(),
                    container_name: "worker".to_owned(),
                    timestamp: "2026-05-31T02:00:00Z".to_owned(),
                    cpu_percent: Some(30.0),
                    memory_usage_bytes: Some(64),
                    memory_limit_bytes: Some(1024),
                    network_rx_bytes: Some(1),
                    network_tx_bytes: Some(2),
                    disk_read_bytes: Some(3),
                    disk_write_bytes: Some(4),
                },
            ],
        };
        let parsed = parser::parse("observe containers | group by state with avg(cpu) as avg_cpu")
            .expect("query should parse");

        let result =
            execute_with_metrics(&parsed.query, &client, &metrics).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let running = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("running".to_owned()))
            .expect("running group");
        let exited = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("exited".to_owned()))
            .expect("exited group");
        assert_eq!(running.fields["avg_cpu"], json_f64(50.0));
        assert_eq!(exited.fields["avg_cpu"], json_f64(30.0));
    }

    #[test]
    fn groups_by_state_with_min_max() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | group by state with min(restart_count) as min_r, max(restart_count) as max_r",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let running = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("running".to_owned()))
            .expect("running group");
        let exited = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("exited".to_owned()))
            .expect("exited group");
        assert_eq!(running.fields["min_r"], json_f64(0.0));
        assert_eq!(running.fields["max_r"], json_f64(0.0));
        assert_eq!(exited.fields["min_r"], json_f64(4.0));
        assert_eq!(exited.fields["max_r"], json_f64(4.0));
    }

    #[test]
    fn groups_by_state_with_count_alias() {
        let client = mock_client();
        let parsed = parser::parse("observe containers | group by state with count(id) as cnt")
            .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let running = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("running".to_owned()))
            .expect("running group");
        let exited = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("exited".to_owned()))
            .expect("exited group");
        assert_eq!(running.fields["cnt"], JsonValue::Number(Number::from(1)));
        assert_eq!(exited.fields["cnt"], JsonValue::Number(Number::from(1)));
        // When explicit aggregates are used, no default `count` column
        assert!(!running.fields.contains_key("count"));
    }

    #[test]
    fn groups_rows_by_state() {
        let client = mock_client();
        let parsed =
            parser::parse("observe containers | group by state").expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let running = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("running".to_owned()))
            .expect("running group");
        let exited = result
            .rows
            .iter()
            .find(|row| row.fields["state"] == JsonValue::String("exited".to_owned()))
            .expect("exited group");
        assert_eq!(running.fields["count"], JsonValue::Number(Number::from(1)));
        assert_eq!(exited.fields["count"], JsonValue::Number(Number::from(1)));
    }

    #[test]
    fn rejects_unknown_field() {
        let client = mock_client();
        let parsed =
            parser::parse("observe containers | select missing").expect("query should parse");

        let error = execute(&parsed.query, &client).unwrap_err();

        assert!(matches!(
            error,
            ExecutorError::Eval(eval::EvalError::UnsupportedField { .. })
        ));
    }

    #[test]
    fn rejects_invalid_type_comparison() {
        let client = mock_client();
        let parsed =
            parser::parse("observe containers | where state > 50").expect("query should parse");

        let error = execute(&parsed.query, &client).unwrap_err();

        assert!(matches!(
            error,
            ExecutorError::Eval(eval::EvalError::InvalidComparison { .. })
        ));
    }

    #[test]
    fn renders_table() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api".to_owned())),
                ("state", JsonValue::String("running".to_owned())),
            ])],
        };

        let table = render_table(&result);

        assert!(table.contains("name"));
        assert!(table.contains("running"));
    }

    #[test]
    fn sets_literal_field() {
        let client = mock_client();
        let parsed = parser::parse("observe containers | set tier = \"prod\" | select name, tier")
            .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["tier"],
            JsonValue::String("prod".to_owned())
        );
    }

    #[test]
    fn sets_case_field() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | set health = case when state = running then \"up\" else \"down\" end | select name, health",
        ).expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["health"],
            JsonValue::String("up".to_owned())
        );
        assert_eq!(
            result.rows[1].fields["health"],
            JsonValue::String("down".to_owned())
        );
    }

    #[test]
    fn applies_if_pipeline() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if state = running then set status_label = \"active\" else set status_label = \"inactive\" | select name, status_label",
        ).expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["status_label"],
            JsonValue::String("active".to_owned())
        );
        assert_eq!(
            result.rows[1].fields["status_label"],
            JsonValue::String("inactive".to_owned())
        );
    }

    #[test]
    fn executes_compose_query() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        // Only myapp containers should be returned
        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
        assert_eq!(
            result.rows[1].fields["name"],
            JsonValue::String("db".to_owned())
        );
    }

    #[test]
    fn executes_compose_with_pipeline() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp | select name, state, compose_project")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert_eq!(
                row.fields["compose_project"],
                JsonValue::String("myapp".to_owned())
            );
        }
    }

    #[test]
    fn executes_compose_services() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp services | select name, service")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(
            api.fields["service"],
            JsonValue::String("api-service".to_owned())
        );
        let db = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "db")
            .unwrap();
        assert_eq!(
            db.fields["service"],
            JsonValue::String("database".to_owned())
        );
    }

    #[test]
    fn executes_compose_empty_for_missing_project() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose nonexistent").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 0);
    }

    fn compose_mock_client() -> MockDockerClient {
        MockDockerClient {
            containers: vec![
                Container {
                    id: "abc".to_owned(),
                    name: "api".to_owned(),
                    image: "api:latest".to_owned(),
                    status: "Up 2 minutes".to_owned(),
                    state: "running".to_owned(),
                    ports: vec!["8080/tcp".to_owned()],
                    labels: vec![
                        "com.docker.compose.project=myapp".to_owned(),
                        "com.docker.compose.service=api-service".to_owned(),
                    ],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                    health: Some("healthy".to_owned()),
                },
                Container {
                    id: "def".to_owned(),
                    name: "db".to_owned(),
                    image: "postgres:16".to_owned(),
                    status: "Up 5 minutes".to_owned(),
                    state: "running".to_owned(),
                    ports: vec!["5432/tcp".to_owned()],
                    labels: vec![
                        "com.docker.compose.project=myapp".to_owned(),
                        "com.docker.compose.service=database".to_owned(),
                    ],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(1),
                    health: Some("unhealthy".to_owned()),
                },
                Container {
                    id: "xyz".to_owned(),
                    name: "worker".to_owned(),
                    image: "worker:latest".to_owned(),
                    status: "Exited (1) 1 minute ago".to_owned(),
                    state: "exited".to_owned(),
                    ports: Vec::new(),
                    labels: vec!["role=worker".to_owned()], // Not part of any compose project
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(4),
                    health: None,
                },
            ],
            images: vec![Image {
                id: "img".to_owned(),
                repository: "postgres".to_owned(),
                tag: "16".to_owned(),
                digest: None,
                size: "432MB".to_owned(),
                created_at: None,
                labels: Vec::new(),
            }],
            networks: vec![
                Network {
                    id: "net1".to_owned(),
                    name: "myapp_default".to_owned(),
                    driver: "bridge".to_owned(),
                    scope: "local".to_owned(),
                    containers: vec!["abc".to_owned(), "def".to_owned()],
                    labels: vec![
                        "com.docker.compose.project=myapp".to_owned(),
                        "com.docker.compose.network=default".to_owned(),
                    ],
                },
                Network {
                    id: "net2".to_owned(),
                    name: "myapp_frontend".to_owned(),
                    driver: "bridge".to_owned(),
                    scope: "local".to_owned(),
                    containers: vec!["abc".to_owned()],
                    labels: vec![
                        "com.docker.compose.project=myapp".to_owned(),
                        "com.docker.compose.network=frontend".to_owned(),
                    ],
                },
                Network {
                    id: "net3".to_owned(),
                    name: "bridge".to_owned(),
                    driver: "bridge".to_owned(),
                    scope: "local".to_owned(),
                    containers: Vec::new(),
                    labels: Vec::new(),
                },
            ],
            volumes: vec![
                Volume {
                    name: "myapp_pgdata".to_owned(),
                    driver: "local".to_owned(),
                    mountpoint: Some("/var/lib/docker/volumes/myapp_pgdata/_data".to_owned()),
                    scope: Some("local".to_owned()),
                    labels: vec![
                        "com.docker.compose.project=myapp".to_owned(),
                        "com.docker.compose.volume=pgdata".to_owned(),
                    ],
                },
                Volume {
                    name: "pgdata".to_owned(),
                    driver: "local".to_owned(),
                    mountpoint: Some("/var/lib/docker/volumes/pgdata/_data".to_owned()),
                    scope: Some("local".to_owned()),
                    labels: Vec::new(),
                },
            ],
            logs: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn executes_compose_with_where_filter() {
        let client = compose_mock_client();
        let parsed =
            parser::parse("compose myapp | where state = \"running\"").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert_eq!(row.fields["state"], JsonValue::String("running".to_owned()));
        }
    }

    #[test]
    fn executes_compose_with_sort() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp | sort by name asc").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
        assert_eq!(
            result.rows[1].fields["name"],
            JsonValue::String("db".to_owned())
        );
    }

    #[test]
    fn executes_compose_with_limit() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp | limit 1").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
    }

    #[test]
    fn executes_compose_with_select() {
        let client = compose_mock_client();
        let parsed =
            parser::parse("compose myapp | select name, image").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert_eq!(row.fields.len(), 2);
            assert!(row.fields.contains_key("name"));
            assert!(row.fields.contains_key("image"));
        }
    }

    #[test]
    fn executes_compose_with_group_by_state() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp | group by state with count(id) as cnt")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        let running_row = result
            .rows
            .iter()
            .find(|r| r.fields["state"] == JsonValue::String("running".to_owned()))
            .unwrap();
        assert_eq!(running_row.fields["cnt"], JsonValue::Number(2.into()));
    }

    #[test]
    fn executes_compose_services_without_service_label() {
        let client = MockDockerClient {
            containers: vec![Container {
                id: "abc".to_owned(),
                name: "orphan".to_owned(),
                image: "busybox:latest".to_owned(),
                status: "Up 1 minute".to_owned(),
                state: "running".to_owned(),
                ports: Vec::new(),
                labels: vec!["com.docker.compose.project=lonely".to_owned()],
                created_at: None,
                started_at: None,
                finished_at: None,
                restart_count: Some(0),
                health: None,
            }],
            images: vec![],
            networks: vec![],
            volumes: vec![],
            logs: std::collections::HashMap::new(),
        };
        let parsed = parser::parse("compose lonely services | select name, service")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].fields["service"], JsonValue::Null);
    }

    #[test]
    fn executes_compose_excludes_non_project_containers() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp | select name").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        let names: Vec<&str> = result
            .rows
            .iter()
            .map(|r| r.fields["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"worker"));
        assert!(names.contains(&"api"));
        assert!(names.contains(&"db"));
    }

    #[test]
    fn executes_compose_with_offset_and_limit() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp | sort by name asc | offset 1 | limit 1")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("db".to_owned())
        );
    }

    #[test]
    fn executes_compose_with_distinct() {
        let client = compose_mock_client();
        let parsed =
            parser::parse("compose myapp | select state | distinct").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["state"],
            JsonValue::String("running".to_owned())
        );
    }

    #[test]
    fn executes_historical_compose_query() {
        use crate::storage::{InMemoryTelemetryStore, TelemetrySnapshot};

        let mut store = InMemoryTelemetryStore::default();
        store
            .write_snapshot(TelemetrySnapshot {
                timestamp: "2026-05-31T00:00:00Z".to_owned(),
                containers: vec![
                    Container {
                        id: "h1".to_owned(),
                        name: "hist-api".to_owned(),
                        image: "api:v1".to_owned(),
                        status: "Up 1 hour".to_owned(),
                        state: "running".to_owned(),
                        ports: vec!["8080/tcp".to_owned()],
                        labels: vec![
                            "com.docker.compose.project=histapp".to_owned(),
                            "com.docker.compose.service=api".to_owned(),
                        ],
                        created_at: None,
                        started_at: None,
                        finished_at: None,
                        restart_count: Some(0),
                        health: None,
                    },
                    Container {
                        id: "h2".to_owned(),
                        name: "hist-db".to_owned(),
                        image: "postgres:16".to_owned(),
                        status: "Up 1 hour".to_owned(),
                        state: "running".to_owned(),
                        ports: vec!["5432/tcp".to_owned()],
                        labels: vec![
                            "com.docker.compose.project=histapp".to_owned(),
                            "com.docker.compose.service=db".to_owned(),
                        ],
                        created_at: None,
                        started_at: None,
                        finished_at: None,
                        restart_count: Some(0),
                        health: None,
                    },
                    Container {
                        id: "h3".to_owned(),
                        name: "standalone".to_owned(),
                        image: "nginx:latest".to_owned(),
                        status: "Up 30 minutes".to_owned(),
                        state: "running".to_owned(),
                        ports: Vec::new(),
                        labels: vec![],
                        created_at: None,
                        started_at: None,
                        finished_at: None,
                        restart_count: Some(0),
                        health: None,
                    },
                ],
                images: vec![],
                networks: vec![],
                volumes: vec![],
            })
            .unwrap();

        let parsed =
            parser::parse("compose histapp | select name, state").expect("query should parse");
        let result = execute_with_store(&parsed.query, &store).expect("should execute");
        assert_eq!(result.rows.len(), 2);
        let names: Vec<&str> = result
            .rows
            .iter()
            .map(|r| r.fields["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"hist-api"));
        assert!(names.contains(&"hist-db"));
        assert!(!names.contains(&"standalone"));
    }

    #[test]
    fn executes_historical_compose_services() {
        use crate::storage::{InMemoryTelemetryStore, TelemetrySnapshot};

        let mut store = InMemoryTelemetryStore::default();
        store
            .write_snapshot(TelemetrySnapshot {
                timestamp: "2026-05-31T00:00:00Z".to_owned(),
                containers: vec![Container {
                    id: "h1".to_owned(),
                    name: "web".to_owned(),
                    image: "web:latest".to_owned(),
                    status: "Up".to_owned(),
                    state: "running".to_owned(),
                    ports: Vec::new(),
                    labels: vec![
                        "com.docker.compose.project=webapp".to_owned(),
                        "com.docker.compose.service=frontend".to_owned(),
                    ],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                    health: None,
                }],
                images: vec![],
                networks: vec![],
                volumes: vec![],
            })
            .unwrap();

        let parsed = parser::parse("compose webapp services | select name, service")
            .expect("query should parse");
        let result = execute_with_store(&parsed.query, &store).expect("should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["service"],
            JsonValue::String("frontend".to_owned())
        );
    }

    #[test]
    fn executes_historical_compose_empty_for_missing_project() {
        use crate::storage::{InMemoryTelemetryStore, TelemetrySnapshot};

        let mut store = InMemoryTelemetryStore::default();
        store
            .write_snapshot(TelemetrySnapshot {
                timestamp: "2026-05-31T00:00:00Z".to_owned(),
                containers: vec![Container {
                    id: "x".to_owned(),
                    name: "solo".to_owned(),
                    image: "nginx:latest".to_owned(),
                    status: "Up".to_owned(),
                    state: "running".to_owned(),
                    ports: Vec::new(),
                    labels: vec![],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                    health: None,
                }],
                images: vec![],
                networks: vec![],
                volumes: vec![],
            })
            .unwrap();

        let parsed = parser::parse("compose nonexistent").expect("query should parse");
        let result = execute_with_store(&parsed.query, &store).expect("should execute");
        assert_eq!(result.rows.len(), 0);
    }

    #[test]
    fn executes_compose_networks() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp networks").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        let names: Vec<&str> = result
            .rows
            .iter()
            .map(|r| r.fields["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"myapp_default"));
        assert!(names.contains(&"myapp_frontend"));
        assert!(!names.contains(&"bridge"));
    }

    #[test]
    fn executes_compose_networks_with_pipeline() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp networks | select name, driver")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert!(row.fields.contains_key("name"));
            assert!(row.fields.contains_key("driver"));
            assert_eq!(row.fields.len(), 2);
        }
    }

    #[test]
    fn executes_compose_networks_empty_for_missing_project() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose nonexistent networks").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 0);
    }

    #[test]
    fn executes_compose_volumes() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp volumes").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("myapp_pgdata".to_owned())
        );
    }

    #[test]
    fn executes_compose_volumes_with_pipeline() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp volumes | select name, driver")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("myapp_pgdata".to_owned())
        );
        assert_eq!(
            result.rows[0].fields["driver"],
            JsonValue::String("local".to_owned())
        );
    }

    #[test]
    fn executes_compose_health() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp health | select name, service, health")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(
            api.fields["health"],
            JsonValue::String("healthy".to_owned())
        );
        let db = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "db")
            .unwrap();
        assert_eq!(
            db.fields["health"],
            JsonValue::String("unhealthy".to_owned())
        );
    }

    #[test]
    fn executes_compose_health_with_filter() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp health | where health = \"unhealthy\"")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("db".to_owned())
        );
    }

    #[test]
    fn executes_historical_compose_networks() {
        use crate::storage::{InMemoryTelemetryStore, TelemetrySnapshot};

        let mut store = InMemoryTelemetryStore::default();
        store
            .write_snapshot(TelemetrySnapshot {
                timestamp: "2026-05-31T00:00:00Z".to_owned(),
                containers: vec![],
                images: vec![],
                networks: vec![
                    Network {
                        id: "n1".to_owned(),
                        name: "hist_default".to_owned(),
                        driver: "bridge".to_owned(),
                        scope: "local".to_owned(),
                        containers: Vec::new(),
                        labels: vec!["com.docker.compose.project=histapp".to_owned()],
                    },
                    Network {
                        id: "n2".to_owned(),
                        name: "bridge".to_owned(),
                        driver: "bridge".to_owned(),
                        scope: "local".to_owned(),
                        containers: Vec::new(),
                        labels: Vec::new(),
                    },
                ],
                volumes: vec![],
            })
            .unwrap();

        let parsed =
            parser::parse("compose histapp networks | select name").expect("query should parse");
        let result = execute_with_store(&parsed.query, &store).expect("should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("hist_default".to_owned())
        );
    }

    #[test]
    fn executes_historical_compose_volumes() {
        use crate::storage::{InMemoryTelemetryStore, TelemetrySnapshot};

        let mut store = InMemoryTelemetryStore::default();
        store
            .write_snapshot(TelemetrySnapshot {
                timestamp: "2026-05-31T00:00:00Z".to_owned(),
                containers: vec![],
                images: vec![],
                networks: vec![],
                volumes: vec![Volume {
                    name: "hist_data".to_owned(),
                    driver: "local".to_owned(),
                    mountpoint: None,
                    scope: Some("local".to_owned()),
                    labels: vec!["com.docker.compose.project=histapp".to_owned()],
                }],
            })
            .unwrap();

        let parsed = parser::parse("compose histapp volumes | select name, driver")
            .expect("query should parse");
        let result = execute_with_store(&parsed.query, &store).expect("should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("hist_data".to_owned())
        );
    }

    fn join_mock_client() -> MockDockerClient {
        MockDockerClient {
            containers: vec![
                Container {
                    id: "i1".to_owned(),
                    name: "web".to_owned(),
                    image: "nginx:latest".to_owned(),
                    status: "Up 1 hour".to_owned(),
                    state: "running".to_owned(),
                    ports: vec!["80/tcp".to_owned()],
                    labels: vec![],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                    health: None,
                },
                Container {
                    id: "i2".to_owned(),
                    name: "db".to_owned(),
                    image: "postgres:16".to_owned(),
                    status: "Up 2 hours".to_owned(),
                    state: "running".to_owned(),
                    ports: vec!["5432/tcp".to_owned()],
                    labels: vec![],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(1),
                    health: None,
                },
            ],
            images: vec![
                Image {
                    id: "i1".to_owned(),
                    repository: "nginx".to_owned(),
                    tag: "latest".to_owned(),
                    digest: None,
                    size: "187MB".to_owned(),
                    created_at: None,
                    labels: Vec::new(),
                },
                Image {
                    id: "i2".to_owned(),
                    repository: "postgres".to_owned(),
                    tag: "16".to_owned(),
                    digest: None,
                    size: "432MB".to_owned(),
                    created_at: None,
                    labels: Vec::new(),
                },
            ],
            networks: vec![
                Network {
                    id: "frontend".to_owned(),
                    name: "frontend".to_owned(),
                    driver: "bridge".to_owned(),
                    scope: "local".to_owned(),
                    containers: vec!["web".to_owned()],
                    labels: Vec::new(),
                },
                Network {
                    id: "backend".to_owned(),
                    name: "backend".to_owned(),
                    driver: "bridge".to_owned(),
                    scope: "local".to_owned(),
                    containers: vec!["db".to_owned()],
                    labels: Vec::new(),
                },
            ],
            volumes: vec![Volume {
                name: "pgdata".to_owned(),
                driver: "local".to_owned(),
                mountpoint: Some("/var/lib/docker/volumes/pgdata/_data".to_owned()),
                scope: Some("local".to_owned()),
                labels: Vec::new(),
            }],
            logs: std::collections::HashMap::new(),
        }
    }

    fn mock_client() -> MockDockerClient {
        MockDockerClient {
            containers: vec![
                Container {
                    id: "abc".to_owned(),
                    name: "api".to_owned(),
                    image: "api:latest".to_owned(),
                    status: "Up 2 minutes".to_owned(),
                    state: "running".to_owned(),
                    ports: vec!["8080/tcp".to_owned()],
                    labels: vec!["role=api".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(0),
                    health: None,
                },
                Container {
                    id: "def".to_owned(),
                    name: "worker".to_owned(),
                    image: "worker:latest".to_owned(),
                    status: "Exited (1) 1 minute ago".to_owned(),
                    state: "exited".to_owned(),
                    ports: Vec::new(),
                    labels: vec!["role=worker".to_owned()],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: Some(4),
                    health: None,
                },
            ],
            images: vec![Image {
                id: "img".to_owned(),
                repository: "postgres".to_owned(),
                tag: "16".to_owned(),
                digest: None,
                size: "432MB".to_owned(),
                created_at: None,
                labels: Vec::new(),
            }],
            networks: vec![Network {
                id: "net".to_owned(),
                name: "bridge".to_owned(),
                driver: "bridge".to_owned(),
                scope: "local".to_owned(),
                containers: vec!["api".to_owned()],
                labels: Vec::new(),
            }],
            volumes: vec![Volume {
                name: "pgdata".to_owned(),
                driver: "local".to_owned(),
                mountpoint: Some("/var/lib/docker/volumes/pgdata/_data".to_owned()),
                scope: Some("local".to_owned()),
                labels: Vec::new(),
            }],
            logs: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn renders_csv() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api".to_owned())),
                ("state", JsonValue::String("running".to_owned())),
            ])],
        };
        let csv = render_csv(&result);
        assert!(csv.contains("name,state"));
        assert!(csv.contains("api,running"));
    }

    #[test]
    fn renders_jsonl() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([(
                "name",
                JsonValue::String("api".to_owned()),
            )])],
        };
        let jsonl = render_jsonl(&result);
        assert!(jsonl.contains("\"name\":\"api\""));
    }

    #[test]
    fn label_field_access_in_pipeline() {
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | where label.role = \"api\" | select name, label.role",
        )
        .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["label.role"],
            JsonValue::String("api".to_owned())
        );
    }

    #[test]
    fn execute_fields_containers() {
        let result = execute_fields(CollectionTarget::Containers).expect("fields should execute");
        assert!(!result.rows.is_empty());
        let field_names: Vec<&str> = result
            .rows
            .iter()
            .map(|r| r.fields["field"].as_str().unwrap())
            .collect();
        assert!(field_names.contains(&"name"));
        assert!(field_names.contains(&"cpu"));
    }

    // ── if branching ──────────────────────────────────────────────

    #[test]
    fn if_without_else_passes_through() {
        // No else branch — unmatched rows pass through unchanged, matched rows get set
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if state = running then set priority = \"high\" | select name, state",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        // Both rows pass through (matched: priority set; unmatched: unchanged)
        assert_eq!(result.rows.len(), 2);
        // Running container has priority
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(api.fields["state"], JsonValue::String("running".to_owned()));
        // Exited container passes through
        let worker = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "worker")
            .unwrap();
        assert_eq!(
            worker.fields["state"],
            JsonValue::String("exited".to_owned())
        );
    }

    #[test]
    fn else_if_chain() {
        // else-if chaining with three conditions
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if state = running then set group = \"active\" else if state = exited then set group = \"stopped\" else set group = \"other\" | select name, state, group",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(api.fields["group"], JsonValue::String("active".to_owned()));
        let worker = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "worker")
            .unwrap();
        assert_eq!(
            worker.fields["group"],
            JsonValue::String("stopped".to_owned())
        );
    }

    #[test]
    fn nested_if_in_then() {
        // Nested if inside the then branch of an outer if
        // api: restart_count=0 -> outer else -> set status = "stable"
        // worker: restart_count=4 -> outer then -> inner if state=exited -> set status = "crashed"
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if restart_count > 0 then if state = \"exited\" then set status = \"crashed\" else set status = \"restarting\" else set status = \"stable\" | select name, state, status",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(api.fields["status"], JsonValue::String("stable".to_owned()));
        let worker = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "worker")
            .unwrap();
        assert_eq!(
            worker.fields["status"],
            JsonValue::String("crashed".to_owned())
        );
    }

    #[test]
    fn nested_if_in_else() {
        // Nested if inside the else branch (else-if with additional nesting in the if)
        // api: state=running -> outer then -> set label = "up"
        // worker: state=exited -> outer else -> inner if restart_count>0 -> set label = "crashed"
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if state = running then set label = \"up\" else if restart_count > 0 then set label = \"crashed\" else set label = \"down\" | select name, state, label",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(api.fields["label"], JsonValue::String("up".to_owned()));
        let worker = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "worker")
            .unwrap();
        assert_eq!(
            worker.fields["label"],
            JsonValue::String("crashed".to_owned())
        );
    }

    #[test]
    fn multiple_nodes_in_branches() {
        // Multiple pipeline nodes in both then and else branches
        // then: set a=1, set b=2
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if state = running then set a = 1 | set b = 2 else set a = 0 | set b = 0 | select name, state, a, b",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(api.fields["a"], JsonValue::Number(Number::from(1)));
        assert_eq!(api.fields["b"], JsonValue::Number(Number::from(2)));
        let worker = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "worker")
            .unwrap();
        assert_eq!(worker.fields["a"], JsonValue::Number(Number::from(0)));
        assert_eq!(worker.fields["b"], JsonValue::Number(Number::from(0)));
    }

    #[test]
    fn if_with_set_in_both_branches() {
        // Set different values in then vs else
        let client = mock_client();
        let parsed = parser::parse(
            "observe containers | if state = running then set tier = \"prod\" else set tier = \"dev\" | select name, state, tier",
        )
        .expect("query should parse");

        let result = execute(&parsed.query, &client).expect("query should execute");

        assert_eq!(result.rows.len(), 2);
        let api = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "api")
            .unwrap();
        assert_eq!(api.fields["tier"], JsonValue::String("prod".to_owned()));
        let worker = result
            .rows
            .iter()
            .find(|r| r.fields["name"] == "worker")
            .unwrap();
        assert_eq!(worker.fields["tier"], JsonValue::String("dev".to_owned()));
    }

    // ── Render helpers ──────────────────────────────────────────

    #[test]
    fn renders_table_with_no_rows() {
        let result = ExecutionResult { rows: vec![] };
        assert_eq!(render_table(&result), "No rows");
    }

    #[test]
    fn renders_csv_empty() {
        let result = ExecutionResult { rows: vec![] };
        assert_eq!(render_csv(&result), "");
    }

    #[test]
    fn renders_csv_with_commas_and_quotes() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api,prod".to_owned())),
                ("note", JsonValue::String("contains \"quotes\"".to_owned())),
            ])],
        };
        let csv = render_csv(&result);
        // The csv crate should properly quote fields with commas or quotes
        assert!(
            csv.contains("\"api,prod\""),
            "comma field should be quoted: {csv}"
        );
        assert!(
            csv.contains("\"contains \"\"quotes\"\"\""),
            "quote field should be escaped: {csv}"
        );
    }

    #[test]
    fn renders_table_colored_basic() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api".to_owned())),
                ("state", JsonValue::String("running".to_owned())),
            ])],
        };
        let table = render_table_colored(&result);
        // Should contain ANSI color codes
        assert!(
            table.contains("\x1b["),
            "colored output should have ANSI codes"
        );
        assert!(table.contains("api"));
        assert!(table.contains("running"));
    }

    #[test]
    fn renders_table_colored_cpu_thresholds() {
        let result = ExecutionResult {
            rows: vec![
                Row::from_fields([
                    ("name", JsonValue::String("low".to_owned())),
                    ("cpu", json_f64(25.0)),
                ]),
                Row::from_fields([
                    ("name", JsonValue::String("mid".to_owned())),
                    ("cpu", json_f64(65.0)),
                ]),
                Row::from_fields([
                    ("name", JsonValue::String("high".to_owned())),
                    ("cpu", json_f64(95.0)),
                ]),
            ],
        };
        let table = render_table_colored(&result);
        // green (\x1b[32m) for <50%
        assert!(table.contains("\x1b[32m25"));
        // yellow (\x1b[33m) for 50-80%
        assert!(table.contains("\x1b[33m65"));
        // red (\x1b[31m) for >80%
        assert!(table.contains("\x1b[31m95"));
    }

    #[test]
    fn renders_table_colored_memory_thresholds() {
        let result = ExecutionResult {
            rows: vec![
                Row::from_fields([
                    ("name", JsonValue::String("small".to_owned())),
                    ("memory", json_u64(268_435_456)), // 256MB
                ]),
                Row::from_fields([
                    ("name", JsonValue::String("medium".to_owned())),
                    ("memory", json_u64(805_306_368)), // 768MB
                ]),
                Row::from_fields([
                    ("name", JsonValue::String("large".to_owned())),
                    ("memory", json_u64(2_147_483_648)), // 2GB
                ]),
            ],
        };
        let table = render_table_colored(&result);
        // green (\x1b[32m) for <512MB
        assert!(table.contains("\x1b[32m268435456"));
        // yellow (\x1b[33m) for 512MB-1GB
        assert!(table.contains("\x1b[33m805306368"));
        // red (\x1b[31m) for >1GB
        assert!(table.contains("\x1b[31m2147483648"));
    }

    #[test]
    fn renders_table_colored_empty() {
        let result = ExecutionResult { rows: vec![] };
        assert_eq!(render_table_colored(&result), "No rows");
    }

    #[test]
    fn executes_logs_query() {
        let client = {
            let mut c = mock_client();
            c.logs.insert(
                "abc".to_owned(),
                vec![
                    "line 1".to_owned(),
                    "line 2".to_owned(),
                    "line 3".to_owned(),
                ],
            );
            c
        };
        let parsed = parser::parse("logs container api tail 10").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 3);
        assert_eq!(
            result.rows[0].fields["line"],
            JsonValue::Number(Number::from(1))
        );
        assert_eq!(
            result.rows[0].fields["message"],
            JsonValue::String("line 1".to_owned())
        );
        assert_eq!(
            result.rows[2].fields["message"],
            JsonValue::String("line 3".to_owned())
        );
    }

    #[test]
    fn executes_logs_query_empty() {
        let client = mock_client();
        let parsed = parser::parse("logs container worker").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 0);
    }

    #[test]
    fn executes_ping_query() {
        let client = mock_client();
        let parsed = parser::parse("ping").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["status"],
            JsonValue::String("ok".to_owned())
        );
    }

    #[test]
    fn renders_colored_header_is_bold_white() {
        let result = ExecutionResult {
            rows: vec![Row::from_fields([
                ("name", JsonValue::String("api".to_owned())),
                ("cpu", json_f64(50.0)),
            ])],
        };
        let table = render_table_colored(&result);
        // Header should be bold white (\x1b[1;37m)
        assert!(table.contains("\x1b[1;4;37m"));
        assert!(table.contains(ANSI_RESET));
    }

    #[test]
    fn renders_jsonl_with_multiple_rows() {
        let result = ExecutionResult {
            rows: vec![
                Row::from_fields([("name", JsonValue::String("a".to_owned()))]),
                Row::from_fields([("name", JsonValue::String("b".to_owned()))]),
            ],
        };
        let jsonl = render_jsonl(&result);
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"name\":\"a\""));
        assert!(lines[1].contains("\"name\":\"b\""));
    }

    #[test]
    fn executes_join_images_on_id() {
        let client = join_mock_client();
        let parsed =
            parser::parse("observe containers join images on id = id").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert!(row.fields.contains_key("c.id"));
            assert!(row.fields.contains_key("i.id"));
        }
        let web = result
            .rows
            .iter()
            .find(|r| r.fields["c.name"] == "web")
            .unwrap();
        assert_eq!(
            web.fields["c.image"],
            JsonValue::String("nginx:latest".to_owned())
        );
        assert_eq!(
            web.fields["i.repository"],
            JsonValue::String("nginx".to_owned())
        );
        let db = result
            .rows
            .iter()
            .find(|r| r.fields["c.name"] == "db")
            .unwrap();
        assert_eq!(
            db.fields["c.image"],
            JsonValue::String("postgres:16".to_owned())
        );
        assert_eq!(
            db.fields["i.repository"],
            JsonValue::String("postgres".to_owned())
        );
    }

    #[test]
    fn executes_join_containers_networks_no_match() {
        let client = join_mock_client();
        let parsed = parser::parse("observe containers join networks on name = name")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 0);
    }

    #[test]
    fn executes_join_with_where() {
        let client = join_mock_client();
        let parsed = parser::parse(
            "observe containers join images on id = id | where c.image = \"nginx:latest\"",
        )
        .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["c.name"],
            JsonValue::String("web".to_owned())
        );
    }

    #[test]
    fn executes_join_with_select() {
        let client = join_mock_client();
        let parsed =
            parser::parse("observe containers join images on id = id | select c.name, i.name")
                .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert_eq!(row.fields.len(), 2);
            assert!(row.fields.contains_key("c.name"));
            assert!(row.fields.contains_key("i.name"));
        }
    }

    #[test]
    fn executes_join_containers_volumes_no_match() {
        let client = join_mock_client();
        let parsed = parser::parse("observe containers join volumes on state = scope")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 0);
    }

    #[test]
    fn executes_compose_ls() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose ls").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row.fields["project"], JsonValue::String("myapp".to_owned()));
        assert_eq!(row.fields["containers"], JsonValue::Number(2.into()));
        assert_eq!(row.fields["running"], JsonValue::Number(2.into()));
        assert_eq!(row.fields["stopped"], JsonValue::Number(0.into()));
        assert_eq!(row.fields["networks"], JsonValue::Number(2.into()));
        assert_eq!(row.fields["volumes"], JsonValue::Number(1.into()));
        assert_eq!(row.fields["status"], JsonValue::String("running".to_owned()));
    }

    #[test]
    fn executes_compose_ls_with_pipeline() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose ls | where containers > 0 | sort by project asc")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["project"],
            JsonValue::String("myapp".to_owned())
        );
    }

    #[test]
    fn executes_compose_ls_partial_status() {
        let client = MockDockerClient {
            containers: vec![
                Container {
                    id: "a1".to_owned(),
                    name: "api".to_owned(),
                    image: "api:latest".to_owned(),
                    status: "Up".to_owned(),
                    state: "running".to_owned(),
                    ports: Vec::new(),
                    labels: vec![
                        "com.docker.compose.project=webapp".to_owned(),
                        "com.docker.compose.service=api".to_owned(),
                    ],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: None,
                    health: None,
                },
                Container {
                    id: "a2".to_owned(),
                    name: "db".to_owned(),
                    image: "postgres:16".to_owned(),
                    status: "Exited".to_owned(),
                    state: "exited".to_owned(),
                    ports: Vec::new(),
                    labels: vec![
                        "com.docker.compose.project=webapp".to_owned(),
                        "com.docker.compose.service=db".to_owned(),
                    ],
                    created_at: None,
                    started_at: None,
                    finished_at: None,
                    restart_count: None,
                    health: None,
                },
            ],
            ..Default::default()
        };
        let parsed = parser::parse("compose ls").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["status"],
            JsonValue::String("partial".to_owned())
        );
    }

    #[test]
    fn executes_compose_images() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp images").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert!(result.rows.len() > 0);
    }

    #[test]
    fn executes_compose_stats() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp stats | select name, service")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert!(row.fields.contains_key("name"));
            assert!(row.fields.contains_key("service"));
        }
    }

    #[test]
    fn executes_compose_ps() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp ps | select name, service")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert!(row.fields.contains_key("name"));
            assert!(row.fields.contains_key("service"));
        }
    }

    #[test]
    fn executes_compose_ps_with_filter() {
        let client = compose_mock_client();
        let parsed =
            parser::parse("compose myapp ps | where service = \"api-service\"")
                .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api".to_owned())
        );
    }

    #[test]
    fn executes_compose_logs() {
        let mut client = compose_mock_client();
        client.logs.insert(
            "abc".to_owned(),
            vec!["line1".to_owned(), "line2".to_owned()],
        );
        let parsed =
            parser::parse("compose myapp logs api-service tail 10").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            result.rows[0].fields["message"],
            JsonValue::String("line1".to_owned())
        );
        assert_eq!(
            result.rows[0].fields["service"],
            JsonValue::String("api-service".to_owned())
        );
        assert_eq!(
            result.rows[0].fields["line"],
            JsonValue::Number(1.into())
        );
    }

    #[test]
    fn executes_compose_logs_with_pipeline() {
        let mut client = compose_mock_client();
        client.logs.insert(
            "abc".to_owned(),
            vec![
                "info start".to_owned(),
                "error: something failed".to_owned(),
                "info end".to_owned(),
            ],
        );
        let parsed = parser::parse(
            "compose myapp logs api-service tail 10 | where message contains \"error\"",
        )
        .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert!(result.rows[0]
            .fields["message"]
            .as_str()
            .unwrap()
            .contains("error"));
    }

    #[test]
    fn executes_compose_port() {
        let client = compose_mock_client();
        let parsed =
            parser::parse("compose myapp port api-service 8080").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["service"],
            JsonValue::String("api-service".to_owned())
        );
    }

    #[test]
    fn executes_compose_config_services() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp config services")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert!(row.fields.contains_key("name"));
            assert!(row.fields.contains_key("image"));
            assert!(row.fields.contains_key("state"));
        }
    }

    #[test]
    fn executes_compose_config_networks() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp config networks")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 2);
        for row in &result.rows {
            assert!(row.fields.contains_key("name"));
            assert!(row.fields.contains_key("driver"));
        }
    }

    #[test]
    fn executes_compose_config_volumes() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp config volumes")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert!(result.rows[0].fields.contains_key("name"));
        assert!(result.rows[0].fields.contains_key("driver"));
    }

    #[test]
    fn executes_compose_config_all() {
        let client = compose_mock_client();
        let parsed =
            parser::parse("compose myapp config").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert!(result.rows.len() >= 5);
    }

    #[test]
    fn executes_compose_config_with_pipeline() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp config services | where image = \"api:latest\"")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].fields["name"],
            JsonValue::String("api-service".to_owned())
        );
    }

    #[test]
    fn parses_compose_ls() {
        let parsed = parser::parse("compose ls").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Projects);
                assert!(q.project.is_empty());
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_images() {
        let parsed = parser::parse("compose myapp images").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Images);
                assert_eq!(q.project, "myapp");
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_stats() {
        let parsed = parser::parse("compose myapp stats").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Stats);
                assert_eq!(q.project, "myapp");
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_ps() {
        let parsed = parser::parse("compose myapp ps").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Ps);
                assert_eq!(q.project, "myapp");
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_events() {
        let parsed = parser::parse("compose myapp events").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Events);
                assert_eq!(q.project, "myapp");
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_logs() {
        let parsed =
            parser::parse("compose myapp logs api-service tail 50").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Logs);
                assert_eq!(q.project, "myapp");
                assert_eq!(q.service.as_deref(), Some("api-service"));
                assert_eq!(q.tail, Some(50));
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_logs_no_tail() {
        let parsed =
            parser::parse("compose myapp logs api-service").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Logs);
                assert_eq!(q.service.as_deref(), Some("api-service"));
                assert_eq!(q.tail, None);
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_port() {
        let parsed =
            parser::parse("compose myapp port api-service 8080").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Port);
                assert_eq!(q.project, "myapp");
                assert_eq!(q.service.as_deref(), Some("api-service"));
                assert_eq!(q.port_number, Some(8080));
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_config() {
        let parsed = parser::parse("compose myapp config services")
            .expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Config);
                assert_eq!(q.project, "myapp");
                assert_eq!(q.config_target, Some(ConfigTarget::Services));
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_config_networks() {
        let parsed = parser::parse("compose myapp config networks")
            .expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Config);
                assert_eq!(q.config_target, Some(ConfigTarget::Networks));
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_config_volumes() {
        let parsed = parser::parse("compose myapp config volumes")
            .expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Config);
                assert_eq!(q.config_target, Some(ConfigTarget::Volumes));
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_compose_config_all() {
        let parsed = parser::parse("compose myapp config").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Config);
                assert_eq!(q.config_target, Some(ConfigTarget::All));
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_observe_compose_images() {
        let parsed = parser::parse("observe compose myapp images").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Images);
                assert_eq!(q.project, "myapp");
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn parses_observe_compose_stats() {
        let parsed = parser::parse("observe compose myapp stats").expect("query should parse");
        match parsed.query {
            Query::Compose(q) => {
                assert_eq!(q.target, ComposeTarget::Stats);
            }
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn semantic_compose_projects_valid() {
        let parsed = parser::parse("compose ls | where containers > 1").expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_projects_select_valid() {
        let parsed =
            parser::parse("compose ls | select project, containers").expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_projects_invalid_field() {
        let parsed =
            parser::parse("compose ls | where cpu > 80%").expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(matches!(res, Err(EvalError::UnsupportedField { .. })));
    }

    #[test]
    fn semantic_compose_images_valid() {
        let parsed = parser::parse("compose myapp images | where service = \"api\"")
            .expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_images_select() {
        let parsed = parser::parse("compose myapp images | select name, tag, size")
            .expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_stats_valid() {
        let parsed = parser::parse("compose myapp stats | where cpu > 80%")
            .expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_logs_valid() {
        let parsed = parser::parse("compose myapp logs api-service | where message contains \"error\"")
            .expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_logs_select() {
        let parsed = parser::parse("compose myapp logs api-service | select line, message")
            .expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_config_valid() {
        let parsed = parser::parse("compose myapp config | select name, image")
            .expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn semantic_compose_port_valid() {
        let parsed = parser::parse("compose myapp port web 80 | select service")
            .expect("query should parse");
        let res = crate::semantic::validate_semantics(&parsed.query);
        assert!(res.is_ok());
    }

    #[test]
    fn compose_events_returns_error() {
        let client = compose_mock_client();
        let parsed =
            parser::parse("compose myapp events").expect("query should parse");
        let result = execute(&parsed.query, &client);
        assert!(result.is_err());
    }

    #[test]
    fn compose_ls_empty_projects() {
        let client = MockDockerClient::default();
        let parsed = parser::parse("compose ls").expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 0);
    }

    #[test]
    fn compose_port_no_matching_container() {
        let client = compose_mock_client();
        let parsed = parser::parse("compose myapp port nonexistent 9999")
            .expect("query should parse");
        let result = execute(&parsed.query, &client).expect("query should execute");
        assert_eq!(result.rows.len(), 0);
    }
}
