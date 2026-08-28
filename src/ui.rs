//! Terminal rendering: nushell-style rounded tables, boxed panels, rules,
//! step lines, a spinner, and a confirm prompt.
//!
//! Everything is written through an `anstream` stream, which enables Windows
//! VT processing and strips ANSI automatically when output is redirected, so
//! callers can style unconditionally with `owo-colors` and `tabled`.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::{OwoColorize, Style as OStyle};
use tabled::builder::Builder;
use tabled::settings::object::{Columns, Rows};
use tabled::settings::{Alignment, Color, Style, Width};

/// Semantic color for step markers.
#[derive(Clone, Copy)]
pub enum Tone {
    Green,
    Yellow,
    Red,
    Dim,
}

impl Tone {
    fn style(self) -> OStyle {
        let s = OStyle::new();
        match self {
            Tone::Green => s.green(),
            Tone::Yellow => s.yellow(),
            Tone::Red => s.red(),
            Tone::Dim => s.dimmed(),
        }
    }
}

/// Terminal width: the `COLUMNS` override if set, else the detected size,
/// falling back to 80.
pub fn width() -> usize {
    if let Ok(c) = std::env::var("COLUMNS")
        && let Ok(n) = c.trim().parse::<usize>()
        && n > 0
    {
        return n;
    }
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80)
}

/// Print a blank line.
pub fn blank() {
    anstream::println!();
}

/// Print a plain (already-styled) line.
pub fn line(s: impl std::fmt::Display) {
    anstream::println!("{s}");
}

/// A centered title flanked by `─` runs in the brand color, spanning the
/// terminal width.
pub fn rule(title: &str) {
    let w = width();
    let dashes = w.saturating_sub(title.chars().count() + 2); // spaces around title
    let left = dashes / 2;
    let bar = |n: usize| "─".repeat(n).bright_cyan().to_string();
    anstream::println!("{} {} {}", bar(left), title.bold(), bar(dashes - left));
}

/// A bold, underlined section heading.
pub fn heading(title: &str) {
    anstream::println!("\n {}", title.bold().underline());
}

/// A single step line, e.g. `  OK  Nu detection: Inserted ...`.
pub fn step(marker: &str, tone: Tone, name: &str, message: &str) {
    anstream::println!("  {}  {}: {}", marker.style(tone.style()).bold(), name, message);
}

/// A rounded box titled `title` around `body` (multi-line allowed). Borders
/// are uncolored (matching nushell); pre-style text as needed before calling.
pub fn panel(title: &str, body: &str) {
    let mut builder = Builder::default();
    builder.push_record([title]);
    for l in body.lines() {
        builder.push_record([l]);
    }
    let mut table = builder.build();
    table.with(Style::rounded());
    table.modify(Rows::first(), Alignment::center());
    table.with(Width::wrap(width()).keep_words(true));
    anstream::println!("{table}");
}

/// Render a rounded table in nushell's default `Rounded` style: a green-bold
/// centered header, a right-aligned green-bold `#` row-index column, left
/// data, and uncolored borders. Cell strings may contain ANSI styling;
/// `tabled`'s ansi feature measures width correctly.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut builder = Builder::default();
    builder.push_record(std::iter::once("#".to_string()).chain(headers.iter().map(|h| h.to_string())));
    for (i, row) in rows.iter().enumerate() {
        builder.push_record(std::iter::once(i.to_string()).chain(row.iter().cloned()));
    }
    let mut table = builder.build();
    table.with(Style::rounded());
    // Row-index column: right-aligned, green-bold (nushell `row_index`).
    table.modify(Columns::first(), Alignment::right());
    table.modify(Columns::first(), Color::FG_GREEN | Color::BOLD);
    // Header row last, so the `#` header re-centers over the index column.
    table.modify(Rows::first(), Alignment::center());
    table.modify(Rows::first(), Color::FG_GREEN | Color::BOLD);
    // Wrap cell content so the table fits the terminal, like nushell.
    table.with(Width::wrap(width()).keep_words(true));
    table.to_string()
}

/// Run `f` while a spinner shows `msg`; clear the spinner when done.
pub fn with_spinner<T>(msg: &str, f: impl FnOnce() -> T) -> T {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb.set_message(msg.to_owned());
    pb.enable_steady_tick(Duration::from_millis(80));
    let out = f();
    pb.finish_and_clear();
    out
}

/// Ask a yes/no question (defaults to no). Returns false on cancel/EOF.
pub fn confirm(prompt: &str) -> bool {
    inquire::Confirm::new(prompt)
        .with_default(false)
        .prompt()
        .unwrap_or(false)
}
