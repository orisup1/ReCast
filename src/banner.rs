//! Startup banner: a neofetch-style rendering of the ReCast keycap logo shown
//! only when the program is launched directly from an interactive terminal.
//!
//! When ReCast is started by the macOS LaunchAgent or the Windows Scheduled
//! Task (its "app" autostart forms), its stdout is redirected to a log file or
//! not attached to any console — so `IsTerminal` is false and no banner prints.
//! A direct `recast` command in a shell has a TTY on stdout, so the user gets
//! greeted with the logo drawn to the left of a small info panel.
//!
//! The drawing is produced from an embedded 32×32 RGBA snapshot of the icon
//! (`assets/banner-logo.rgba`, regenerable from `assets/banner-logo.svg`) using
//! Unicode upper-half blocks (`▀`): each character packs two vertical pixels as
//! foreground (top) and background (bottom) truecolor, so the 32×32 image draws
//! in 32×16 cells at the icon's true aspect ratio.

use std::io::{IsTerminal, Write};

/// Raw 32×32 RGBA of the upright keycap icon, baked in at compile time.
const LOGO_RGBA: &[u8] = include_bytes!("../assets/banner-logo.rgba");
const LOGO_W: usize = 32;
const LOGO_H: usize = 32;
/// Alpha at or above this counts as an opaque pixel.
const ALPHA_CUTOFF: u8 = 110;

/// True when stdout is connected to an interactive terminal, i.e. the program
/// was run by a direct shell command rather than an autostart service.
pub fn ran_from_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// Print the logo banner to stdout. Intended to be called only when
/// [`ran_from_terminal`] returns true. Honors `NO_COLOR` (https://no-color.org)
/// by falling back to a plain monochrome rendering (the half-block drawing needs
/// color to convey shape).
pub fn print_logo() {
    let banner = if std::env::var_os("NO_COLOR").is_some() {
        plain_banner()
    } else {
        color_banner()
    };
    // Ignore write errors: a failed greeting must never take down startup.
    let mut out = std::io::stdout();
    let _ = out.write_all(banner.as_bytes());
    let _ = out.flush();
}

/// Info lines shown to the right of the drawing (label, value), neofetch-style.
fn info_lines(cyan: &str, dim: &str, bold: &str, reset: &str) -> Vec<String> {
    let v = env!("CARGO_PKG_VERSION");
    vec![
        format!("{bold}{cyan}reCast{reset} {dim}v{v}{reset}"),
        format!("{dim}──────────────────────────────{reset}"),
        format!("{cyan}Layouts{reset}   English ⇄ Hebrew"),
        format!("{cyan}Options{reset}   recast --help"),
        format!("{cyan}Stop{reset}      recast --stop"),
    ]
}

/// Compose the truecolor drawing beside the info panel.
fn color_banner() -> String {
    let cyan = "\x1b[38;5;39m";
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";

    let rows = logo_rows();
    let info = info_lines(cyan, dim, bold, reset);
    // Vertically center the info panel against the drawing.
    let start = rows.len().saturating_sub(info.len()) / 2;

    let mut s = String::from("\n");
    for (i, row) in rows.iter().enumerate() {
        s.push_str("  ");
        s.push_str(row);
        if i >= start && i - start < info.len() {
            s.push_str("   ");
            s.push_str(&info[i - start]);
        }
        s.push('\n');
    }
    s.push('\n');
    s
}

/// Render the embedded RGBA image as half-block rows. Each returned string is
/// exactly `LOGO_W` display cells wide (transparent pixels become spaces), so
/// callers can append text after it and keep alignment.
fn logo_rows() -> Vec<String> {
    let px = |x: usize, y: usize| -> (u8, u8, u8, u8) {
        let i = (y * LOGO_W + x) * 4;
        (LOGO_RGBA[i], LOGO_RGBA[i + 1], LOGO_RGBA[i + 2], LOGO_RGBA[i + 3])
    };
    let mut rows = Vec::with_capacity(LOGO_H / 2);
    let mut y = 0;
    while y < LOGO_H {
        let mut s = String::new();
        for x in 0..LOGO_W {
            let (tr, tg, tb, ta) = px(x, y);
            let (br, bg, bb, ba) = if y + 1 < LOGO_H { px(x, y + 1) } else { (0, 0, 0, 0) };
            let top = ta >= ALPHA_CUTOFF;
            let bot = ba >= ALPHA_CUTOFF;
            match (top, bot) {
                (true, true) => {
                    s.push_str(&format!(
                        "\x1b[38;2;{tr};{tg};{tb}m\x1b[48;2;{br};{bg};{bb}m▀"
                    ));
                }
                (true, false) => s.push_str(&format!("\x1b[0m\x1b[38;2;{tr};{tg};{tb}m▀")),
                (false, true) => s.push_str(&format!("\x1b[0m\x1b[38;2;{br};{bg};{bb}m▄")),
                (false, false) => s.push_str("\x1b[0m "),
            }
        }
        s.push_str("\x1b[0m");
        rows.push(s);
        y += 2;
    }
    rows
}

/// Monochrome fallback for `NO_COLOR`: a plain-ASCII keycap with the recast loop
/// and the same info, since the half-block drawing conveys nothing without color.
fn plain_banner() -> String {
    let v = env!("CARGO_PKG_VERSION");
    format!(
        "\n\
   ,----------.      reCast v{v}\n\
   | ,------. |      ------------------------------\n\
   | |  /\\  | |      Layouts   English <-> Hebrew\n\
   | | /  \\ | |      Options   recast --help\n\
   | '------' |      Stop      recast --stop\n\
   '----------'\n\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_rows_are_uniform_and_half_height() {
        let rows = logo_rows();
        assert_eq!(rows.len(), LOGO_H / 2);
    }

    #[test]
    fn print_logo_does_not_panic() {
        // Exercises the format + write path. In `cargo test` stdout is captured
        // (not a TTY), so this prints nothing visible.
        print_logo();
    }

    #[test]
    fn print_logo_respects_no_color() {
        std::env::set_var("NO_COLOR", "1");
        print_logo();
        std::env::remove_var("NO_COLOR");
        print_logo();
    }

    #[test]
    fn terminal_detection_returns_without_panic() {
        let _: bool = ran_from_terminal();
    }
}
