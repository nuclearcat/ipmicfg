//! Terminal UI helpers: colors, status badges, headers and simple aligned tables.
//!
//! Color output is auto-disabled when stdout is not a TTY, when `NO_COLOR` is set,
//! or when the user passes `--no-color`.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR_ENABLED: AtomicBool = AtomicBool::new(false);

/// Initialise color support. Call once at startup.
///
/// `force_off` corresponds to the `--no-color` flag.
pub fn init_color(force_off: bool) {
    let enabled =
        !force_off && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    COLOR_ENABLED.store(enabled, Ordering::Relaxed);
}

fn enabled() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

fn paint(text: &str, code: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint(s, "1")
}
pub fn dim(s: &str) -> String {
    paint(s, "2")
}
pub fn red(s: &str) -> String {
    paint(s, "31")
}
pub fn green(s: &str) -> String {
    paint(s, "32")
}
pub fn yellow(s: &str) -> String {
    paint(s, "33")
}
pub fn cyan(s: &str) -> String {
    paint(s, "36")
}

/// Health status used to color sensor readings, power state, etc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Crit,
    Unknown,
}

impl Status {
    /// A fixed-width, colored badge.
    pub fn badge(self) -> String {
        match self {
            Status::Ok => green("OK  "),
            Status::Warn => yellow("WARN"),
            Status::Crit => red("CRIT"),
            Status::Unknown => dim(" -- "),
        }
    }
}

/// A transient one-line status message for operations that take long enough
/// that a silent terminal looks like a hang.
///
/// It goes to stderr so it never contaminates piped output, and only when
/// stderr is a terminal. Dropping it erases the line, so the message leaves no
/// trace once the work finishes — including on the early-return paths.
pub struct Progress {
    active: bool,
}

impl Progress {
    pub fn start(message: &str) -> Self {
        let active = std::io::stderr().is_terminal();
        if active {
            let mut err = std::io::stderr();
            let _ = write!(err, "{}", dim(message));
            let _ = err.flush();
        }
        Self { active }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut err = std::io::stderr();
        // Carriage return plus erase-to-end-of-line: cursor control, not color,
        // so it is correct even with --no-color or NO_COLOR.
        let _ = write!(err, "\r\x1b[2K");
        let _ = err.flush();
    }
}

/// Print a top-level section header.
pub fn header(title: &str) {
    println!();
    println!("{}", bold(&cyan(title)));
    println!("{}", dim(&"─".repeat(title.chars().count().max(8))));
}

/// Print an aligned `key: value` pair, with the key right-padded to `width`.
pub fn kv(key: &str, value: &str, width: usize) {
    println!("  {}  {}", dim(&format!("{key:<width$}")), value);
}

/// A minimal left-aligned table renderer that respects per-column widths and
/// keeps alignment correct even when cells contain ANSI color codes.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<Cell>>,
    aligns: Vec<Align>,
}

#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Right,
}

/// A table cell carries both the visible text (for width calculation) and the
/// rendered text (which may include color escapes).
pub struct Cell {
    plain: String,
    rendered: String,
}

impl Cell {
    pub fn new(plain: impl Into<String>) -> Self {
        let plain = plain.into();
        Self {
            rendered: plain.clone(),
            plain,
        }
    }

    /// A cell whose plain width differs from its rendered (colored) form.
    pub fn colored(plain: impl Into<String>, rendered: impl Into<String>) -> Self {
        Self {
            plain: plain.into(),
            rendered: rendered.into(),
        }
    }
}

impl Table {
    pub fn new(headers: &[&str], aligns: &[Align]) -> Self {
        Self {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
            aligns: aligns.to_vec(),
        }
    }

    pub fn row(&mut self, cells: Vec<Cell>) {
        self.rows.push(cells);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn print(&self) {
        let ncols = self.headers.len();
        let mut widths = vec![0usize; ncols];
        for (i, h) in self.headers.iter().enumerate() {
            widths[i] = h.chars().count();
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(ncols) {
                widths[i] = widths[i].max(cell.plain.chars().count());
            }
        }

        // Header
        let mut line = String::from("  ");
        for (i, h) in self.headers.iter().enumerate() {
            line.push_str(&pad(
                &dim(&bold(h)),
                h.chars().count(),
                widths[i],
                self.align(i),
            ));
            if i + 1 < ncols {
                line.push_str("  ");
            }
        }
        println!("{line}");

        // Rows
        for row in &self.rows {
            let mut line = String::from("  ");
            for (i, &width) in widths.iter().enumerate().take(ncols) {
                let cell = row.get(i);
                let (plain_w, rendered) = match cell {
                    Some(c) => (c.plain.chars().count(), c.rendered.clone()),
                    None => (0, String::new()),
                };
                line.push_str(&pad(&rendered, plain_w, width, self.align(i)));
                if i + 1 < ncols {
                    line.push_str("  ");
                }
            }
            println!("{}", line.trim_end());
        }
    }

    fn align(&self, i: usize) -> Align {
        self.aligns.get(i).copied().unwrap_or(Align::Left)
    }
}

/// Pad `rendered` (whose visible width is `plain_w`) to `target` columns.
fn pad(rendered: &str, plain_w: usize, target: usize, align: Align) -> String {
    let fill = target.saturating_sub(plain_w);
    match align {
        Align::Left => format!("{}{}", rendered, " ".repeat(fill)),
        Align::Right => format!("{}{}", " ".repeat(fill), rendered),
    }
}
