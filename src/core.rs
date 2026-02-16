//! Core patching logic for Cursor nushell patcher.
//!
//! All business logic lives here. Returns structured types instead of
//! printing. No CLI framework dependency.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::util::{lazy_re, re};

use crate::integrity::{backup, bak_path, checksums_all_match, restore_from_backup, sha256_hex};
use crate::paths::CursorPaths;

/// Safe display name for a path -- falls back to full path if `file_name()` is `None`.
fn display_name(path: &Path) -> Cow<'_, str> {
    path.file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_else(|| path.to_string_lossy())
}

// ---------------------------------------------------------------------------
//  Data types
// ---------------------------------------------------------------------------

/// Result of a single patch step.
#[derive(Debug)]
pub struct StepResult {
    pub name: &'static str,
    pub ok: bool,
    pub message: String,
    pub skipped: bool,
    pub detail: String,
}

impl StepResult {
    pub fn ok(name: &'static str, msg: impl Into<String>) -> Self {
        Self { name, ok: true, message: msg.into(), skipped: false, detail: String::new() }
    }
    pub fn fail(name: &'static str, msg: impl Into<String>) -> Self {
        Self { name, ok: false, message: msg.into(), skipped: false, detail: String::new() }
    }
    pub fn skipped(name: &'static str, msg: impl Into<String>) -> Self {
        Self { name, ok: true, message: msg.into(), skipped: true, detail: String::new() }
    }
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// Result of a patch / integrity operation.
#[derive(Debug)]
pub struct PatchResult {
    pub success: bool,
    pub steps: Vec<StepResult>,
}

/// Status of a single component (CLI or IDE).
#[derive(Default)]
pub struct ComponentStatus {
    pub path: Option<String>,
    pub exists: bool,
    pub backup_exists: bool,
    pub patches: HashMap<String, bool>,
}

/// Status of integrity checks.
#[derive(Default)]
pub struct IntegrityStatus {
    pub ehp_hash_matches: Option<bool>,
    pub product_checksums_match: Option<bool>,
}

/// Overall patch status.
#[derive(Default)]
pub struct PatchStatus {
    pub cli: ComponentStatus,
    pub ide: ComponentStatus,
    pub integrity: IntegrityStatus,
}

/// Result of reverting a single file.
pub struct RevertFileResult {
    pub filename: String,
    pub restored: bool,
}

/// Result of the revert operation.
pub struct RevertResult {
    pub files: Vec<RevertFileResult>,
}

// ---------------------------------------------------------------------------
//  Pattern discovery (internal)
// ---------------------------------------------------------------------------

/// Discovered minified variable names.
struct DiscoveredVars {
    hint_var: String,
    enum_var: String,
    lazy_exec: Option<String>,
    naive_exec: Option<String>,
    /// Minified name of the "command exists on PATH" helper (Ie / Qe).
    cmd_exists_fn: Option<String>,
    /// Full `(0,<mod>.findActualExecutable)` call pattern for constructing
    /// PATH-based shell resolution.
    find_exec_call: Option<String>,
    has_naive_case: bool,
    has_nu_detection: bool,
    /// System-level `<cmdExists>("nu")` check in detectShellType.
    has_system_nu: bool,
    has_user_terminal_hint: bool,
}

/// Discover minified variable names from structural patterns.
fn discover_vars(code: &str) -> Result<DiscoveredVars, String> {
    // 1. hintVar + enumVar from: <hint>.includes("zsh")?<enum>.Zsh
    let re1 = lazy_re!(r#"(\w+)\.includes\("zsh"\)\?(\w+)\.Zsh"#);
    let caps = re1
        .captures(code)
        .ok()
        .flatten()
        .ok_or_else(|| r#"Cannot find includes("zsh")?<enum>.Zsh pattern"#.to_string())?;
    let hint_var = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
    let enum_var = caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
    let enum_escaped = fancy_regex::escape(&enum_var);

    // 2. LazyExecutor from: case <enum>.Zsh:...new <LazyExec>(
    let re2 = re(&format!(r"case\s*{enum_escaped}\.Zsh\s*:.*?new\s+(\w+)\(")).map_err(|e| e.to_string())?;
    let lazy_exec = re2
        .captures(code)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()));

    // 3. NaiveTerminalExecutor -- multiple methods
    let mut naive_exec: Option<String> = None;

    // Method A: from existing case Naive
    let naive_case_str = format!("case {enum_var}.Naive:");
    if code.contains(&naive_case_str) {
        let re_a = re(&format!(
            r"case\s*{enum_escaped}\.Naive\s*:.*?new\s+\w+\(.*?new\s+(\w+)\("
        )).map_err(|e| e.to_string())?;
        if let Some(c) = re_a.captures(code).ok().flatten() {
            naive_exec = Some(c.get(1).map(|m| m.as_str()).unwrap_or("").to_string());
        }
    }

    // Method C: from new <X>(process.cwd(), {shell:
    if naive_exec.is_none() {
        let re_c = lazy_re!(r"new\s+(\w+)\(process\.cwd\(\)\s*,\s*\{shell:");
        if let Some(c) = re_c.captures(code).ok().flatten() {
            naive_exec = Some(c.get(1).map(|m| m.as_str()).unwrap_or("").to_string());
        }
    }

    // Method D: from new <X>(n, {...e, shell:r})
    if naive_exec.is_none() {
        let re_d = lazy_re!(r"new\s+(\w+)\(\w+,\s*\{\.\.\.\w+\s*,\s*shell\s*:");
        if let Some(c) = re_d.captures(code).ok().flatten() {
            naive_exec = Some(c.get(1).map(|m| m.as_str()).unwrap_or("").to_string());
        }
    }

    // 4. cmd_exists function + findActualExecutable call pattern
    //    Pattern: function <name>(<arg>){try{return(0,<mod>.findActualExecutable)(<arg>,[]).cmd!==<arg>}
    let re_cmd = lazy_re!(
        r"function\s+(\w+)\(\w+\)\{try\{return(\(0,\w+\.\w+\))\(\w+,\[\]\)\.cmd!==\w+\}"
    );
    let (cmd_exists_fn, find_exec_call) = match re_cmd.captures(code).ok().flatten() {
        Some(c) => (
            c.get(1).map(|m| m.as_str().to_string()),
            c.get(2).map(|m| m.as_str().to_string()),
        ),
        None => (None, None),
    };

    // 5. State flags
    let has_naive_case = code.contains(&naive_case_str);
    let nu_detection_str = format!(r#".includes("nu")?{enum_var}.Naive"#);
    let has_nu_detection = code.contains(&nu_detection_str);
    let has_system_nu = cmd_exists_fn
        .as_ref()
        .map(|f| code.contains(&format!(r#"{f}("nu")"#)))
        .unwrap_or(false);
    // Match specifically our patch: ?.shell??<var>?.userTerminalHint??
    // The trailing ?? distinguishes this from the original .userTerminalHint
    // usage in the switch(Te(e?.userTerminalHint...)) statement.
    let re_uth = lazy_re!(r"\.shell\?\?\w+\?\.userTerminalHint\?\?");
    let has_user_terminal_hint = re_uth.is_match(code).unwrap_or(false);

    Ok(DiscoveredVars {
        hint_var,
        enum_var,
        lazy_exec,
        naive_exec,
        cmd_exists_fn,
        find_exec_call,
        has_naive_case,
        has_nu_detection,
        has_system_nu,
        has_user_terminal_hint,
    })
}

/// Lightweight patch detection without full discover_vars output.
struct QuickDetect {
    has_nu: bool,
    has_system_nu: bool,
    has_naive_case: bool,
    has_uth: bool,
}

fn quick_detect(code: &str) -> Option<QuickDetect> {
    let re1 = lazy_re!(r#"(\w+)\.includes\("zsh"\)\?(\w+)\.Zsh"#);
    let caps = re1.captures(code).ok().flatten()?;
    let _hint_var = caps.get(1)?.as_str();
    let enum_var = caps.get(2)?.as_str();

    let has_nu = code.contains(&format!(r#".includes("nu")?{enum_var}.Naive"#));
    let has_naive_case = code.contains(&format!("case {enum_var}.Naive:"));
    let re_uth = lazy_re!(r"\.shell\?\?\w+\?\.userTerminalHint\?\?");
    let has_uth = re_uth.is_match(code).unwrap_or(false);

    // System-level nu detection: find cmd_exists function name, check for ("nu")
    let re_cmd = lazy_re!(
        r"function\s+(\w+)\(\w+\)\{try\{return\(0,\w+\.\w+\)\(\w+,\[\]\)\.cmd!==\w+\}"
    );
    let has_system_nu = re_cmd
        .captures(code)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .map(|f| code.contains(&format!(r#"{f}("nu")"#)))
        .unwrap_or(false);

    Some(QuickDetect {
        has_nu,
        has_system_nu,
        has_naive_case,
        has_uth,
    })
}

// ---------------------------------------------------------------------------
//  Patch: Nu detection in detectShellType
// ---------------------------------------------------------------------------

/// Insert `includes("nu")` check before the PowerShell condition.
fn patch_nu_detection<'a>(code: &'a str, v: &DiscoveredVars) -> (Cow<'a, str>, StepResult) {
    if v.has_nu_detection {
        return (
            Cow::Borrowed(code),
            StepResult::skipped("Nu detection", "Already present, skipped"),
        );
    }

    let zsh_pattern = format!(r#"{}.includes("zsh")"#, v.hint_var);
    let zsh_idx = match code.find(&zsh_pattern) {
        Some(idx) => idx,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail("Nu detection", "Cannot locate detectShellType region"),
            );
        }
    };

    let region_end = (zsh_idx + 2000).min(code.len());
    let region = &code[zsh_idx..region_end];

    // Insert BEFORE the PowerShell includes check
    let ps_includes = format!(r#"{}.includes("pwsh")"#, v.hint_var);
    let ps_inc_idx = match region.find(&ps_includes) {
        Some(idx) => idx,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail("Nu detection", format!("Cannot find {ps_includes} in detectShellType")),
            );
        }
    };

    let insert_at = zsh_idx + ps_inc_idx;
    let insertion = format!(
        r#"{}.includes("nu")?{}.Naive:"#,
        v.hint_var, v.enum_var
    );

    if code[insert_at..].starts_with(&insertion) {
        return (
            Cow::Borrowed(code),
            StepResult::skipped("Nu detection", "Already present at insertion point, skipped"),
        );
    }

    let mut new_code = String::with_capacity(code.len() + insertion.len());
    new_code.push_str(&code[..insert_at]);
    new_code.push_str(&insertion);
    new_code.push_str(&code[insert_at..]);

    let ctx_start = insert_at.saturating_sub(40);
    let ctx_end = (insert_at + insertion.len() + 60).min(new_code.len());
    let detail = format!(
        "Insertion: {}\nContext:   ...{}...",
        insertion,
        &new_code[ctx_start..ctx_end]
    );

    (
        Cow::Owned(new_code),
        StepResult::ok("Nu detection", "Inserted before PowerShell check").with_detail(detail),
    )
}

// ---------------------------------------------------------------------------
//  Patch: System-level nu detection in detectShellType (CLI + IDE)
// ---------------------------------------------------------------------------

/// Insert a `<cmdExists>("nu")` system-level check in `detectShellType` so
/// nushell is detected from PATH even when the hint/env doesn't mention it.
///
/// The final fallback chain in `detectShellType` is:
///   `...<cmdExists>("pwsh")||<cmdExists>("powershell")?<enum>.PowerShell:<enum>.Naive}`
///
/// We insert `<cmdExists>("nu")?<enum>.Naive:` before that final `<enum>.Naive}`
/// so nushell-on-PATH wins over the fallback.
fn patch_system_nu_detection<'a>(code: &'a str, v: &DiscoveredVars) -> (Cow<'a, str>, StepResult) {
    if v.has_system_nu {
        return (
            Cow::Borrowed(code),
            StepResult::skipped("System nu detection", "Already present, skipped"),
        );
    }

    let cmd_exists = match &v.cmd_exists_fn {
        Some(f) => f,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail(
                    "System nu detection",
                    "Cannot find commandExists function (Ie/Qe)",
                ),
            );
        }
    };

    // The end of detectShellType is: ...?<enum>.PowerShell:<enum>.Naive}
    // We find the LAST occurrence of this pattern (rfind) to target the
    // final fallback, not an earlier duplicate in the detection chain.
    let tail_pattern = format!(
        "{ev}.PowerShell:{ev}.Naive}}",
        ev = v.enum_var
    );
    let tail_idx = match code.rfind(&tail_pattern) {
        Some(idx) => idx,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail(
                    "System nu detection",
                    format!("Cannot find `{tail_pattern}` at end of detectShellType"),
                ),
            );
        }
    };

    // Insert point: right before <enum>.Naive} (after <enum>.PowerShell:)
    let ps_colon = format!("{}.PowerShell:", v.enum_var);
    let naive_start = tail_idx + ps_colon.len();
    let insertion = format!(
        r#"{cmd_exists}("nu")?{ev}.Naive:"#,
        ev = v.enum_var
    );

    let mut new_code = String::with_capacity(code.len() + insertion.len());
    new_code.push_str(&code[..naive_start]);
    new_code.push_str(&insertion);
    new_code.push_str(&code[naive_start..]);

    let ctx_start = naive_start.saturating_sub(40);
    let ctx_end = (naive_start + insertion.len() + 40).min(new_code.len());
    let detail = format!(
        "Insertion: {}\nContext:   ...{}...",
        insertion,
        &new_code[ctx_start..ctx_end]
    );

    (
        Cow::Owned(new_code),
        StepResult::ok("System nu detection", "Inserted PATH-based nu check before final fallback")
            .with_detail(detail),
    )
}

// ---------------------------------------------------------------------------
//  Patch: userTerminalHint in shell resolution (IDE)
// ---------------------------------------------------------------------------

/// Wire `userTerminalHint` into the shell resolution function so the IDE
/// agent respects the user's configured default terminal.
///
/// The `ce()` function (createNaiveExecutor) resolves the shell path from
/// `opts?.shell`. We insert `opts?.userTerminalHint` as a fallback before
/// the platform default. The PATH-based `findActualExecutable("nu")` in
/// `Se()` provides the safety net when userTerminalHint is unset or broken.
fn patch_user_terminal_hint<'a>(code: &'a str, v: &DiscoveredVars) -> (Cow<'a, str>, StepResult) {
    if v.has_user_terminal_hint {
        return (
            Cow::Borrowed(code),
            StepResult::skipped("userTerminalHint", "Already present, skipped"),
        );
    }

    let re = lazy_re!(r"(\w+)\?\.shell\?\?(?!\w+\?\.userTerminalHint)");
    let caps = match re.captures(code).ok().flatten() {
        Some(c) => c,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail("userTerminalHint", "Cannot find ?.shell?? pattern"),
            );
        }
    };

    let shell_var = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
    let find = format!("{shell_var}?.shell??");
    let replace = format!("{shell_var}?.shell??{shell_var}?.userTerminalHint??");

    let new_code = code.replacen(&find, &replace, 1);
    let detail = format!("Find:    {find}\nReplace: {replace}");

    (
        Cow::Owned(new_code),
        StepResult::ok("userTerminalHint", format!("{find} -> {replace}")).with_detail(detail),
    )
}

// ---------------------------------------------------------------------------
//  Patch: Executor factory Naive case (CLI)
// ---------------------------------------------------------------------------

/// Add `case ShellType.Naive:` to the executor factory.
///
/// The shell path resolution uses PATH-based discovery (`findActualExecutable`)
/// so it works without `$env.SHELL` being set. Falls back to
/// `userTerminalHint` → `findActualExecutable("nu")` → `process.env.SHELL` → `/bin/sh`.
fn patch_naive_case<'a>(code: &'a str, v: &DiscoveredVars) -> (Cow<'a, str>, StepResult) {
    if v.has_naive_case {
        return (
            Cow::Borrowed(code),
            StepResult::skipped("Naive case", "Already exists, skipped"),
        );
    }

    let lazy_exec = match &v.lazy_exec {
        Some(le) => le,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail("Naive case", format!(
                    "Cannot construct Naive case (lazy_exec={:?}, naive_exec={:?})",
                    v.lazy_exec, v.naive_exec
                )),
            );
        }
    };

    let naive_exec = match &v.naive_exec {
        Some(ne) => ne,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail("Naive case", format!(
                    "Cannot construct Naive case (lazy_exec={:?}, naive_exec={:?})",
                    v.lazy_exec, v.naive_exec
                )),
            );
        }
    };

    let find_exec = match &v.find_exec_call {
        Some(f) => f,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail(
                    "Naive case",
                    "Cannot find findActualExecutable call pattern",
                ),
            );
        }
    };

    // Find the opts variable from switch context
    let re_opts = lazy_re!(r"switch\(\w+\((\w+)\?\.userTerminalHint");
    let opts_var = re_opts
        .captures(code)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .unwrap_or_else(|| "t".to_string());

    // PATH-based shell resolution: try userTerminalHint first, then
    // findActualExecutable("nu") for auto-discovery, then env fallbacks.
    // findActualExecutable returns {cmd: "nu"} when NOT found (cmd === input),
    // so we check _np !== "nu" to distinguish found vs not-found.
    let naive_case = format!(
        "case {ev}.Naive:{{const _np={fex}(\"nu\",[]).cmd;\
         return new {lazy_exec}(Promise.resolve(\
         new {naive_exec}(process.cwd(),{{shell:{ov}?.userTerminalHint\
         ||(_np!==\"nu\"?_np:void 0)\
         ||process.env.SHELL||\"/bin/sh\",...{ov}}})))}}",
        ev = v.enum_var,
        fex = find_exec,
        ov = opts_var,
    );

    // Find insertion point: after Zsh case
    let zsh_case = format!("case {}.Zsh:", v.enum_var);
    let search_from = match code.find(&zsh_case) {
        Some(idx) => idx,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail("Naive case", "Cannot find executor factory"),
            );
        }
    };

    // Try before default:
    let default_idx = code[search_from..].find("default:");
    let zsh_light = format!("case {}.ZshLight:", v.enum_var);
    let zsh_light_idx = code[search_from..].find(&zsh_light);

    let (target_idx, insert_label) =
        if let Some(di) = default_idx.filter(|&d| d < 10000) {
            (search_from + di, "before default:")
        } else if let Some(zli) = zsh_light_idx {
            (search_from + zli, "before ZshLight")
        } else {
            return (
                Cow::Borrowed(code),
                StepResult::fail("Naive case", "Cannot find insertion point for Naive case"),
            );
        };

    let mut new_code = String::with_capacity(code.len() + naive_case.len());
    new_code.push_str(&code[..target_idx]);
    new_code.push_str(&naive_case);
    new_code.push_str(&code[target_idx..]);

    (
        Cow::Owned(new_code),
        StepResult::ok("Naive case", format!("Inserted {insert_label}"))
            .with_detail(format!("Insertion: {naive_case}")),
    )
}

// ---------------------------------------------------------------------------
//  Patch: getShellExecutablePath default fallback (IDE legacy path)
// ---------------------------------------------------------------------------

/// Fix `getShellExecutablePath` (`Se()`) to properly handle `ShellType.Naive`:
///
/// 1. Adds `case <enum>.Naive:` that uses `findActualExecutable("nu")` to
///    resolve the nushell path from PATH. This makes the legacy terminal tool
///    path work (`getSuggestedShell` → `Se(O.Naive)` → real nushell path).
///
/// 2. Fixes the `default:` case to return PowerShell on Windows instead of
///    `/bin/sh` (which doesn't exist on Windows).
fn patch_shell_path_fallback<'a>(code: &'a str, v: &DiscoveredVars) -> (Cow<'a, str>, StepResult) {
    let find_exec = match &v.find_exec_call {
        Some(f) => f,
        None => {
            return (
                Cow::Borrowed(code),
                StepResult::fail(
                    "Shell path fallback",
                    "Cannot find findActualExecutable call pattern",
                ),
            );
        }
    };

    // Detect if already patched (has the Naive case with findActualExecutable("nu"))
    let naive_marker = format!(r#"{}("nu",[])"#, find_exec);
    if code.contains(&naive_marker) {
        return (
            Cow::Borrowed(code),
            StepResult::skipped("Shell path fallback", "Already patched, skipped"),
        );
    }

    // Original: `default:return process.env.SHELL||"/bin/sh"`
    let find = r#"default:return process.env.SHELL||"/bin/sh""#;

    if !code.contains(find) {
        return (
            Cow::Borrowed(code),
            StepResult::fail(
                "Shell path fallback",
                format!("Cannot find `{find}` pattern"),
            ),
        );
    }

    // Verify this is in getShellExecutablePath (near findActualExecutable and ne())
    if let Some(idx) = code.find(find) {
        let region_start = idx.saturating_sub(500);
        let region = &code[region_start..idx];
        if !region.contains("findActualExecutable") && !region.contains("PowerShell") {
            return (
                Cow::Borrowed(code),
                StepResult::fail(
                    "Shell path fallback",
                    "Found pattern but not in getShellExecutablePath context",
                ),
            );
        }
    }

    // Replace with:
    //   case <enum>.Naive: { const _np = findActualExecutable("nu",[]).cmd;
    //                        if (_np !== "nu") return _np }
    //   default: return process.env.SHELL || ("win32" === process.platform ? ne() : "/bin/sh")
    let replace = format!(
        "case {ev}.Naive:{{const _np={fex}(\"nu\",[]).cmd;\
         if(_np!==\"nu\")return _np}}\
         default:return process.env.SHELL||(\"win32\"===process.platform?ne():\"/bin/sh\")",
        ev = v.enum_var,
        fex = find_exec,
    );

    let new_code = code.replacen(find, &replace, 1);
    let detail = format!("Find:    {find}\nReplace: {replace}");

    (
        Cow::Owned(new_code),
        StepResult::ok("Shell path fallback", "Added Naive case with PATH-based nu discovery")
            .with_detail(detail),
    )
}

// ---------------------------------------------------------------------------
//  Shared patch driver
// ---------------------------------------------------------------------------

type PatchFn = for<'a> fn(&'a str, &DiscoveredVars) -> (Cow<'a, str>, StepResult);

struct PatchPlan {
    label: &'static str,
    /// Which patches to apply (in order).
    patches: &'static [(&'static str, PatchFn)],
    /// Whether all required patches are already applied.
    is_fully_patched: fn(&QuickDetect) -> bool,
    /// Restore from backup before patching (IDE needs this, CLI does not).
    restore_before_patch: bool,
}

fn run_patch(path: &Path, dry_run: bool, plan: &PatchPlan) -> PatchResult {
    let mut steps: Vec<StepResult> = Vec::new();
    let fail = |steps| PatchResult { success: false, steps };

    // Read the live file and check if already fully patched.
    let live_code = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return fail(vec![StepResult::fail("Read", format!("Failed to read {} agent: {e}", plan.label))]);
        }
    };
    if let Some(det) = quick_detect(&live_code)
        && (plan.is_fully_patched)(&det)
    {
        steps.push(StepResult::ok("Pattern discovery", "Discovered minified variable names"));
        for &(name, _) in plan.patches {
            steps.push(StepResult::skipped(name, "Already present, skipped"));
        }
        return PatchResult { success: true, steps };
    }
    // Drop early so the IDE path can re-read after restore.
    drop(live_code);

    if !dry_run {
        if let Err(e) = backup(path) {
            return fail(vec![StepResult::fail("Backup", format!("Failed to create backup: {e}"))]);
        }
        if plan.restore_before_patch
            && let Err(e) = restore_from_backup(path)
        {
            return fail(vec![StepResult::fail("Restore", format!("Failed to restore from backup: {e}"))]);
        }
    }

    // (Re-)read the file (may have been restored from backup).
    let code = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return fail(vec![StepResult::fail("Read", format!("Failed to read {} agent: {e}", plan.label))]);
        }
    };
    let v = match discover_vars(&code) {
        Ok(v) => v,
        Err(err) => {
            return fail(vec![StepResult::fail("Pattern discovery", err)]);
        }
    };

    steps.push(StepResult::ok("Pattern discovery", "Discovered minified variable names")
        .with_detail(format!(
            "hint_var={}  enum_var={}  lazy_exec={:?}  naive_exec={:?}  \
             cmd_exists={:?}  find_exec={:?}  has_uth={}  has_sys_nu={}",
            v.hint_var, v.enum_var, v.lazy_exec, v.naive_exec,
            v.cmd_exists_fn, v.find_exec_call,
            v.has_user_terminal_hint, v.has_system_nu,
        )));

    // Apply each patch in order.
    let mut code = Cow::Borrowed(code.as_str());
    for &(_name, patch_fn) in plan.patches {
        let (new_code, step) = patch_fn(&code, &v);
        let ok = step.ok;
        steps.push(step);
        if !ok {
            return PatchResult { success: false, steps };
        }
        code = match new_code {
            Cow::Borrowed(_) => code,
            Cow::Owned(s) => Cow::Owned(s),
        };
    }

    if !dry_run {
        if let Err(e) = fs::write(path, code.as_bytes()) {
            steps.push(StepResult::fail("Write", format!("Failed to write {} agent: {e}", plan.label)));
            return PatchResult { success: false, steps };
        }
        steps.push(StepResult::ok("Write", format!("Written: {}", display_name(path))));
    } else {
        steps.push(StepResult::skipped("Write", format!("Would write: {}", display_name(path))));
    }

    PatchResult { success: true, steps }
}

// ---------------------------------------------------------------------------
//  Public API -- Patch CLI agent
// ---------------------------------------------------------------------------

const CLI_PLAN: PatchPlan = PatchPlan {
    label: "CLI",
    patches: &[
        ("Nu detection", patch_nu_detection),
        ("System nu detection", patch_system_nu_detection),
        ("Naive case", patch_naive_case),
    ],
    is_fully_patched: |d| d.has_nu && d.has_system_nu && d.has_naive_case,
    restore_before_patch: false,
};

/// Patch the CLI agent file. Applies nu detection and Naive executor case.
pub fn patch_cli_agent(path: &Path, dry_run: bool) -> PatchResult {
    run_patch(path, dry_run, &CLI_PLAN)
}

// ---------------------------------------------------------------------------
//  Public API -- Patch IDE agent
// ---------------------------------------------------------------------------

const IDE_PLAN: PatchPlan = PatchPlan {
    label: "IDE",
    patches: &[
        ("Nu detection", patch_nu_detection),
        ("System nu detection", patch_system_nu_detection),
        ("userTerminalHint", patch_user_terminal_hint),
        ("Shell path fallback", patch_shell_path_fallback),
    ],
    is_fully_patched: |d| d.has_nu && d.has_system_nu && d.has_uth,
    restore_before_patch: true,
};

/// Patch the IDE agent file. Applies nu detection and userTerminalHint wiring.
pub fn patch_ide_agent(path: &Path, dry_run: bool) -> PatchResult {
    run_patch(path, dry_run, &IDE_PLAN)
}

// ---------------------------------------------------------------------------
//  Public API -- Check status
// ---------------------------------------------------------------------------

/// Return the current patch / integrity status without modifying files.
pub fn check_status(paths: &CursorPaths) -> PatchStatus {
    let mut status = PatchStatus::default();

    // CLI
    if let Some(cli_index) = &paths.cli_index
        && cli_index.is_file()
    {
        status.cli.exists = true;
        status.cli.path = Some(display_name(cli_index).into_owned());
        status.cli.backup_exists = bak_path(cli_index).exists();

        if let Ok(code) = fs::read_to_string(cli_index)
            && let Some(det) = quick_detect(&code)
        {
            status.cli.patches.insert("Nu detection".into(), det.has_nu);
            status.cli.patches.insert("System nu detection".into(), det.has_system_nu);
            status.cli.patches.insert("Naive case".into(), det.has_naive_case);
        }
    }

    // IDE
    if let Some(ide_main) = &paths.ide_main
        && ide_main.is_file()
    {
        status.ide.exists = true;
        status.ide.path = Some(display_name(ide_main).into_owned());
        status.ide.backup_exists = bak_path(ide_main).exists();

        if let Ok(code) = fs::read_to_string(ide_main)
            && let Some(det) = quick_detect(&code)
        {
            status.ide.patches.insert("Nu detection".into(), det.has_nu);
            status.ide.patches.insert("System nu detection".into(), det.has_system_nu);
            status.ide.patches.insert("userTerminalHint".into(), det.has_uth);
        }
    }

    // Integrity
    if let (Some(ide_main), Some(ehp)) = (&paths.ide_main, &paths.ehp)
        && ide_main.is_file() && ehp.is_file()
        && let (Ok(main_hash), Ok(ehp_code)) =
            (sha256_hex(ide_main), fs::read_to_string(ehp))
    {
        status.integrity.ehp_hash_matches = Some(ehp_code.contains(&main_hash));
    }

    if let (Some(product_json), Some(cursor_app)) = (&paths.product_json, &paths.cursor_app)
        && product_json.is_file()
    {
        status.integrity.product_checksums_match =
            checksums_all_match(product_json, cursor_app);
    }

    status
}

// ---------------------------------------------------------------------------
//  Public API -- Revert all
// ---------------------------------------------------------------------------

/// Restore all patched files from their `.bak` backups.
pub fn revert_all(paths: &CursorPaths) -> RevertResult {
    let mut result = RevertResult { files: vec![] };

    let targets: [Option<&Path>; 4] = [
        paths.cli_index.as_deref(),
        paths.ide_main.as_deref(),
        paths.ehp.as_deref(),
        paths.product_json.as_deref(),
    ];

    for target in targets.into_iter().flatten() {
        let restored = restore_from_backup(target).unwrap_or(false);
        result.files.push(RevertFileResult {
            filename: display_name(target).into_owned(),
            restored,
        });
    }

    result
}
