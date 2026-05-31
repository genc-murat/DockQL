use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::{Frame, Terminal};

use crate::{
    config::DolConfig,
    docker::{Container, DockerCliClient, DockerClient},
};

/// Run `dol top` — live-updating container dashboard.
pub async fn run_top(_config: &DolConfig) -> anyhow::Result<()> {
    let docker = DockerCliClient::default();

    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut sort_column: usize = 0;
    let mut sort_desc: bool = false;
    let mut containers: Vec<Container> = Vec::new();
    let mut should_quit = false;

    let _ = refresh_containers(&docker, &mut containers);

    while !should_quit {
        terminal.draw(|f| {
            draw_top(f, &containers, &mut table_state, sort_column, sort_desc);
        })?;

        if event::poll(Duration::from_millis(2000))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => should_quit = true,
                        KeyCode::Char('r') => {
                            let _ = refresh_containers(&docker, &mut containers);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let i = table_state.selected().unwrap_or(0);
                            let next = i.saturating_add(1).min(containers.len().saturating_sub(1));
                            table_state.select(Some(next));
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let i = table_state.selected().unwrap_or(0);
                            table_state.select(Some(i.saturating_sub(1)));
                        }
                        KeyCode::Char('s') => {
                            sort_column = (sort_column + 1) % 4;
                        }
                        KeyCode::Char('d') => {
                            sort_desc = !sort_desc;
                        }
                        _ => {}
                    }
                }
            }
        } else {
            let _ = refresh_containers(&docker, &mut containers);
        }
    }

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn refresh_containers(
    docker: &DockerCliClient,
    containers: &mut Vec<Container>,
) -> Result<(), anyhow::Error> {
    match docker.list_containers() {
        Ok(c) => {
            *containers = c;
        }
        Err(e) => {
            eprintln!("Docker error: {e}");
        }
    }
    Ok(())
}

fn draw_top(
    f: &mut Frame,
    containers: &[Container],
    table_state: &mut TableState,
    sort_col: usize,
    sort_desc: bool,
) {
    let area = f.area();
    let title = format!(
        " DOL Top  |  {} containers  |  [↑↓] navigate  [s] sort  [d] desc  [r] refresh  [q] quit ",
        containers.len()
    );

    let mut sorted: Vec<&Container> = containers.iter().collect();
    sorted.sort_by(|a, b| {
        let cmp = match sort_col {
            0 => a.name.cmp(&b.name),
            1 => a.image.cmp(&b.image),
            2 => a.state.cmp(&b.state),
            _ => a.status.cmp(&b.status),
        };
        if sort_desc { cmp.reverse() } else { cmp }
    });

    let rows: Vec<Row> = sorted.iter().map(|c| {
        let state_style = match c.state.as_str() {
            "running" => Style::default().fg(Color::Green),
            "exited" | "dead" => Style::default().fg(Color::Red),
            "paused" => Style::default().fg(Color::Yellow),
            "restarting" => Style::default().fg(Color::Cyan),
            _ => Style::default().fg(Color::White),
        };

        Row::new(vec![
            Cell::from(Span::styled(c.name.as_str(), Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled(c.image.as_str(), Style::default())),
            Cell::from(Span::styled(c.state.as_str(), state_style)),
            Cell::from(Span::styled(c.status.as_str(), Style::default())),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(30),
            Constraint::Length(30),
            Constraint::Length(12),
            Constraint::Length(20),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from(Span::styled("NAME", Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled("IMAGE", Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled("STATE", Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled("STATUS", Style::default().add_modifier(Modifier::BOLD))),
        ])
        .style(Style::default().fg(Color::Cyan)),
    )
    .block(
        Block::default()
            .title(title)
            .title_alignment(ratatui::layout::Alignment::Center)
            .borders(Borders::ALL),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ");

    f.render_stateful_widget(table, area, table_state);
}

/// Run `dol dashboard` — multi-panel dashboard with containers, events.
pub async fn run_dashboard(_config: &DolConfig) -> anyhow::Result<()> {
    let docker = DockerCliClient::default();

    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let mut containers: Vec<Container> = Vec::new();
    let mut events: Vec<String> = Vec::new();
    let mut should_quit = false;
    let mut selected_panel: usize = 0;

    let _ = refresh_containers(&docker, &mut containers);

    while !should_quit {
        terminal.draw(|f| {
            draw_dashboard(f, &containers, &events, selected_panel);
        })?;

        if event::poll(Duration::from_millis(2000))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => should_quit = true,
                        KeyCode::Char('r') => {
                            let _ = refresh_containers(&docker, &mut containers);
                            let _ = refresh_events(&mut events);
                        }
                        KeyCode::Tab => {
                            selected_panel = (selected_panel + 1) % 2;
                        }
                        _ => {}
                    }
                }
            }
        } else {
            let _ = refresh_containers(&docker, &mut containers);
        }
    }

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn refresh_events(events: &mut Vec<String>) -> Result<(), anyhow::Error> {
    if let Ok(output) = std::process::Command::new("docker")
        .args(["events", "--until", "5s", "--format", "{{json .}}"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().rev().take(20) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                    let summary = format!(
                        "{} {} {}",
                        val.get("Type").and_then(|v| v.as_str()).unwrap_or("?"),
                        val.get("Action").and_then(|v| v.as_str()).unwrap_or("?"),
                        val.get("Actor")
                            .and_then(|a| a.get("ID"))
                            .and_then(|v| v.as_str())
                            .map(|s| &s[..12.min(s.len())])
                            .unwrap_or("")
                    );
                    if !events.contains(&summary) {
                        events.push(summary);
                    }
                }
            }
            if events.len() > 100 {
                events.drain(0..events.len() - 100);
            }
        }
    }
    Ok(())
}

fn draw_dashboard(
    f: &mut Frame,
    containers: &[Container],
    events: &[String],
    selected: usize,
) {
    let area = f.area();
    let chunks = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(3),
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(12),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(" DOL Dashboard ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("  [Tab] panels  [r] refresh  [q] quit"),
    ]))
    .style(Style::default().on_dark_gray());
    f.render_widget(title, chunks[0]);

    let rows: Vec<Row> = containers.iter().map(|c| {
        let state_style = match c.state.as_str() {
            "running" => Style::default().fg(Color::Green),
            "exited" | "dead" => Style::default().fg(Color::Red),
            "paused" => Style::default().fg(Color::Yellow),
            _ => Style::default().fg(Color::White),
        };
        Row::new(vec![
            Cell::from(Span::styled(c.name.as_str(), Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled(c.image.as_str(), Style::default())),
            Cell::from(Span::styled(c.state.as_str(), state_style)),
            Cell::from(Span::styled(c.status.as_str(), Style::default())),
        ])
    }).collect();

    let container_table = Table::new(
        rows,
        [
            Constraint::Length(28),
            Constraint::Length(28),
            Constraint::Length(10),
            Constraint::Length(18),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from(Span::styled("NAME", Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled("IMAGE", Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled("STATE", Style::default().add_modifier(Modifier::BOLD))),
            Cell::from(Span::styled("STATUS", Style::default().add_modifier(Modifier::BOLD))),
        ])
        .style(Style::default().fg(Color::Cyan)),
    )
    .block(
        Block::default()
            .title(format!(" Containers ({}) ", containers.len()))
            .borders(Borders::ALL)
            .border_style(if selected == 0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            }),
    );

    f.render_widget(container_table, chunks[1]);

    let event_lines: Vec<Line> = events.iter().rev().take(8).map(|e| {
        Line::from(Span::raw(e.as_str()))
    }).collect();

    let event_panel = Paragraph::new(event_lines)
        .block(
            Block::default()
                .title(" Recent Events ")
                .borders(Borders::ALL)
                .border_style(if selected == 1 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }),
        );
    f.render_widget(event_panel, chunks[2]);
}