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
//! foreground (top) and background (bottom), so the 32×32 image draws in 32×16
//! cells at the icon's true aspect ratio. Color is emitted as 24-bit truecolor
//! when the terminal advertises it (`COLORTERM`) and quantized to xterm-256
//! otherwise — notably for stock macOS Terminal.app, which is not truecolor.

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

/// Terminal color capability. macOS Terminal.app (and some others) speak only
/// 256-color SGR, not 24-bit; emitting truecolor there renders wrong, so we
/// detect support and quantize down when it's absent.
#[derive(Clone, Copy)]
enum ColorDepth {
    True,
    Ansi256,
}

/// Assume truecolor only when the terminal advertises it via `COLORTERM`
/// (iTerm2, kitty, WezTerm, Alacritty, Ghostty, most modern Linux terminals).
/// Everything else — notably stock macOS Terminal.app — gets the 256-color path.
fn color_depth() -> ColorDepth {
    match std::env::var("COLORTERM") {
        Ok(v) if v.contains("truecolor") || v.contains("24bit") => ColorDepth::True,
        _ => ColorDepth::Ansi256,
    }
}

/// SGR foreground escape for the detected depth.
fn fg(depth: ColorDepth, r: u8, g: u8, b: u8) -> String {
    match depth {
        ColorDepth::True => format!("\x1b[38;2;{r};{g};{b}m"),
        ColorDepth::Ansi256 => format!("\x1b[38;5;{}m", rgb_to_256(r, g, b)),
    }
}

/// SGR background escape for the detected depth.
fn bg(depth: ColorDepth, r: u8, g: u8, b: u8) -> String {
    match depth {
        ColorDepth::True => format!("\x1b[48;2;{r};{g};{b}m"),
        ColorDepth::Ansi256 => format!("\x1b[48;5;{}m", rgb_to_256(r, g, b)),
    }
}

/// Map a 24-bit color to the nearest xterm-256 index, considering both the
/// 6×6×6 color cube and the grayscale ramp and picking whichever is closer.
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |v: u8| -> usize {
        let mut best = 0usize;
        let mut bd = i32::MAX;
        for (i, &lv) in LEVELS.iter().enumerate() {
            let d = (lv - v as i32).abs();
            if d < bd {
                bd = d;
                best = i;
            }
        }
        best
    };
    let dist = |c: (i32, i32, i32)| {
        let (dr, dg, db) = (c.0 - r as i32, c.1 - g as i32, c.2 - b as i32);
        dr * dr + dg * dg + db * db
    };

    // Color-cube candidate.
    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube_code = 16 + 36 * ri + 6 * gi + bi;
    let cube_rgb = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);

    // Grayscale-ramp candidate (indices 232..=255, values 8,18,…,238).
    let gray = (r as i32 + g as i32 + b as i32) / 3;
    let gray_i = ((gray - 8).max(0) / 10).min(23);
    let gray_val = 8 + 10 * gray_i;
    let gray_code = 232 + gray_i;

    if dist((gray_val, gray_val, gray_val)) < dist(cube_rgb) {
        gray_code as u8
    } else {
        cube_code as u8
    }
}

/// Compose the logo drawing beside the info panel.
fn color_banner() -> String {
    let cyan = "\x1b[38;5;39m";
    let dim = "\x1b[2m";
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";

    let rows = logo_rows(color_depth());
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
fn logo_rows(depth: ColorDepth) -> Vec<String> {
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
            let (br, bgc, bb, ba) = if y + 1 < LOGO_H { px(x, y + 1) } else { (0, 0, 0, 0) };
            let top = ta >= ALPHA_CUTOFF;
            let bot = ba >= ALPHA_CUTOFF;
            // `▀` shows the top pixel as foreground, the bottom as background;
            // `▄` is the inverse. When only one half is opaque we reset the
            // other to the terminal's default so transparency reads correctly.
            match (top, bot) {
                (true, true) => {
                    s.push_str(&fg(depth, tr, tg, tb));
                    s.push_str(&bg(depth, br, bgc, bb));
                    s.push('▀');
                }
                (true, false) => {
                    s.push_str("\x1b[0m");
                    s.push_str(&fg(depth, tr, tg, tb));
                    s.push('▀');
                }
                (false, true) => {
                    s.push_str("\x1b[0m");
                    s.push_str(&fg(depth, br, bgc, bb));
                    s.push('▄');
                }
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
        assert_eq!(logo_rows(ColorDepth::True).len(), LOGO_H / 2);
        assert_eq!(logo_rows(ColorDepth::Ansi256).len(), LOGO_H / 2);
    }

    #[test]
    fn rgb_to_256_is_in_range_and_sane() {
        // Pure white / black / brand blue land on reasonable indices.
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(255, 255, 255), 231);
        let _ = rgb_to_256(70, 167, 240); // must not panic / overflow
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
