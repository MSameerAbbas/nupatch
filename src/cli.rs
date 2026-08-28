//! Command handlers: thin orchestration over `patch`, `integrity`, and `ui`.

use owo_colors::OwoColorize;

use crate::error::{NupatchError, Result};
use crate::integrity::{self, FixStatus, update_integrity};
use crate::patch::{
    PatchResult, PatchStatus, StepResult, check_status, patch_cli_agent, patch_ide_agent,
    revert_all,
};
use crate::paths::{CursorPaths, detect_paths};
use crate::ui::{self, Tone};

// ---------------------------------------------------------------------------
//  help / version
// ---------------------------------------------------------------------------

pub fn cmd_help(version: &str) {
    ui::blank();
    ui::rule("nupatch");
    ui::blank();
    ui::line("Patch Cursor's CLI and IDE agents to use nushell instead of PowerShell.");
    ui::line(format!("\n{} nupatch {}", "Usage:".dimmed(), "<COMMAND>".cyan()));
    ui::blank();

    let cmd = |name: &str, alias: &str, desc: &str| {
        let head = if alias.is_empty() {
            format!("{}", name.cyan())
        } else {
            format!("{} {}", name.cyan(), format!("({alias})").dimmed())
        };
        vec![head, desc.to_string()]
    };
    ui::line(ui::table(
        &["Command", "Description"],
        &[
            cmd("patch", "", "Apply nushell patches to Cursor agents"),
            cmd("revert", "", "Restore all patched files from backups"),
            cmd("status", "s", "Show current patch status"),
            cmd("verify", "v", "Verify product.json checksums against files on disk"),
            cmd("fix-checksums", "fc", "Recalculate all product.json checksums"),
        ],
    ));
    ui::blank();

    let flag = |name: &str, desc: &str| vec![format!("{}", name.cyan()), desc.to_string()];
    ui::line(ui::table(
        &["Flag", "Description"],
        &[
            flag("--cli-only", "Patch CLI agent only (patch)"),
            flag("--ide-only", "Patch IDE agent only (patch)"),
            flag("-n, --dry-run", "Preview changes without applying (patch)"),
            flag("-h, --help", "Show this help"),
            flag("-V, --version", "Show version"),
        ],
    ));
    ui::line(format!("\n{}", format!("v{version}").dimmed()));
}

pub fn cmd_version(version: &str) {
    ui::line(format!("{} {}", "nupatch".bold(), format!("v{version}").dimmed()));
}

// ---------------------------------------------------------------------------
//  patch
// ---------------------------------------------------------------------------

pub fn cmd_patch(cli_only: bool, ide_only: bool, dry_run: bool) -> Result<()> {
    let paths = detect_paths();
    if !cli_only && paths.cursor_app.is_none() {
        return Err(NupatchError::CursorNotFound);
    }

    ui::blank();
    ui::rule("nupatch");
    if dry_run {
        ui::line(format!("{} -- no files will be modified", "DRY RUN".yellow()));
    }

    let mut ok = true;

    if !ide_only {
        if let Some(cli) = &paths.cli_index {
            let result = ui::with_spinner("Patching CLI agent...", || patch_cli_agent(cli, dry_run));
            show_result("CLI Agent", &result, dry_run);
            ok &= result.success;
        } else {
            ui::line(format!("\n {}", "CLI agent not found, skipping.".dimmed()));
        }
    }

    if !cli_only {
        if let Some(ide) = &paths.ide_main {
            let result = ui::with_spinner("Patching IDE agent...", || patch_ide_agent(ide, dry_run));
            show_result("IDE Agent", &result, dry_run);

            if result.success && !dry_run {
                let integrity = ui::with_spinner("Updating integrity hashes...", || {
                    update_integrity(paths.product_json.as_deref(), paths.cursor_app.as_deref(), dry_run)
                });
                show_result("Integrity Chain", &integrity, false);
                ok &= integrity.success;
            }
            ok &= result.success;
        } else {
            ui::line(format!("\n {}", "IDE agent not found, skipping.".dimmed()));
        }
    }

    ui::blank();
    if ok {
        ui::line(format!("{}", "Patching complete!".green().bold()));
        if !ide_only {
            ui::line(format!(
                "  {} Nushell auto-detected from PATH -- no {} needed",
                "CLI:".cyan(),
                "$env:SHELL".bold(),
            ));
        }
        if !cli_only {
            ui::line(format!("  {} Full quit + relaunch Cursor (not just Reload Window)", "IDE:".cyan()));
        }
        Ok(())
    } else {
        ui::line(format!("{}  See steps above.", "Some patches failed.".red().bold()));
        Err(NupatchError::PatchFailed)
    }
}

// ---------------------------------------------------------------------------
//  revert
// ---------------------------------------------------------------------------

pub fn cmd_revert() -> Result<()> {
    let paths = detect_paths();
    ui::line(format!("\n{}", "This will revert all patches and restore from backups.".yellow()));

    if !ui::confirm("Continue?") {
        ui::line(format!("{}", "Aborted.".dimmed()));
        return Ok(());
    }

    let result = ui::with_spinner("Reverting patches...", || revert_all(&paths));
    ui::blank();
    for f in &result.files {
        if f.restored {
            ui::step("  OK", Tone::Green, "Restored", &f.filename);
        } else {
            ui::step("SKIP", Tone::Dim, "No backup", &f.filename);
        }
    }
    ui::blank();
    ui::line(format!("{}", "Revert complete.".green().bold()));
    ui::line("Restart Cursor to apply.");
    Ok(())
}

// ---------------------------------------------------------------------------
//  status
// ---------------------------------------------------------------------------

pub fn cmd_status() -> Result<()> {
    let paths = detect_paths();
    let st = ui::with_spinner("Checking status...", || check_status(&paths));

    ui::blank();
    render_status_table(&st);
    render_integrity(&st);
    render_overall(&st);
    Ok(())
}

fn render_status_table(st: &PatchStatus) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (label, comp) in [("CLI", &st.cli), ("IDE", &st.ide)] {
        if !comp.exists {
            rows.push(vec![
                label.into(),
                format!("{}", "not found".dimmed()),
                String::new(),
                String::new(),
                String::new(),
            ]);
            continue;
        }

        let mut patches: Vec<(&String, &bool)> = comp.patches.iter().collect();
        patches.sort_by_key(|(n, _)| n.as_str());
        let cell = patches
            .iter()
            .map(|(name, applied)| {
                if **applied {
                    format!("{} {name}", "●".green())
                } else {
                    format!("{} {}", "○".dimmed(), name.dimmed())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let all = !comp.patches.is_empty() && comp.patches.values().all(|&v| v);
        let any = comp.patches.values().any(|&v| v);
        let status = if all {
            format!("{}", "PATCHED".green().bold())
        } else if any {
            format!("{}", "PARTIAL".yellow().bold())
        } else {
            format!("{}", "ORIGINAL".dimmed())
        };

        let backup = if comp.backup_exists {
            format!("{}", "exists".green())
        } else {
            format!("{}", "none".dimmed())
        };

        rows.push(vec![
            label.into(),
            comp.path.clone().unwrap_or_else(|| "N/A".into()),
            backup,
            cell,
            status,
        ]);
    }

    ui::line(ui::table(&["Component", "File", "Backup", "Patches", "Status"], &rows));
}

fn render_integrity(st: &PatchStatus) {
    let Some(m) = st.integrity.product_checksums_match else {
        return;
    };
    ui::blank();
    if m {
        ui::line(format!("{} product.json checksums  {}", "●".green(), "ALL MATCH".green()));
    } else {
        ui::line(format!("{} product.json checksums  {}", "●".red(), "MISMATCH".red()));
    }
}

fn render_overall(st: &PatchStatus) {
    let done = |c: &crate::patch::ComponentStatus| {
        c.exists && !c.patches.is_empty() && c.patches.values().all(|&v| v)
    };
    let overall = match (done(&st.cli), done(&st.ide)) {
        (true, true) => format!("{} (both CLI + IDE)", "PATCHED".green().bold()),
        (false, true) => format!("{} (IDE only)", "PATCHED".green().bold()),
        (true, false) => format!("{} (CLI only)", "PATCHED".green().bold()),
        (false, false) => format!("{} (no patches applied)", "ORIGINAL".dimmed()),
    };
    ui::blank();
    ui::rule(&format!("Overall: {overall}"));
    ui::blank();
}

// ---------------------------------------------------------------------------
//  verify / fix-checksums
// ---------------------------------------------------------------------------

pub fn cmd_verify() -> Result<()> {
    let (cursor_app, product_json) = require_paths(&detect_paths())?;
    let result = ui::with_spinner("Verifying checksums...", || {
        integrity::verify_checksums(&product_json, &cursor_app)
    })?;

    ui::blank();
    let rows = result
        .entries
        .iter()
        .map(|e| {
            let status = if e.missing {
                format!("{}", "MISSING".red().bold())
            } else if e.matches {
                format!("{}", "MATCH".green().bold())
            } else {
                format!("{}", "MISMATCH".red().bold())
            };
            vec![e.rel_path.clone(), truncate(&e.expected), truncate(&e.actual), status]
        })
        .collect::<Vec<_>>();
    ui::line(ui::table(&["File", "Expected", "Actual", "Status"], &rows));
    ui::blank();

    if result.all_match {
        ui::line(format!(
            "{}  --  corruption warning will not appear",
            "ALL CHECKSUMS MATCH".green().bold()
        ));
        Ok(())
    } else {
        ui::line(format!(
            "{}  --  corruption warning will appear",
            "MISMATCH FOUND".red().bold()
        ));
        Err(NupatchError::ChecksumMismatch)
    }
}

pub fn cmd_fix_checksums() -> Result<()> {
    let (cursor_app, product_json) = require_paths(&detect_paths())?;
    let result = ui::with_spinner("Fixing checksums...", || {
        integrity::fix_checksums(&product_json, &cursor_app)
    })?;

    ui::blank();
    for entry in &result.entries {
        match entry.status {
            FixStatus::Ok => ui::step("  OK", Tone::Green, "match", &entry.rel_path),
            FixStatus::Updated => ui::step("UPDT", Tone::Yellow, "updated", &entry.rel_path),
            FixStatus::Missing => ui::step("MISS", Tone::Red, "missing", &entry.rel_path),
        }
    }
    ui::blank();
    if result.changed_count > 0 {
        ui::line(format!(
            "{}",
            format!("Updated {} checksum(s).", result.changed_count).green().bold()
        ));
        ui::line("Restart Cursor to apply.");
    } else {
        ui::line(format!("{}", "All checksums already match.".dimmed()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
//  helpers
// ---------------------------------------------------------------------------

fn show_result(title: &str, result: &PatchResult, dry_run: bool) {
    ui::heading(title);
    for step in &result.steps {
        let (marker, tone) = step_marker(step);
        ui::step(marker, tone, step.name, &step.message);
    }
    if dry_run {
        for step in &result.steps {
            if !step.detail.is_empty() && !step.skipped {
                ui::panel(step.name, &step.detail);
            }
        }
    }
}

fn step_marker(step: &StepResult) -> (&'static str, Tone) {
    if step.skipped {
        ("SKIP", Tone::Dim)
    } else if step.ok {
        ("  OK", Tone::Green)
    } else {
        ("FAIL", Tone::Red)
    }
}

fn require_paths(paths: &CursorPaths) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let cursor_app = paths.cursor_app.clone().ok_or(NupatchError::CursorNotFound)?;
    let product_json = paths.product_json.clone().ok_or(NupatchError::ProductJsonNotFound)?;
    Ok((cursor_app, product_json))
}

fn truncate(s: &str) -> String {
    if s.len() > 18 { format!("{}...", &s[..18]) } else { s.to_string() }
}
