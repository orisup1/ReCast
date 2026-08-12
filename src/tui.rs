//! The terminal dashboard behind `-g` / `--gui`.
//!
//! Linux and Windows only — `main` gates the module itself, because on macOS
//! the event tap owns the main run loop and the menubar is the UI there. Every
//! item below used to repeat that gate as a `cfg(any(linux, macos, windows))`
//! attribute, which was true on all three and so decided nothing.

use crate::types::{AppControl, Correction};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};

/// How long a pause started from the TUI lasts — the same half hour the tray
/// offers, so the two UIs mean the same thing by the word.
const PAUSE_LENGTH: Duration = Duration::from_secs(30 * 60);

/// Uptime as something readable at a glance. A raw second count is the one
/// number here nobody can use: "Uptime: 419213 s" answers the question with
/// arithmetic homework.
fn uptime_human(secs: u64) -> String {
    let (days, hours, mins) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    match (days, hours) {
        (0, 0) => format!("{}m {}s", mins, secs % 60),
        (0, _) => format!("{hours}h {mins}m"),
        _ => format!("{days}d {hours}h {mins}m"),
    }
}

/// One line of the corrections log: when it happened, what it was, and whether
/// the user took it back.
fn correction_line(c: &Correction) -> String {
    format!(
        "[{}] {} → {} ({}){}",
        c.at.format("%H:%M:%S"),
        c.from,
        c.to,
        c.kind.tag(),
        if c.undone { "  ↩ undone" } else { "" }
    )
}

pub fn run_tui(control: Arc<AppControl>) -> std::io::Result<()> {
    // Setup terminal
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = App {
        control,
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

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> std::io::Result<()> {
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
                    // The one switch: layout, spelling and completion together.
                    KeyCode::Char('e') | KeyCode::Char(' ') => {
                        let enabled = !app.control.is_switched_on();
                        app.control.set_enabled(enabled);
                    }
                    // Pause for a while, or end a pause already running.
                    KeyCode::Char('p') => {
                        if app.control.pause_remaining().is_some() {
                            app.control.resume();
                        } else {
                            app.control.pause_for(PAUSE_LENGTH);
                        }
                    }
                    // Re-read abbrev.txt / ignore.txt now rather than waiting
                    // for the watcher's next pass.
                    KeyCode::Char('r') => crate::complete::reload_user_files(),
                    KeyCode::F(1) | KeyCode::Char('?') => app.tab = 2,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, app: &App) {
    let enabled = app.control.is_enabled();
    let uptime = app.start.elapsed().as_secs();
    let history = app.control.history();

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
        format!(
            "ReCast v{} — layout correction, autocorrect and completion",
            env!("CARGO_PKG_VERSION")
        ),
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
        0 => render_info(f, tab_area[1], app, uptime, &normal, &enabled_col, &disabled_col),
        1 => render_log(f, tab_area[1], &history, &normal),
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
    let recent = Paragraph::new(
        history
            .iter()
            .take(3)
            .map(correction_line)
            .collect::<Vec<String>>()
            .join("\n"),
    )
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
        " ← → : tab   e/Space : toggle   p : pause 30m   r : reload lists   q : quit   F1 : help ",
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

#[allow(clippy::too_many_arguments)]
fn render_info(f: &mut Frame, area: ratatui::layout::Rect, app: &App, uptime: u64, normal: &Style, enabled_col: &Style, disabled_col: &Style) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled("Information", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
        .title_alignment(Alignment::Center);
    let enabled = app.control.is_enabled();
    let paused = app.control.pause_remaining();
    let state = match paused {
        Some(left) => format!("[PAUSED {} min]", left.as_secs() / 60 + 1),
        None if enabled => "[ON]".to_string(),
        None => "[OFF]".to_string(),
    };
    let mut text = vec![
        Line::from(vec![
            Span::styled("Enabled: ", *normal),
            Span::styled(state, if enabled { *enabled_col } else { *disabled_col }),
        ]),
        Line::from(vec![
            Span::from("Fixed words: "),
            Span::styled(app.control.fixed_count().to_string(), *normal),
        ]),
        // The undo tally sits next to the fixed one because the pair is the
        // reading: corrections that stick are invisible, so only the ratio
        // says whether the thresholds suit this user.
        Line::from(vec![
            Span::from("Taken back: "),
            Span::styled(app.control.undo_count().to_string(), *normal),
        ]),
        Line::from(vec![
            Span::from("Uptime: "),
            Span::styled(uptime_human(uptime), *normal),
        ]),
    ];
    if let Some(hint) = app.control.tighten_hint() {
        text.push(Line::from(""));
        text.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::Yellow),
        )));
    }
    text.push(Line::from(""));
    text.push(Line::from(
        "ReCast retypes words you typed in the wrong keyboard layout, fixes English \
         typos in place, and finishes words on a tap of Right Shift.",
    ));
    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// The corrections themselves, newest first. This used to be a heartbeat of
/// "enabled=ON, fixed=3" lines, which said that something had happened without
/// ever saying what — the one question a log of silent text replacement exists
/// to answer.
fn render_log(f: &mut Frame, area: ratatui::layout::Rect, history: &[Correction], normal: &Style) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled("Corrections", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
        .title_alignment(Alignment::Center);
    let items: Vec<ListItem> = if history.is_empty() {
        vec![ListItem::new("No corrections yet.")]
    } else {
        history.iter().map(|c| ListItem::new(correction_line(c))).collect()
    };
    let list = List::new(items)
        .block(block)
        .style(*normal)
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::Yellow))
        .highlight_symbol(">> ");
    f.render_widget(list, area);
}

fn render_help(f: &mut Frame, area: ratatui::layout::Rect, normal: &Style) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled("Help", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
        .title_alignment(Alignment::Center);
    // What the user needs from a help tab is the two gestures and where their
    // files live — none of which is visible anywhere else, since ReCast has no
    // window of its own and does its work inside other applications.
    let text = vec![
        Line::from("This dashboard:"),
        Line::from("  ← / h     : previous tab"),
        Line::from("  → / l     : next tab"),
        Line::from("  e / Space : turn correction on/off (remembered across restarts)"),
        Line::from("  p         : pause for 30 minutes, or resume"),
        Line::from("  r         : re-read abbrev.txt and ignore.txt now"),
        Line::from("  q / Esc   : quit"),
        Line::from("  F1 / ?    : show this help"),
        Line::from(""),
        Line::from("While typing anywhere:"),
        Line::from("  Right Shift (tap)  : finish the word; tap again to cycle guesses"),
        Line::from("  Ctrl Ctrl (tap x2) : undo the correction the cursor is sitting on,"),
        Line::from("                       and stop correcting that word. On a word that"),
        Line::from("                       was skipped because it is listed, it does the"),
        Line::from("                       opposite: unlists it and corrects it."),
        Line::from(""),
        Line::from("Your files (edits are picked up within ~2s, no restart):"),
        Line::from("  abbrev.txt : `btw = by the way`, one per line"),
        Line::from("  ignore.txt : one word per line, never corrected"),
    ];
    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: true })
        .style(*normal);
    f.render_widget(paragraph, area);
}

struct App {
    control: Arc<AppControl>,
    start: Instant,
    tab: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_as_a_duration_rather_than_a_number() {
        assert_eq!(uptime_human(0), "0m 0s");
        assert_eq!(uptime_human(90), "1m 30s");
        assert_eq!(uptime_human(3600), "1h 0m");
        assert_eq!(uptime_human(90_061), "1d 1h 1m");
    }
}
