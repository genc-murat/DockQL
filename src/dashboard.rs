//! TUI dashboard and top-like container monitor.
//!
//! Uses [`ratatui`] and [`crossterm`] for terminal UI. Provides two modes:
//!
//! - **`dol top`** — live-updating container list with CPU/memory gauges
//! - **`dol dashboard`** — multi-panel view with container list, stats, and
//!   event stream
//!
//! Both modes listen to Docker events for real-time updates and fall back
//! to periodic polling when the events stream is unavailable.
//!
//! # Example
//!
//! ```ignore
//! dashboard::run_top(&config).await?;
//! dashboard::run_dashboard(&config).await?;
//! ```

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::{Frame, Terminal};

use crate::{
    config::DolConfig,
    docker::{BollardDockerClient, Container, DockerClient, DockerEvent, MetricSample},
    metrics::{BollardMetricsCollector, MetricsCollector},
};

/// Fallback interval for full container refresh when events listener fails.
const CONTAINER_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Interval for lightweight metrics-only refresh.
const METRICS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Poll duration for non-blocking keyboard event reading.
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(200);

const HELP_TEXT: &str = "\
 DOL Keyboard Help

 [q/Esc]      Quit
 [↑/↓] or j/k Navigate rows
 [s]          Cycle sort column
 [d]          Toggle sort direction
 [r]          Force refresh
 [/]          Filter containers by name  (top only)
 [h]          Toggle this help overlay
 [Tab]        Switch panel focus         (dashboard only)
 [c]          Clear events               (dashboard only)
";

fn state_color(state: &str) -> Color {
    match state {
        "running" => Color::Green,
        "exited" | "dead" => Color::Red,
        "paused" => Color::Yellow,
        "restarting" => Color::Cyan,
        "created" => Color::Blue,
        _ => Color::White,
    }
}

fn gauge_color(ratio: f64) -> Color {
    if ratio > 0.80 {
        Color::Red
    } else if ratio > 0.50 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn event_action_color(action: &str) -> Color {
    match action {
        "start" | "restart" | "unpause" => Color::Green,
        "die" | "kill" | "oom" | "destroy" => Color::Red,
        "stop" | "pause" => Color::Yellow,
        "create" | "pull" => Color::Cyan,
        _ => Color::White,
    }
}

async fn collect_metrics_map(collector: &impl MetricsCollector) -> HashMap<String, MetricSample> {
    collector
        .collect()
        .await
        .ok()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.container_name.clone(), s))
        .collect()
}

/// Check if a Docker event action signals a container state change that
/// would affect `docker ps -a` output. Only these actions trigger a full
/// container-list refresh.
fn is_container_state_change(action: &str) -> bool {
    matches!(
        action,
        "create"
            | "start"
            | "die"
            | "stop"
            | "destroy"
            | "kill"
            | "restart"
            | "pause"
            | "unpause"
            | "update"
    )
}

/// Format a Docker event's time (Unix seconds or nanoseconds as string)
/// into HH:MM:SS display format.
#[allow(clippy::option_if_let_else)]
fn format_event_time(event: &DockerEvent) -> String {
    let time_raw = &event.time;
    if let Ok(secs) = time_raw.parse::<u64>() {
        // timeNano values are ~19 digits (nanoseconds), time values are ~10 digits (seconds)
        let secs = if time_raw.len() >= 16 {
            secs / 1_000_000_000
        } else {
            secs
        };
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("{h:02}:{m:02}:{s:02}")        } else if time_raw.len() >= 19 {
            // ISO timestamp: "2026-05-31T02:00:00.000000000Z"
            time_raw.get(11..19).map_or_else(|| "??:??:??".to_owned(), std::borrow::ToOwned::to_owned)
        } else {
            "??:??:??".to_owned()
        }
}

/// Spawn a background tokio task that listens to Docker events via the
/// bollard API and sends parsed `ParsedEvent` values through a channel.
fn spawn_event_listener(docker: std::sync::Arc<BollardDockerClient>, events_timeout: Duration) -> mpsc::Receiver<ParsedEvent> {
    let (tx, rx) = mpsc::channel();

    tokio::spawn(async move {
    let Ok(stream) = docker.events_stream(None, None).await else { return; };

        let mut stream = stream;
        loop {
            let next = tokio::time::timeout(events_timeout, stream.next()).await;
            let Ok(Some(Ok(event))) = next else { break };
            let actor_id = if event.actor_id.len() > 12 {
                event.actor_id[..12].to_owned()
            } else {
                event.actor_id.clone()
            };
            let parsed = ParsedEvent {
                time: format_event_time(&event),
                action: event.action,
                actor_id,
                container_name: event.container.unwrap_or_default(),
            };
            if tx.send(parsed).is_err() {
                break; // receiver dropped → main loop ended
            }
        }
    });

    rx
}

// ── Shared helpers ─────────────────────────────────────────

async fn refresh_all(
    docker: &BollardDockerClient,
    metrics: &BollardMetricsCollector<BollardDockerClient>,
    containers: &mut Vec<Container>,
    metrics_map: &mut HashMap<String, MetricSample>,
    last_refresh: &mut String,
) -> Result<(), anyhow::Error> {
    if let Ok(c) = docker.list_containers().await {
        *containers = c;
    }
    *metrics_map = collect_metrics_map(metrics).await;
    update_timestamp(last_refresh);
    Ok(())
}

async fn refresh_metrics_only(
    metrics: &impl MetricsCollector,
    metrics_map: &mut HashMap<String, MetricSample>,
    last_refresh: &mut String,
) {
    *metrics_map = collect_metrics_map(metrics).await;
    update_timestamp(last_refresh);
}

fn update_timestamp(last_refresh: &mut String) {
    use std::fmt::Write;
    let now = chrono::Local::now();
    let _ = write!(last_refresh, "{}", now.format("%H:%M:%S"));
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let help_h = HELP_TEXT.lines().count() as u16 + 4;
    let help_w = 44u16;
    let x = area.x + (area.width.saturating_sub(help_w)) / 2;
    let y = area.y + (area.height.saturating_sub(help_h)) / 2;

    let popup_area = Rect::new(x, y, help_w, help_h);
    f.render_widget(Clear, popup_area);

    let help_para = Paragraph::new(HELP_TEXT)
        .style(Style::default().fg(Color::White).bg(Color::Black))
        .block(
            Block::default()
                .title(" Help ")
                .title_alignment(ratatui::layout::Alignment::Center)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    f.render_widget(help_para, popup_area);
}

fn format_mem(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1}G", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.0}M", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0}K", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes}B")
    }
}

fn gauge_bar(ratio: f64, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    let filled = (ratio * f64::from(width)).round() as usize;
    let filled = filled.min(width as usize);
    let empty = width.saturating_sub(filled as u16) as usize;
    "█".repeat(filled) + "░".repeat(empty).as_str()
}

// ── dol top ────────────────────────────────────────────────────

pub async fn run_top(config: &DolConfig) -> anyhow::Result<()> {
    let docker_arc = std::sync::Arc::new(BollardDockerClient::connect_with_config(config)?);
    let docker = docker_arc.as_ref();
    let metrics_collector = BollardMetricsCollector::with_config(std::sync::Arc::clone(&docker_arc), config);

    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut sort_column: usize = 0;
    let mut sort_desc: bool = false;
    let mut containers: Vec<Container> = Vec::new();
    let mut metrics_map: HashMap<String, MetricSample> = HashMap::new();
    let mut should_quit = false;
    let mut show_help = false;
    let mut filter_text = String::new();
    let mut in_filter_mode = false;
    let mut last_refresh = String::new();

    let events_timeout = Duration::from_secs(config.events_timeout.unwrap_or(30));

    // Spawn a background Docker events listener via bollard API
    let event_rx = spawn_event_listener(std::sync::Arc::clone(&docker_arc), events_timeout);

    // Initial load
    let _ = refresh_all(
        docker,
        &metrics_collector,
        &mut containers,
        &mut metrics_map,
        &mut last_refresh,
    ).await;
    let mut last_metrics_refresh = std::time::Instant::now();
    let mut last_container_refresh = std::time::Instant::now();

    while !should_quit {
        terminal.draw(|f| {
            draw_top(
                f,
                &containers,
                &metrics_map,
                &mut table_state,
                sort_column,
                sort_desc,
                &last_refresh,
                show_help,
                &filter_text,
                in_filter_mode,
            );
        })?;

        // ── Event-driven refresh (non-blocking) ──
        // Drain all queued Docker events and refresh containers if a
        // state-changing event occurred. If the events listener fails
        // (e.g., docker not available), falls back to a periodic full
        // refresh every 30 seconds.
        let mut container_changed = false;
        while let Ok(event) = event_rx.try_recv() {
            if is_container_state_change(&event.action) {
                container_changed = true;
            }
        }

        if container_changed {
            // Full refresh: containers + metrics (triggered by Docker event)
            let _ = refresh_all(
                docker,
                &metrics_collector,
                &mut containers,
                &mut metrics_map,
                &mut last_refresh,
            ).await;
            last_metrics_refresh = std::time::Instant::now();
            last_container_refresh = std::time::Instant::now();
        } else if last_container_refresh.elapsed() >= CONTAINER_REFRESH_INTERVAL {
            // Fallback full refresh (in case the events listener failed)
            let _ = refresh_all(
                docker,
                &metrics_collector,
                &mut containers,
                &mut metrics_map,
                &mut last_refresh,
            ).await;
            last_metrics_refresh = std::time::Instant::now();
            last_container_refresh = std::time::Instant::now();
        } else if last_metrics_refresh.elapsed() >= METRICS_REFRESH_INTERVAL {
            // Periodic metrics-only refresh (lighter weight — no docker ps)
            refresh_metrics_only(&metrics_collector, &mut metrics_map, &mut last_refresh).await;
            last_metrics_refresh = std::time::Instant::now();
        }

        // ── Key events ──
        if event::poll(EVENT_POLL_INTERVAL)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc if !in_filter_mode => should_quit = true,
                KeyCode::Char('h') if !in_filter_mode => show_help = !show_help,
                KeyCode::Char('r') if !in_filter_mode => {
                    let _ = refresh_all(
                        docker,
                        &metrics_collector,
                        &mut containers,
                        &mut metrics_map,
                        &mut last_refresh,
                    ).await;
                    last_metrics_refresh = std::time::Instant::now();
                    last_container_refresh = std::time::Instant::now();
                }
                KeyCode::Down | KeyCode::Char('j') if !in_filter_mode => {
                    let i = table_state.selected().unwrap_or(0);
                    let n = containers.len().saturating_sub(1);
                    table_state.select(Some(i.saturating_add(1).min(n)));
                }
                KeyCode::Up | KeyCode::Char('k') if !in_filter_mode => {
                    let i = table_state.selected().unwrap_or(0);
                    table_state.select(Some(i.saturating_sub(1)));
                }
                KeyCode::Char('s') if !in_filter_mode => {
                    sort_column = (sort_column + 1) % 4;
                }
                KeyCode::Char('d') if !in_filter_mode => {
                    sort_desc = !sort_desc;
                }
                KeyCode::Char('/') if !in_filter_mode => {
                    in_filter_mode = true;
                    filter_text.clear();
                }
                KeyCode::Char(c) if in_filter_mode => {
                    filter_text.push(c);
                }
                KeyCode::Backspace if in_filter_mode => {
                    filter_text.pop();
                }
                KeyCode::Enter | KeyCode::Char(' ') if in_filter_mode => {
                    in_filter_mode = false;
                }
                KeyCode::Esc if in_filter_mode => {
                    in_filter_mode = false;
                    filter_text.clear();
                }
                _ => {}
            }
        }
    }

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_top(
    f: &mut Frame,
    containers: &[Container],
    metrics_map: &HashMap<String, MetricSample>,
    table_state: &mut TableState,
    sort_col: usize,
    sort_desc: bool,
    last_refresh: &str,
    show_help: bool,
    filter_text: &str,
    in_filter_mode: bool,
) {
    let area = f.area();

    let filtered: Vec<&Container> = if filter_text.is_empty() {
        containers.iter().collect()
    } else {
        containers
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&filter_text.to_lowercase()))
            .collect()
    };

    let mut sorted: Vec<&Container> = filtered.clone();
    sorted.sort_by(|a, b| {
        let cmp = match sort_col {
            0 => a.name.cmp(&b.name),
            1 => a.image.cmp(&b.image),
            2 => a.state.cmp(&b.state),
            _ => a.status.cmp(&b.status),
        };
        if sort_desc { cmp.reverse() } else { cmp }
    });

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    draw_summary_bar(f, chunks[0], containers, sort_col, sort_desc, last_refresh);
    draw_container_table_top(f, chunks[1], &sorted, metrics_map, table_state);
    draw_status_bar(
        f,
        chunks[2],
        filtered.len(),
        containers.len(),
        in_filter_mode,
        filter_text,
    );

    if show_help {
        draw_help_overlay(f, area);
    }
}

fn draw_summary_bar(
    f: &mut Frame,
    area: Rect,
    containers: &[Container],
    sort_col: usize,
    sort_desc: bool,
    last_refresh: &str,
) {
    let running = containers.iter().filter(|c| c.state == "running").count();
    let exited = containers
        .iter()
        .filter(|c| c.state == "exited" || c.state == "dead")
        .count();
    let paused = containers.iter().filter(|c| c.state == "paused").count();
    let other = containers.len().saturating_sub(running + exited + paused);

    let sort_names = ["NAME", "IMAGE", "STATE", "STATUS"];
    let arrow = if sort_desc { "▼" } else { "▲" };
    let sort_label = format!("{arrow} {}", sort_names[sort_col]);

    let mut spans = vec![
        Span::styled(
            format!(" {} ", containers.len()),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("total  │ "),
        Span::styled("●", Style::default().fg(Color::Green)),
        Span::raw(format!(" {running}  ")),
        Span::styled("●", Style::default().fg(Color::Red)),
        Span::raw(format!(" {exited}  ")),
        Span::styled("●", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" {paused}  ")),
    ];
    if other > 0 {
        spans.push(Span::styled("●", Style::default().fg(Color::Blue)));
        spans.push(Span::raw(format!(" {other}  │  ")));
    } else {
        spans.push(Span::raw(" │  "));
    }
    spans.push(Span::styled(sort_label, Style::default().fg(Color::Cyan)));
    spans.push(Span::raw("  │  "));
    spans.push(Span::raw(format!("refresh: {last_refresh}")));

    let block = Block::default().style(Style::default().on_dark_gray());
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn draw_container_table_top(
    f: &mut Frame,
    area: Rect,
    sorted: &[&Container],
    metrics_map: &HashMap<String, MetricSample>,
    table_state: &mut TableState,
) {
    let gauge_w = 12u16.min(area.width.saturating_sub(80) / 2);

    let rows: Vec<Row> = sorted
        .iter()
        .map(|c| {
            let s_style = Style::default().fg(state_color(&c.state));

            let metric = metrics_map.get(&c.name);
            let cpu_pct = metric.and_then(|m| m.cpu_percent).unwrap_or(0.0);
            let mem_used = metric.and_then(|m| m.memory_usage_bytes).unwrap_or(0);
            let mem_limit = metric.and_then(|m| m.memory_limit_bytes).unwrap_or(1);
            let mem_pct = if mem_limit > 0 {
                (mem_used as f64 / mem_limit as f64) * 100.0
            } else {
                0.0
            };
            let rc = c.restart_count.unwrap_or(0);

            let cpu_bar = gauge_bar(cpu_pct / 100.0, gauge_w);
            let mem_bar = gauge_bar(mem_pct / 100.0, gauge_w);
            let mem_str = format_mem(mem_used);

            Row::new(vec![
                Cell::from(Span::styled(
                    c.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(c.image.clone(), Style::default())),
                Cell::from(Span::styled(
                    cpu_bar,
                    Style::default().fg(gauge_color(cpu_pct / 100.0)),
                )),
                Cell::from(Span::styled(
                    mem_bar,
                    Style::default().fg(gauge_color(mem_pct / 100.0)),
                )),
                Cell::from(Span::styled(
                    mem_str + " " + &format!("{mem_pct:5.1}%"),
                    Style::default().fg(gauge_color(mem_pct / 100.0)),
                )),
                Cell::from(Span::styled(c.state.clone(), s_style)),
                Cell::from(Span::styled(c.status.clone(), Style::default())),
                Cell::from(Span::styled(
                    format!("{rc}"),
                    if rc > 3 {
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                )),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(22),
            Constraint::Length(22),
            Constraint::Length(gauge_w + 2),
            Constraint::Length(gauge_w + 2),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(4),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from(Span::styled(
                "NAME",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "IMAGE",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "CPU",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "MEM",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "MEMORY",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "STATE",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "STATUS",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "RST",
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ])
        .style(Style::default().fg(Color::Cyan)),
    )
    .block(
        Block::default()
            .title(format!(" Containers ({}) ", sorted.len()))
            .borders(Borders::ALL),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ");

    f.render_stateful_widget(table, area, table_state);
}

fn draw_status_bar(
    f: &mut Frame,
    area: Rect,
    shown: usize,
    total: usize,
    in_filter: bool,
    filter_text: &str,
) {
    let text = if in_filter {
        Line::from(Span::styled(
            format!("/{filter_text}▌"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
    } else if shown < total {
        Line::from(Span::raw(format!(
            "[↑↓] nav  [s] sort  [d] desc  [/] filter  [r] refresh  [h] help  [q] quit  (showing {shown}/{total})",
        )))
    } else {
        Line::from(Span::raw(
            "[↑↓] nav  [s] sort  [d] desc  [/] filter  [r] refresh  [h] help  [q] quit",
        ))
    };

    let block = Block::default().style(Style::default().on_dark_gray());
    f.render_widget(Paragraph::new(text).block(block), area);
}

// ── dol dashboard ──────────────────────────────────────────────

pub async fn run_dashboard(config: &DolConfig) -> anyhow::Result<()> {
    let docker_arc = std::sync::Arc::new(BollardDockerClient::connect_with_config(config)?);
    let docker = docker_arc.as_ref();
    let metrics_collector = BollardMetricsCollector::with_config(std::sync::Arc::clone(&docker_arc), config);

    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let mut containers: Vec<Container> = Vec::new();
    let mut metrics_map: HashMap<String, MetricSample> = HashMap::new();
    let mut events: Vec<ParsedEvent> = Vec::new();
    let mut should_quit = false;
    let mut selected_panel: usize = 0;
    let mut show_help = false;
    let mut last_refresh = String::new();

    let events_timeout = Duration::from_secs(config.events_timeout.unwrap_or(30));

    // Spawn a background Docker events listener via bollard API
    let event_rx = spawn_event_listener(std::sync::Arc::clone(&docker_arc), events_timeout);

    // Initial load: containers + recent events
    let _ = refresh_all(
        docker,
        &metrics_collector,
        &mut containers,
        &mut metrics_map,
        &mut last_refresh,
    ).await;
    refresh_events(docker, &mut events, events_timeout).await;
    let mut last_metrics_refresh = std::time::Instant::now();
    let mut last_container_refresh = std::time::Instant::now();

    while !should_quit {
        terminal.draw(|f| {
            draw_dashboard(
                f,
                &containers,
                &metrics_map,
                &events,
                selected_panel,
                &last_refresh,
                show_help,
            );
        })?;

        // ── Event-driven refresh (non-blocking) ──
        // Drain all queued Docker events: add to the events panel and
        // trigger a container refresh if a state-changing event occurred.
        // Falls back to a periodic full refresh every 30 seconds if the
        // events listener fails.
        let mut container_changed = false;
        while let Ok(event) = event_rx.try_recv() {
            if is_container_state_change(&event.action) {
                container_changed = true;
            }
            events.push(event);
        }
        // Keep events buffer bounded
        if events.len() > 500 {
            events.drain(0..events.len() - 500);
        }

        if container_changed {
            // Full refresh: containers + metrics (triggered by Docker event)
            let _ = refresh_all(
                docker,
                &metrics_collector,
                &mut containers,
                &mut metrics_map,
                &mut last_refresh,
            ).await;
            last_metrics_refresh = std::time::Instant::now();
            last_container_refresh = std::time::Instant::now();
        } else if last_container_refresh.elapsed() >= CONTAINER_REFRESH_INTERVAL {
            // Fallback full refresh (in case the events listener failed)
            let _ = refresh_all(
                docker,
                &metrics_collector,
                &mut containers,
                &mut metrics_map,
                &mut last_refresh,
            ).await;
            last_metrics_refresh = std::time::Instant::now();
            last_container_refresh = std::time::Instant::now();
        } else if last_metrics_refresh.elapsed() >= METRICS_REFRESH_INTERVAL {
            // Periodic metrics-only refresh (lighter weight)
            refresh_metrics_only(&metrics_collector, &mut metrics_map, &mut last_refresh).await;
            last_metrics_refresh = std::time::Instant::now();
        }

        // ── Key events ──
        if event::poll(EVENT_POLL_INTERVAL)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc if !show_help => should_quit = true,
                KeyCode::Char('h') => show_help = !show_help,
                KeyCode::Char('r') => {
                    let _ = refresh_all(
                        docker,
                        &metrics_collector,
                        &mut containers,
                        &mut metrics_map,
                        &mut last_refresh,
                    ).await;
                    refresh_events(docker, &mut events, events_timeout).await;
                    last_metrics_refresh = std::time::Instant::now();
                    last_container_refresh = std::time::Instant::now();
                }
                KeyCode::Tab => selected_panel = (selected_panel + 1) % 2,
                KeyCode::Char('c') => events.clear(),
                _ => {}
            }
        }
    }

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

struct ParsedEvent {
    time: String,
    action: String,
    actor_id: String,
    container_name: String,
}

/// Fetch recent Docker events (last ~5 seconds) via the bollard events API
/// and append new unique events to the events buffer.
async fn refresh_events(docker: &BollardDockerClient, events: &mut Vec<ParsedEvent>, events_timeout: Duration) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let since = now.saturating_sub(5);

    let Ok(stream) = docker.events_stream(Some(&since.to_string()), Some(&now.to_string())).await else { return };

    let mut stream = stream;
    loop {
        let next = tokio::time::timeout(events_timeout, stream.next()).await;
        let Ok(Some(Ok(event))) = next else { break };
        let actor_id = if event.actor_id.len() > 12 {
            event.actor_id[..12].to_owned()
        } else {
            event.actor_id.clone()
        };
        let pe = ParsedEvent {
            time: format_event_time(&event),
            action: event.action,
            actor_id,
            container_name: event.container.unwrap_or_default(),
        };
        if !events.iter().any(|e| {
            e.actor_id == pe.actor_id && e.action == pe.action && e.time == pe.time
        }) {
            events.push(pe);
        }
    }
    if events.len() > 200 {
        events.drain(0..events.len() - 200);
    }
}

fn draw_dashboard(
    f: &mut Frame,
    containers: &[Container],
    metrics_map: &HashMap<String, MetricSample>,
    events: &[ParsedEvent],
    selected: usize,
    last_refresh: &str,
    show_help: bool,
) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(10),
            Constraint::Length(1),
        ])
        .split(area);

    draw_dash_summary(f, chunks[0], containers, last_refresh);

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(3, 5), Constraint::Ratio(2, 5)])
        .split(chunks[1]);

    draw_dash_container_panel(f, main_chunks[0], containers, metrics_map, selected == 0);
    draw_dash_stats_panel(f, main_chunks[1], containers, selected == 1);
    draw_dash_events_panel(f, chunks[2], events);

    let status_text = Line::from(Span::raw(
        "[Tab] panels  [c] clear events  [r] refresh  [h] help  [q] quit",
    ));
    f.render_widget(
        Paragraph::new(status_text).block(Block::default().style(Style::default().on_dark_gray())),
        chunks[3],
    );

    if show_help {
        draw_help_overlay(f, area);
    }
}

fn draw_dash_summary(f: &mut Frame, area: Rect, containers: &[Container], last_refresh: &str) {
    let running = containers.iter().filter(|c| c.state == "running").count();
    let exited = containers
        .iter()
        .filter(|c| c.state == "exited" || c.state == "dead")
        .count();
    let paused = containers.iter().filter(|c| c.state == "paused").count();
    let other = containers.len().saturating_sub(running + exited + paused);

    let mut spans = vec![
        Span::styled(
            " DOL Dashboard ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  │  "),
        Span::raw(format!("{} total", containers.len())),
        Span::raw("  │  "),
        Span::styled("●", Style::default().fg(Color::Green)),
        Span::raw(format!(" {running}  ")),
        Span::styled("●", Style::default().fg(Color::Red)),
        Span::raw(format!(" {exited}  ")),
        Span::styled("●", Style::default().fg(Color::Yellow)),
        Span::raw(format!(" {paused}  ")),
    ];
    if other > 0 {
        spans.push(Span::styled("●", Style::default().fg(Color::Blue)));
        spans.push(Span::raw(format!(" {other}")));
    }
    spans.push(Span::raw("  │  "));
    spans.push(Span::raw(format!("refresh: {last_refresh}")));

    let block = Block::default().style(Style::default().on_dark_gray());
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn draw_dash_container_panel(
    f: &mut Frame,
    area: Rect,
    containers: &[Container],
    metrics_map: &HashMap<String, MetricSample>,
    focused: bool,
) {
    let rows: Vec<Row> = containers
        .iter()
        .take(area.height.saturating_sub(3) as usize)
        .map(|c| {
            let s_style = Style::default().fg(state_color(&c.state));
            let metric = metrics_map.get(&c.name);
            let cpu_pct = metric.and_then(|m| m.cpu_percent).unwrap_or(0.0);
            let mem_used = metric.and_then(|m| m.memory_usage_bytes).unwrap_or(0);
            let mem_limit = metric.and_then(|m| m.memory_limit_bytes).unwrap_or(1);
            let mem_pct = if mem_limit > 0 {
                (mem_used as f64 / mem_limit as f64) * 100.0
            } else {
                0.0
            };
            let mem_str = format_mem(mem_used);

            Row::new(vec![
                Cell::from(Span::styled(
                    c.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(
                    format!("{cpu_pct:5.1}%"),
                    Style::default().fg(gauge_color(cpu_pct / 100.0)),
                )),
                Cell::from(Span::styled(
                    mem_str + " " + &format!("({mem_pct:.0}%)"),
                    Style::default().fg(gauge_color(mem_pct / 100.0)),
                )),
                Cell::from(Span::styled(c.state.clone(), s_style)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(20),
            Constraint::Length(8),
            Constraint::Length(14),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from(Span::styled(
                "NAME",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "CPU",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "MEMORY",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(
                "STATE",
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ])
        .style(Style::default().fg(Color::Cyan)),
    )
    .block(
        Block::default()
            .title(format!(" Containers ({}) ", containers.len()))
            .borders(Borders::ALL)
            .border_style(if focused {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            }),
    );

    f.render_widget(table, area);
}

fn draw_dash_stats_panel(f: &mut Frame, area: Rect, containers: &[Container], focused: bool) {
    let running = containers.iter().filter(|c| c.state == "running").count();
    let exited = containers
        .iter()
        .filter(|c| c.state == "exited" || c.state == "dead")
        .count();
    let paused = containers.iter().filter(|c| c.state == "paused").count();
    let total = containers.len();
    let other = total.saturating_sub(running + exited + paused);

    let mut image_counts: HashMap<&str, usize> = HashMap::new();
    for c in containers {
        *image_counts.entry(&c.image).or_insert(0) += 1;
    }
    let mut image_vec: Vec<(&str, usize)> = image_counts.into_iter().collect();
    image_vec.sort_by_key(|a| std::cmp::Reverse(a.1));

    let max_w = 14usize;
    let mut lines = vec![
        Line::from(vec![Span::styled(
            " State Distribution ",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(Span::raw("")),
    ];

    for (label, count, color) in [
        ("running", running, Color::Green),
        ("exited", exited, Color::Red),
        ("paused", paused, Color::Yellow),
        ("other", other, Color::Blue),
    ] {
        let bar_w = if total > 0 {
            (count * max_w)
                .checked_div(total)
                .map_or(0, |v| v.max(1).min(max_w))
        } else {
            0
        };
        let bar = "█".repeat(bar_w);
        let pct = if total > 0 {
            (count * 100).checked_div(total).unwrap_or(0)
        } else {
            0
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {label:8} "), Style::default().fg(color)),
            Span::styled(bar, Style::default().fg(color)),
            Span::raw(format!(" {count} ({pct}%)")),
        ]));
    }

    lines.push(Line::from(Span::raw("")));
    lines.push(Line::from(vec![Span::styled(
        " Top Images ",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    for (img, cnt) in image_vec.iter().take(6) {
        lines.push(Line::from(Span::raw(format!(" {img} x{cnt}"))));
    }

    let block = Block::default()
        .title(" Stats ")
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_dash_events_panel(f: &mut Frame, area: Rect, events: &[ParsedEvent]) {
    let lines: Vec<Line> = events
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|e| {
            let action_color = event_action_color(&e.action);
            Line::from(vec![
                Span::styled(
                    format!(" {} ", e.time),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(" {:8} ", e.action),
                    Style::default()
                        .fg(action_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(e.container_name.clone(), Style::default().fg(Color::White)),
            ])
        })
        .collect();

    let block = Block::default()
        .title(format!(" Recent Events ({}) ", events.len()))
        .borders(Borders::ALL);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::DockerEvent;

    // ── is_container_state_change ───────────────────────────────

    #[test]
    fn test_is_container_state_change_true_for_state_actions() {
        for action in &[
            "create", "start", "die", "stop", "destroy", "kill", "restart", "pause", "unpause",
            "update",
        ] {
            assert!(
                is_container_state_change(action),
                "{action} should be a state change"
            );
        }
    }

    #[test]
    fn test_is_container_state_change_false_for_other_actions() {
        for action in &[
            "exec_create",
            "exec_start",
            "attach",
            "export",
            "pull",
            "push",
            "tag",
            "commit",
        ] {
            assert!(
                !is_container_state_change(action),
                "{action} should NOT be a state change"
            );
        }
    }

    // ── format_event_time ───────────────────────────────────────

    #[test]
    fn test_format_event_time_unix_seconds() {
        // 10 digits = Unix seconds; 3600+1800+45 = 5445s = 01:30:45
        let event = DockerEvent {
            time: "5445".to_owned(),
            event_type: "container".to_owned(),
            action: "start".to_owned(),
            actor_id: "abc123".to_owned(),
            container: Some("test".to_owned()),
            image: Some("nginx".to_owned()),
            attributes: vec![],
        };
        assert_eq!(format_event_time(&event), "01:30:45");
    }

    #[test]
    fn test_format_event_time_nanoseconds() {
        // >=16 digits = nanoseconds: 5,400,000,000,000,000 ns = 5,400,000 sec = 12:00:00
        let event = DockerEvent {
            time: "5400000000000000".to_owned(),
            event_type: "container".to_owned(),
            action: "die".to_owned(),
            actor_id: "def456".to_owned(),
            container: Some("app".to_owned()),
            image: Some("python".to_owned()),
            attributes: vec![],
        };
        assert_eq!(format_event_time(&event), "12:00:00");
    }

    #[test]
    fn test_format_event_time_iso_timestamp() {
        // >=19 chars and non-numeric → extract HH:MM:SS from ISO
        let event = DockerEvent {
            time: "2026-05-31T14:30:00.000000000Z".to_owned(),
            event_type: "container".to_owned(),
            action: "stop".to_owned(),
            actor_id: "ghi789".to_owned(),
            container: Some("db".to_owned()),
            image: Some("postgres".to_owned()),
            attributes: vec![],
        };
        assert_eq!(format_event_time(&event), "14:30:00");
    }

    #[test]
    fn test_format_event_time_short_string() {
        // Short non-numeric string → ??
        let event = DockerEvent {
            time: "?".to_owned(),
            event_type: "container".to_owned(),
            action: "unknown".to_owned(),
            actor_id: "".to_owned(),
            container: None,
            image: None,
            attributes: vec![],
        };
        assert_eq!(format_event_time(&event), "??:??:??");
    }

    // ── format_mem ──────────────────────────────────────────────

    #[test]
    fn test_format_mem_bytes() {
        assert_eq!(format_mem(0), "0B");
        assert_eq!(format_mem(500), "500B");
        assert_eq!(format_mem(1500), "2K");
        assert_eq!(format_mem(1_500_000), "2M");
        assert_eq!(format_mem(1_500_000_000), "1.5G");
        assert_eq!(format_mem(999), "999B");
        assert_eq!(format_mem(1_000_000_000), "1.0G");
    }

    // ── gauge_bar ───────────────────────────────────────────────

    #[test]
    fn test_gauge_bar_full() {
        let bar = gauge_bar(1.0, 10);
        assert_eq!(bar.chars().count(), 10);
        assert!(bar.chars().all(|c| c == '█'));
    }

    #[test]
    fn test_gauge_bar_empty() {
        let bar = gauge_bar(0.0, 10);
        assert_eq!(bar.chars().count(), 10);
        assert!(bar.chars().all(|c| c == '░'));
    }

    #[test]
    fn test_gauge_bar_half() {
        let bar = gauge_bar(0.5, 10);
        assert_eq!(bar.chars().count(), 10);
        assert_eq!(bar.matches('█').count(), 5);
        assert_eq!(bar.matches('░').count(), 5);
    }

    #[test]
    fn test_gauge_bar_zero_width() {
        assert_eq!(gauge_bar(0.5, 0), "");
    }

    #[test]
    fn test_gauge_bar_clamps() {
        let bar = gauge_bar(2.0, 5);
        assert_eq!(bar.chars().count(), 5);
        assert!(bar.chars().all(|c| c == '█'));
    }

    // ── gauge_color ─────────────────────────────────────────────

    #[test]
    fn test_gauge_color_thresholds() {
        assert_eq!(gauge_color(0.0), Color::Green);
        assert_eq!(gauge_color(0.30), Color::Green);
        assert_eq!(gauge_color(0.50), Color::Green);
        assert_eq!(gauge_color(0.51), Color::Yellow);
        assert_eq!(gauge_color(0.75), Color::Yellow);
        assert_eq!(gauge_color(0.80), Color::Yellow);
        assert_eq!(gauge_color(0.81), Color::Red);
        assert_eq!(gauge_color(1.0), Color::Red);
    }

    // ── state_color ─────────────────────────────────────────────

    #[test]
    fn test_state_color_mapping() {
        assert_eq!(state_color("running"), Color::Green);
        assert_eq!(state_color("exited"), Color::Red);
        assert_eq!(state_color("dead"), Color::Red);
        assert_eq!(state_color("paused"), Color::Yellow);
        assert_eq!(state_color("restarting"), Color::Cyan);
        assert_eq!(state_color("created"), Color::Blue);
        assert_eq!(state_color("unknown"), Color::White);
    }

    // ── event_action_color ──────────────────────────────────────

    #[test]
    fn test_event_action_color_mapping() {
        assert_eq!(event_action_color("start"), Color::Green);
        assert_eq!(event_action_color("restart"), Color::Green);
        assert_eq!(event_action_color("unpause"), Color::Green);
        assert_eq!(event_action_color("die"), Color::Red);
        assert_eq!(event_action_color("kill"), Color::Red);
        assert_eq!(event_action_color("oom"), Color::Red);
        assert_eq!(event_action_color("destroy"), Color::Red);
        assert_eq!(event_action_color("stop"), Color::Yellow);
        assert_eq!(event_action_color("pause"), Color::Yellow);
        assert_eq!(event_action_color("create"), Color::Cyan);
        assert_eq!(event_action_color("pull"), Color::Cyan);
        assert_eq!(event_action_color("unknown_action"), Color::White);
    }
}
