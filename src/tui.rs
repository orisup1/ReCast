use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::io;
use chrono::Local;
use crate::types::AppControl;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};

/// Shared log for TUI
#[derive(Default)]
struct TuiLog {
    lines: Mutex<Vec<String>>,
    max_lines: usize,
}

impl TuiLog {
    fn new(max_lines: usize) -> Self {
        Self {
            lines: Mutex::new(Vec::new()),
            max_lines,
        }
    }

    fn push(&self, line: String) {
        let mut guard = self.lines.lock().unwrap();
        guard.push(line);
        if guard.len() > self.max_lines {
            guard.remove(0);
        }
    }

    fn items(&self) -> Vec<ListItem<'_>> {
        let guard = self.lines.lock().unwrap();
        guard
            .iter()
            .rev()
            .map(|l| ListItem::new(l.clone()))
            .collect()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn run_tui(control: Arc<AppControl>) -> std::io::Result<()> {
    // Setup terminal
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let log = Arc::new(TuiLog::new(100));
    let log_clone = Arc::clone(&log);
    let control_clone = control.clone();
    // Status heartbeat: log a line whenever the enabled state or the fixed
    // counter changes (plus one initial line), for as long as the TUI runs.
    std::thread::spawn(move || {
        let mut last: Option<(bool, u64)> = None;
        loop {
            let enabled = control_clone.is_enabled();
            let fixed = control_clone.fixed_count();
            if last != Some((enabled, fixed)) {
                last = Some((enabled, fixed));
                log_clone.push(format!(
                    "[{}] recast: enabled={}, fixed={}",
                    Local::now().format("%H:%M:%S"),
                    if enabled { "ON" } else { "OFF" },
                    fixed
                ));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    let app = App {
        control,
        log,
        start: Instant::now(),
        tab: 0,
    };
    let res = run_app(&mut terminal, app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    res
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, &app))?;
        if crossterm::event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = crossterm::event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Esc => break,
                    KeyCode::Left | KeyCode::Char('h') => {
                        if app.tab > 0 {
                            app.tab -= 1;
                        }
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        if app.tab < 2 {
                            app.tab += 1;
                        }
                    }
                    // Toggle layout correction on/off.
                    KeyCode::Char('e') | KeyCode::Char(' ') => {
                        let enabled = !app.control.is_enabled();
                        app.control.set_enabled(enabled);
                    }
                    KeyCode::F(1) | KeyCode::Char('?') => app.tab = 2,
                    _ => {}
                }
            }
        }
        app.update();
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn ui(f: &mut Frame, app: &App) {
    let enabled = app.control.is_enabled();
    let fixed = app.control.fixed_count();
    let uptime = app.start.elapsed().as_secs();

    // Styles
    let title_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let border_style = Style::default().fg(Color::Gray);
    let normal = Style::default().fg(Color::White);
    let enabled_col = Style::default().fg(Color::Green);
    let disabled_col = Style::default().fg(Color::Red);
    let highlight = Style::default().bg(Color::DarkGray).fg(Color::Yellow);

    // Layout
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(0),    // body
            Constraint::Length(3), // footer
        ].as_ref())
        .split(f.size());

    // Header
    let header = Paragraph::new(Span::styled(
        "recast – layout correction daemon",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(border_style)
            .title(Span::styled("Status", title_style))
            .title_alignment(Alignment::Center),
    )
    .alignment(Alignment::Center);
    f.render_widget(header, chunks[0]);

    // Body: horizontal split
    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(60),
            Constraint::Percentage(40),
        ].as_ref())
        .split(chunks[1]);

    // Left: tabs
    let titles = ["Info", "Log", "Help"];
    let tabs = Tabs::new(titles.iter().map(|t| Line::from(*t)))
        .select(app.tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title(Span::styled("Menu", title_style))
                .title_alignment(Alignment::Center),
        )
        .style(normal)
        .highlight_style(highlight);
    f.render_widget(tabs, Layout::default()
        .constraints([Constraint::Length(3)].as_ref())
        .split(body_chunks[0])[0]);

    // Tab content
    let tab_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
        ].as_ref())
        .split(body_chunks[0]);
    match app.tab {
        0 => render_info(f, tab_area[1], enabled, fixed, uptime, &normal, &enabled_col, &disabled_col),
        1 => render_log(f, tab_area[1], &app.log, &normal),
        2 => render_help(f, tab_area[1], &normal),
        _ => {}
    }

    // Right: gauge + recent log
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
        ].as_ref())
        .split(body_chunks[1]);
    let gauge = Gauge::default()
        .gauge_style(
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::DarkGray)
        )
        .label(format!("{}%", if enabled { 100 } else { 0 }))
        .ratio(if enabled { 1.0 } else { 0.0 })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(border_style)
                .title(Span::styled("Activity", title_style))
                .title_alignment(Alignment::Center),
        );
    f.render_widget(gauge, right_chunks[0]);
    let recent = Paragraph::new({
        let guard = app.log.lines.lock().unwrap();
        let start = guard.len().saturating_sub(3);
        guard[start..]
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<&str>>()
            .join("\n")
    })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(border_style)
            .title(Span::styled("Recent", title_style))
            .title_alignment(Alignment::Center),
    )
    .style(normal);
    f.render_widget(recent, right_chunks[1]);

    // Footer
    let footer = Paragraph::new(Span::styled(
        " ← → / h l : switch tab   e/Space : toggle   q/Esc : quit   F1 : help ",
        Style::default().fg(Color::Gray),
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(border_style)
            .title(Span::styled("Controls", title_style))
            .title_alignment(Alignment::Center),
    )
    .alignment(Alignment::Center);
    f.render_widget(footer, chunks[2]);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[allow(clippy::too_many_arguments)]
fn render_info(f: &mut Frame, area: ratatui::layout::Rect, enabled: bool, fixed: u64, uptime: u64, normal: &Style, enabled_col: &Style, disabled_col: &Style) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled("Information", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
        .title_alignment(Alignment::Center);
    let text = vec![
        Line::from(vec![
            Span::styled("Enabled: ", *normal),
            Span::styled(
                if enabled { "[ON]" } else { "[OFF]" },
                if enabled { *enabled_col } else { *disabled_col },
            ),
        ]),
        Line::from(vec![
            Span::from("Fixed words: "),
            Span::styled(fixed.to_string(), *normal),
        ]),
        Line::from(vec![
            Span::from("Uptime: "),
            Span::styled(uptime.to_string(), *normal),
            Span::from(" s"),
        ]),
        Line::from(""),
        Line::from("Recast corrects mistyped keyboard layouts by switching the layout and re‑typing the word."),
    ];
    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn render_log(f: &mut Frame, area: ratatui::layout::Rect, log: &Arc<TuiLog>, normal: &Style) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled("Log", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
        .title_alignment(Alignment::Center);
    let items: Vec<ListItem> = log.items();
    let list = List::new(items)
        .block(block)
        .style(*normal)
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::Yellow))
        .highlight_symbol(">> ");
    f.render_widget(list, area);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn render_help(f: &mut Frame, area: ratatui::layout::Rect, normal: &Style) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled("Help", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
        .title_alignment(Alignment::Center);
    let text = vec![
        Line::from("Keyboard-centric navigation:"),
        Line::from("  ← / h     : previous tab"),
        Line::from("  → / l     : next tab"),
        Line::from("  e / Space : toggle layout correction on/off"),
        Line::from("  q / Esc   : quit"),
        Line::from("  F1 / ?    : show this help"),
        Line::from(""),
        Line::from("Features:"),
        Line::from("  • Responsive layout using Ratatui"),
        Line::from("  • Semantic colors (green=enabled, red=disabled)"),
        Line::from("  • Modular panels: Info, Log, Help"),
        Line::from("  • Activity gauge and recent log"),
        Line::from("  • Low‑CPU, terminal‑only UI"),
    ];
    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: true })
        .style(*normal);
    f.render_widget(paragraph, area);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
struct App {
    control: Arc<AppControl>,
    log: Arc<TuiLog>,
    start: Instant,
    tab: usize,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl App {
    fn update(&self) {
        // No-op
    }
}