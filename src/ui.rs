//! ratatui rendering + the main event loop.

use crate::app::{App, TabData, TabState};
use crate::keys;
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Tabs},
};
use std::io::Stdout;
use std::time::{Duration, Instant};

pub async fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        terminal.draw(|f| draw(f, app))?;
        app.drain();
        if app.cfg.refresh_interval_secs > 0
            && last_refresh.elapsed().as_secs() >= app.cfg.refresh_interval_secs
        {
            // Only `builds` tabs honor the timed refresh — `logs` tabs
            // keep their `aws logs tail --follow` child alive.
            if matches!(app.active().spec.kind, crate::app::TabKind::Builds) {
                app.refresh_active();
            }
            last_refresh = Instant::now();
        }
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    if let Some(action) = keys::handle(key, app) {
                        let quit = keys::apply(action, app).await;
                        if quit {
                            break;
                        }
                        last_refresh = Instant::now();
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);
    draw_tabs(f, chunks[0], app);
    match &app.active().data {
        TabData::Builds(_) => draw_builds_table(f, chunks[1], app.active()),
        TabData::Logs(_) => draw_logs(f, chunks[1], app.active()),
    }
    draw_status(f, chunks[2], app);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let suffix = match &t.data {
                TabData::Builds(b) => {
                    if b.loading && b.items.is_empty() {
                        " · loading".to_string()
                    } else if !b.items.is_empty() {
                        format!(" ({})", b.items.len())
                    } else {
                        String::new()
                    }
                }
                TabData::Logs(l) => {
                    if l.pane.is_some() {
                        " · tailing".to_string()
                    } else {
                        String::new()
                    }
                }
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, suffix))
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" aws "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_builds_table(f: &mut Frame, area: Rect, tab: &TabState) {
    let TabData::Builds(b) = &tab.data else {
        return;
    };
    if let Some(err) = &b.last_error {
        let p = Paragraph::new(format!("error: {err}\n\nPress `r` to retry."))
            .style(Style::default().fg(Color::Red));
        f.render_widget(p, area);
        return;
    }
    if b.loading && b.items.is_empty() {
        let p = Paragraph::new("loading…").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    if b.items.is_empty() {
        let p = Paragraph::new("(no recent builds for this project)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    }
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("STATUS"),
        Cell::from("STARTED"),
        Cell::from("DUR"),
        Cell::from("INITIATOR"),
        Cell::from("SOURCE"),
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let rows: Vec<Row> = b
        .items
        .iter()
        .map(|r| {
            let n = format!("#{}", r.build_number);
            let status = format!("{} {}", r.status.glyph(), r.status.label());
            let status_style = match r.status {
                crate::codebuild::BuildStatus::Succeeded => Style::default().fg(Color::Green),
                crate::codebuild::BuildStatus::Failed
                | crate::codebuild::BuildStatus::Fault
                | crate::codebuild::BuildStatus::TimedOut => Style::default().fg(Color::Red),
                crate::codebuild::BuildStatus::InProgress => Style::default().fg(Color::Cyan),
                crate::codebuild::BuildStatus::Stopped => Style::default().fg(Color::Yellow),
                _ => Style::default().fg(Color::Gray),
            };
            let started = r
                .started_at_ms
                .map(format_ms)
                .unwrap_or_else(|| "—".to_string());
            let dur = r
                .duration_ms
                .map(|d| format!("{}s", d / 1000))
                .unwrap_or_else(|| "—".to_string());
            let initiator = r.initiator.clone().unwrap_or_else(|| "—".to_string());
            let source = r
                .source_version
                .as_ref()
                .map(|s| s.chars().take(12).collect::<String>())
                .unwrap_or_else(|| "—".to_string());
            Row::new(vec![
                Cell::from(n).style(Style::default().fg(Color::Yellow)),
                Cell::from(status).style(status_style),
                Cell::from(started),
                Cell::from(dur),
                Cell::from(initiator),
                Cell::from(source),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(8),
        Constraint::Length(28),
        Constraint::Length(14),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", tab.name)),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = TableState::default();
    state.select(Some(b.selected));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_logs(f: &mut Frame, area: Rect, tab: &TabState) {
    let TabData::Logs(l) = &tab.data else { return };
    if let Some(err) = &l.last_error {
        let p = Paragraph::new(format!("error: {err}")).style(Style::default().fg(Color::Red));
        f.render_widget(p, area);
        return;
    }
    let Some(pane) = l.pane.as_ref() else {
        let p =
            Paragraph::new("(spawning aws logs tail…)").style(Style::default().fg(Color::DarkGray));
        f.render_widget(p, area);
        return;
    };
    let body_rows = area.height.saturating_sub(2) as usize;
    let total = pane.lines.len();
    let start = if pane.scroll == usize::MAX {
        total.saturating_sub(body_rows)
    } else {
        pane.scroll.min(total.saturating_sub(body_rows.max(1)))
    };
    let lines: Vec<Line> = pane.lines[start..]
        .iter()
        .take(body_rows)
        .map(|ln| {
            let style = match ln.severity {
                crate::log_tail::LineSeverity::Error => Style::default().fg(Color::Red),
                crate::log_tail::LineSeverity::Warn => Style::default().fg(Color::Yellow),
                crate::log_tail::LineSeverity::Info => Style::default().fg(Color::Cyan),
                crate::log_tail::LineSeverity::Debug => Style::default().fg(Color::DarkGray),
                crate::log_tail::LineSeverity::Plain => Style::default().fg(Color::Gray),
            };
            Line::from(Span::styled(ln.text.clone(), style))
        })
        .collect();
    let title = match &pane.log_stream {
        Some(s) => format!(" {} · {} ", tab.name, s),
        None => format!(" {} · {} ", tab.name, pane.log_group),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · Enter/o open · y yank URL · L logs · r refresh · q quit ";
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn format_ms(ms: i64) -> String {
    // ISO-ish: `YYYY-MM-DD HH:MM` UTC. Used only for the "STARTED"
    // column — a quick eyeball of recency, not a parser target.
    let secs = ms / 1000;
    use chrono::TimeZone as _;
    chrono::Utc
        .timestamp_opt(secs, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}
