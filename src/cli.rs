//! CLI output formatting -- tables, panels, spinners, step display.
//!
//! Uses `richrs` for all terminal rendering: Console with markup, Panel,
//! Table, Rule, Status spinner, and Confirm prompt -- a single dependency
//! mirroring the Python version's use of the `rich` library.

use std::borrow::Cow;

use color_eyre::eyre::{Result, eyre};
use richrs::prelude::*;
use richrs::segment::Segments;
use richrs::table::Row;

use crate::core::{
    PatchResult, StepResult, check_status, patch_cli_agent, patch_ide_agent, revert_all,
};
use crate::integrity::{self, update_integrity};
use crate::paths::detect_paths;

// ---------------------------------------------------------------------------
//  help / version
// ---------------------------------------------------------------------------

pub fn cmd_help(version: &str) {
    let mut c = Console::new();
    let w = c.width();

    let _ = c.print("");
    let _ = print_renderable(&mut c, &Rule::with_title(markup("[bold]nupatch[/]"))
        .style(Style::parse("bright_cyan").unwrap_or_default())
        .render(w));
    let _ = c.print("");
    let _ = c.print(
        "Automatically patches Cursor's CLI and IDE agents so they recognise \
         [bold yellow]nushell[/] and route\nexecution through \
         [bold]NaiveTerminalExecutor[/].",
    );
    let _ = c.print("\n[dim]Usage:[/] [bold]nupatch[/] [cyan]<COMMAND>[/]");

    // Core Commands
    let core = "\
[bold cyan]patch[/]                Apply nushell patches to Cursor agents.
  [dim]--cli-only[/]          Patch CLI agent only
  [dim]--ide-only[/]          Patch IDE agent only
  [dim]-n, --dry-run[/]       Preview changes without applying
[bold cyan]revert[/]               Restore all patched files from backups.";
    let panel = Panel::new(markup(core))
        .title(markup("[bold]Core Commands[/]"))
        .border_style(Style::parse("cyan").unwrap_or_default());
    let _ = print_renderable(&mut c, &panel.render(w));

    // Diagnostics
    let diag = "\
[bold cyan]status[/]  [dim](s)[/]           Show current patch status for CLI and IDE agents.
[bold cyan]verify[/]  [dim](v)[/]           Verify [bold]product.json[/] checksums against files on disk.
[bold cyan]fix-checksums[/] [dim](fc)[/]    Recalculate all [bold]product.json[/] checksums.";
    let panel = Panel::new(markup(diag))
        .title(markup("[bold]Diagnostics[/]"))
        .border_style(Style::parse("cyan").unwrap_or_default());
    let _ = print_renderable(&mut c, &panel.render(w));

    // Info
    let info = "\
[bold cyan]--help[/]  [dim](-h)[/]          Display this message and exit.
[bold cyan]--version[/] [dim](-V)[/]        Display application version.";
    let panel = Panel::new(markup(info))
        .title(markup("[bold]Info[/]"))
        .border_style(Style::parse("cyan").unwrap_or_default());
    let _ = print_renderable(&mut c, &panel.render(w));

    let _ = c.print(&format!("[dim]v{version}[/]"));
}

pub fn cmd_version(version: &str) {
    let mut c = Console::new();
    let _ = c.print(&format!("[bold]nupatch[/] [dim]v{version}[/]"));
}

// ---------------------------------------------------------------------------
//  Helpers
// ---------------------------------------------------------------------------

/// Parse a Rich markup string into a styled `Text` object.
/// Use this when passing markup to Panel, Table, Rule, etc.
/// (`console.print()` parses markup internally; these components don't.)
fn markup(s: &str) -> Text {
    Markup::parse(s).unwrap_or_default().to_text()
}

/// Print a renderable (Panel, Table, etc.) through the console.
fn print_renderable(
    console: &mut Console,
    segments: &Segments,
) -> Result<()> {
    console.write_segments(segments)?;
    console.write("\n")?;
    console.flush()?;
    Ok(())
}

fn step_icon(step: &StepResult) -> &'static str {
    if step.skipped {
        "[dim]SKIP[/]"
    } else if step.ok {
        "[bold green]  OK[/]"
    } else {
        "[bold red]FAIL[/]"
    }
}

fn display_steps(console: &mut Console, steps: &[StepResult]) {
    for step in steps {
        let _ = console.print(&format!(
            "  {}  {}: {}",
            step_icon(step),
            step.name,
            step.message
        ));
    }
}

fn display_result(console: &mut Console, title: &str, result: &PatchResult) {
    let _ = console.print(&format!("\n [bold underline]{title}[/]"));
    display_steps(console, &result.steps);
}

fn display_dry_run_detail(console: &mut Console, steps: &[StepResult]) {
    let width = console.width();
    for step in steps {
        if !step.detail.is_empty() && !step.skipped {
            let _ = console.print("");
            let panel = Panel::new(step.detail.as_str())
                .title(step.name)
                .border_style(Style::parse("yellow").unwrap_or_default());
            let _ = print_renderable(console, &panel.render(width));
        }
    }
}

fn display_error_panel(console: &mut Console, message: &str) {
    let width = console.width();
    let panel = Panel::new(markup(&format!("[bold red]{message}[/]")))
        .title("Error")
        .border_style(Style::parse("red").unwrap_or_default());
    let _ = print_renderable(console, &panel.render(width));
}

fn require_cursor_app(
    console: &mut Console,
    cursor_app: Option<&std::path::Path>,
) -> Result<()> {
    if cursor_app.is_none() {
        display_error_panel(console, "Could not find Cursor installation.");
        return Err(eyre!("Could not find Cursor installation."));
    }
    Ok(())
}

fn require_paths<'a>(
    console: &mut Console,
    cursor_app: Option<&'a std::path::Path>,
    product_json: Option<&'a std::path::Path>,
) -> Result<(&'a std::path::Path, &'a std::path::Path)> {
    let cursor_app = cursor_app.ok_or_else(|| {
        display_error_panel(console, "Could not find Cursor installation.");
        eyre!("Could not find Cursor installation.")
    })?;
    let product_json = product_json.ok_or_else(|| {
        display_error_panel(console, "Could not find product.json.");
        eyre!("Could not find product.json.")
    })?;
    Ok((cursor_app, product_json))
}

// ---------------------------------------------------------------------------
//  patch
// ---------------------------------------------------------------------------

pub fn cmd_patch(cli_only: bool, ide_only: bool, dry_run: bool) -> Result<()> {
    let mut console = Console::new();
    let paths = detect_paths();
    if !cli_only {
        require_cursor_app(&mut console, paths.cursor_app.as_deref())?;
    }

    let _ = console.print("");
    let width = console.width();
    let rule = Rule::with_title(markup("[bold]nupatch[/]"))
        .style(Style::parse("bright_cyan").unwrap_or_default());
    let _ = print_renderable(&mut console, &rule.render(width));

    if dry_run {
        let _ = console.print("");
        let panel = Panel::new(markup("[yellow]DRY RUN[/] -- no files will be modified"))
            .border_style(Style::parse("yellow").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
    }

    let mut ok = true;

    // CLI Agent
    if !ide_only {
        if let Some(ref cli_index) = paths.cli_index {
            let cli_result = Status::new("Patching CLI agent...")
                .run(|| patch_cli_agent(cli_index, dry_run));

            display_result(&mut console, "CLI Agent", &cli_result);
            if dry_run {
                display_dry_run_detail(&mut console, &cli_result.steps);
            }
            if !cli_result.success {
                ok = false;
            }
        } else {
            let _ = console.print("\n [dim]CLI agent not found, skipping.[/]");
        }
    }

    // IDE Agent
    if !cli_only {
        if let Some(ref ide_main) = paths.ide_main {
            let ide_result = Status::new("Patching IDE agent...")
                .run(|| patch_ide_agent(ide_main, dry_run));

            display_result(&mut console, "IDE Agent", &ide_result);
            if dry_run {
                display_dry_run_detail(&mut console, &ide_result.steps);
            }

            if ide_result.success && !dry_run {
                let integrity_result = Status::new("Updating integrity hashes...")
                    .run(|| {
                        update_integrity(
                            ide_main,
                            paths.ehp.as_deref(),
                            paths.product_json.as_deref(),
                            paths.cursor_app.as_deref(),
                            dry_run,
                        )
                    });

                display_result(&mut console, "Integrity Chain", &integrity_result);
                if !integrity_result.success {
                    ok = false;
                }
            } else if !ide_result.success {
                ok = false;
            }
        } else {
            let _ = console.print("\n [dim]IDE agent not found, skipping.[/]");
        }
    }

    // Summary
    let _ = console.print("");
    let width = console.width();
    if ok {
        let mut lines = vec![
            "[bold green]Patching complete![/]".to_string(),
            String::new(),
        ];
        if !ide_only {
            lines.push(
                "[cyan]CLI:[/] Nushell auto-detected from PATH -- \
                 no [bold]$env:SHELL[/] needed"
                    .to_string(),
            );
        }
        if !cli_only {
            lines.push(
                "[cyan]IDE:[/] Full quit + relaunch Cursor \
                 (not just Reload Window)"
                    .to_string(),
            );
        }
        let content = lines.join("\n");
        let panel = Panel::new(markup(&content))
            .title("Next Steps")
            .border_style(Style::parse("green").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
    } else {
        let panel = Panel::new(markup(
            "[bold red]Some patches failed.[/]  See errors above.",
        ))
        .title("Error")
        .border_style(Style::parse("red").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
        return Err(eyre!("Some patches failed"));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
//  revert
// ---------------------------------------------------------------------------

pub fn cmd_revert() -> Result<()> {
    let mut console = Console::new();
    let paths = detect_paths();

    let _ = console.print(
        "\n[yellow]This will revert all patches and restore from backups.[/]",
    );

    let confirmed = Confirm::new("Continue?")
        .default(false)
        .ask()?;

    if !confirmed {
        let _ = console.print("[dim]Aborted.[/]");
        return Ok(());
    }

    let result = Status::new("Reverting patches...")
        .run(|| revert_all(&paths));

    let _ = console.print("");
    for f in &result.files {
        if f.restored {
            let _ = console.print(&format!(
                "  [bold green]  OK[/]  Restored: {}",
                f.filename
            ));
        } else {
            let _ = console.print(&format!(
                "  [dim]SKIP[/]  No backup: {}",
                f.filename
            ));
        }
    }

    let _ = console.print("");
    let width = console.width();
    let panel = Panel::new(markup(
        "[bold green]Revert complete.[/]\nRestart Cursor to apply.",
    ))
    .title("Done")
    .border_style(Style::parse("green").unwrap_or_default());
    let _ = print_renderable(&mut console, &panel.render(width));

    Ok(())
}

// ---------------------------------------------------------------------------
//  status
// ---------------------------------------------------------------------------

pub fn cmd_status() -> Result<()> {
    let mut console = Console::new();
    let paths = detect_paths();

    let st = Status::new("Checking status...")
        .run(|| check_status(&paths));

    let _ = console.print("");

    // Table
    let mut table = Table::new()
        .title("Patch Status")
        .border_style(Style::parse("bright_cyan").unwrap_or_default())
        .header_style(Style::parse("bold magenta").unwrap_or_default());
    table.add_column(Column::new("Component").style(Style::new().bold()).min_width(5));
    table.add_column(Column::new("File"));
    table.add_column(Column::new("Backup"));
    table.add_column(Column::new("Patches").min_width(30));
    table.add_column(Column::new("Status"));

    // Collect all patch names to find the longest for padding.
    let all_names: Vec<&String> = [&st.cli, &st.ide]
        .iter()
        .flat_map(|c| c.patches.keys())
        .collect();
    let max_name_len = all_names.iter().map(|n| n.len()).max().unwrap_or(0);

    for (label, comp) in [("CLI", &st.cli), ("IDE", &st.ide)] {
        if comp.exists {
            let mut sorted_patches: Vec<(&String, &bool)> = comp.patches.iter().collect();
            sorted_patches.sort_by_key(|(name, _)| name.as_str());

            let mut patch_parts: Vec<String> = Vec::new();
            for (name, applied) in &sorted_patches {
                let padded_name = format!("{:width$}", name, width = max_name_len);
                let tag = if **applied {
                    "[green]PATCHED[/]"
                } else {
                    "[dim]ORIGINAL[/]"
                };
                patch_parts.push(format!("{padded_name}: {tag}"));
            }
            let patches_str = if patch_parts.is_empty() {
                "[dim]unknown[/]".to_string()
            } else {
                patch_parts.join("  ")
            };

            let all_patched = !comp.patches.is_empty() && comp.patches.values().all(|&v| v);
            let any_patched = comp.patches.values().any(|&v| v);

            let status_str = if all_patched {
                "[bold green]PATCHED[/]"
            } else if any_patched {
                "[bold yellow]PARTIAL[/]"
            } else {
                "[dim]ORIGINAL[/]"
            };

            let file_name = comp.path.as_deref().unwrap_or("N/A");
            let backup_str = if comp.backup_exists {
                "[green]exists[/]"
            } else {
                "[dim]none[/]"
            };

            table.add_row(Row::new([
                markup(label),
                markup(file_name),
                markup(backup_str),
                markup(&patches_str),
                markup(status_str),
            ]));
        } else {
            table.add_row(Row::new([
                markup(label),
                markup("[dim]not found[/]"),
                markup(""),
                markup(""),
                markup(""),
            ]));
        }
    }

    let width = console.width();
    let _ = print_renderable(&mut console, &table.render(width));

    // Integrity
    let mut int_lines: Vec<String> = Vec::new();
    if let Some(matches) = st.integrity.ehp_hash_matches {
        let icon = if matches {
            "[green]YES[/]"
        } else {
            "[red]NO[/]"
        };
        int_lines.push(format!("EHP hash matches main.js:  {icon}"));
    }
    if let Some(matches) = st.integrity.product_checksums_match {
        let icon = if matches {
            "[green]ALL MATCH[/]"
        } else {
            "[red]MISMATCH[/]"
        };
        int_lines.push(format!("product.json checksums:    {icon}"));
    }

    if !int_lines.is_empty() {
        let _ = console.print("");
        let content = int_lines.join("\n");
        let panel = Panel::new(markup(&content))
            .title("Integrity")
            .border_style(Style::parse("blue").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
    }

    // Overall
    let cli_ok =
        st.cli.exists && !st.cli.patches.is_empty() && st.cli.patches.values().all(|&v| v);
    let ide_ok =
        st.ide.exists && !st.ide.patches.is_empty() && st.ide.patches.values().all(|&v| v);

    let overall = if cli_ok && ide_ok {
        "[bold green]PATCHED[/] (both CLI + IDE)"
    } else if ide_ok {
        "[bold green]PATCHED[/] (IDE only)"
    } else if cli_ok {
        "[bold green]PATCHED[/] (CLI only)"
    } else {
        "[dim]ORIGINAL[/] (no patches applied)"
    };

    let _ = console.print("");
    let overall_title = format!("Overall: {overall}");
    let rule = Rule::with_title(markup(&overall_title))
        .style(Style::parse("bright_cyan").unwrap_or_default());
    let _ = print_renderable(&mut console, &rule.render(width));
    let _ = console.print("");

    Ok(())
}

// ---------------------------------------------------------------------------
//  verify
// ---------------------------------------------------------------------------

pub fn cmd_verify() -> Result<()> {
    let mut console = Console::new();
    let paths = detect_paths();
    let (cursor_app, product_json) = require_paths(
        &mut console,
        paths.cursor_app.as_deref(),
        paths.product_json.as_deref(),
    )?;

    let result = Status::new("Verifying checksums...")
        .run(|| {
            integrity::verify_checksums(product_json, cursor_app)
        })?;

    let _ = console.print("");

    let mut table = Table::new()
        .title("Checksum Verification")
        .border_style(Style::parse("bright_cyan").unwrap_or_default())
        .header_style(Style::parse("bold magenta").unwrap_or_default());
    table.add_column(Column::new("File").style(Style::parse("cyan").unwrap_or_default()));
    table.add_column(Column::new("Expected").style(Style::parse("dim").unwrap_or_default()));
    table.add_column(Column::new("Actual").style(Style::parse("dim").unwrap_or_default()));
    table.add_column(Column::new("Status"));

    for entry in &result.entries {
        let status_str = if entry.missing {
            "[bold red]MISSING[/]"
        } else if entry.matches {
            "[bold green]MATCH[/]"
        } else {
            "[bold red]MISMATCH[/]"
        };

        let expected_display: Cow<'_, str> = if entry.expected.len() > 18 {
            format!("{}...", &entry.expected[..18]).into()
        } else {
            Cow::Borrowed(&entry.expected)
        };
        let actual_display: Cow<'_, str> = if entry.actual.len() > 18 {
            format!("{}...", &entry.actual[..18]).into()
        } else {
            Cow::Borrowed(&entry.actual)
        };

        table.add_row(Row::new([
            markup(&entry.rel_path),
            markup(&expected_display),
            markup(&actual_display),
            markup(status_str),
        ]));
    }

    let width = console.width();
    let _ = print_renderable(&mut console, &table.render(width));
    let _ = console.print("");

    if result.all_match {
        let panel = Panel::new(markup(
            "[bold green]ALL CHECKSUMS MATCH[/]  --  \
             corruption warning will not appear",
        ))
        .border_style(Style::parse("green").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
    } else {
        let panel = Panel::new(markup(
            "[bold red]MISMATCH FOUND[/]  --  \
             corruption warning will appear",
        ))
        .border_style(Style::parse("red").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
        return Err(eyre!("Checksum mismatch found"));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
//  fix-checksums
// ---------------------------------------------------------------------------

pub fn cmd_fix_checksums() -> Result<()> {
    let mut console = Console::new();
    let paths = detect_paths();
    let (cursor_app, product_json) = require_paths(
        &mut console,
        paths.cursor_app.as_deref(),
        paths.product_json.as_deref(),
    )?;

    let result = Status::new("Fixing checksums...")
        .run(|| {
            integrity::fix_checksums(product_json, cursor_app)
        })?;

    let _ = console.print("");

    for entry in &result.entries {
        match entry.status {
            integrity::FixStatus::Ok => {
                let _ = console.print(&format!(
                    "  [bold green]  OK[/]  {}",
                    entry.rel_path
                ));
            }
            integrity::FixStatus::Updated => {
                let _ = console.print(&format!(
                    "  [bold yellow]UPDT[/]  {}",
                    entry.rel_path
                ));
            }
            integrity::FixStatus::Missing => {
                let _ = console.print(&format!(
                    "  [bold red]MISS[/]  {}",
                    entry.rel_path
                ));
            }
        }
    }

    let _ = console.print("");
    let width = console.width();

    if result.changed_count > 0 {
        let content = format!(
            "[bold green]Updated {} checksum(s).[/]\nRestart Cursor to apply.",
            result.changed_count
        );
        let panel = Panel::new(markup(&content))
            .title("Done")
            .border_style(Style::parse("green").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
    } else {
        let panel = Panel::new(markup("[dim]All checksums already match.[/]"))
            .border_style(Style::parse("dim").unwrap_or_default());
        let _ = print_renderable(&mut console, &panel.render(width));
    }

    Ok(())
}
