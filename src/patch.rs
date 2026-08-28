//! Patch logic for Cursor's CLI and IDE shell-execution library.
//!
//! Each patch is a pure `&str -> (Cow<str>, StepResult)` transform. A
//! [`PatchPlan`] lists the patches for an agent; [`run_patch`] drives them
//! against a file. All patches key off names recovered by [`Discovery`], so
//! they survive Cursor updates that only rename minified identifiers.
//!
//! The exact inserted strings and insertion points here are verified against a
//! live Cursor install and locked by the test suite -- do not change them
//! without re-verifying end to end.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::discovery::{Discovery, NU_LOGIN_MARKER, SHELL_PATH_PATCHED_MARKER};
use crate::integrity::{bak_path, backup, checksums_all_match, restore_from_backup};
use crate::paths::CursorPaths;
use crate::util::re;

// ---------------------------------------------------------------------------
//  Result / status types
// ---------------------------------------------------------------------------

/// Outcome of one patch step.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub name: &'static str,
    pub ok: bool,
    pub skipped: bool,
    pub message: String,
    pub detail: String,
}

impl StepResult {
    pub fn ok(name: &'static str, msg: impl Into<String>) -> Self {
        Self { name, ok: true, skipped: false, message: msg.into(), detail: String::new() }
    }
    pub fn fail(name: &'static str, msg: impl Into<String>) -> Self {
        Self { name, ok: false, skipped: false, message: msg.into(), detail: String::new() }
    }
    pub fn skipped(name: &'static str, msg: impl Into<String>) -> Self {
        Self { name, ok: true, skipped: true, message: msg.into(), detail: String::new() }
    }
    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// Outcome of a full patch (or integrity) operation.
#[derive(Debug)]
pub struct PatchResult {
    pub success: bool,
    pub steps: Vec<StepResult>,
}

impl PatchResult {
    fn failed(step: StepResult) -> Self {
        Self { success: false, steps: vec![step] }
    }
}

/// Patch state of a single agent (CLI or IDE).
#[derive(Default)]
pub struct ComponentStatus {
    pub path: Option<String>,
    pub exists: bool,
    pub backup_exists: bool,
    pub patches: HashMap<String, bool>,
}

/// Integrity-check state.
#[derive(Default)]
pub struct IntegrityStatus {
    pub product_checksums_match: Option<bool>,
}

/// Overall patch status across both agents.
#[derive(Default)]
pub struct PatchStatus {
    pub cli: ComponentStatus,
    pub ide: ComponentStatus,
    pub integrity: IntegrityStatus,
}

/// Restore outcome for a single file.
pub struct RevertFileResult {
    pub filename: String,
    pub restored: bool,
}

/// Restore outcome across all files.
pub struct RevertResult {
    pub files: Vec<RevertFileResult>,
}

// ---------------------------------------------------------------------------
//  Public API
// ---------------------------------------------------------------------------

/// Patch the CLI agent (`index.js`).
pub fn patch_cli_agent(path: &Path, dry_run: bool) -> PatchResult {
    run_patch(path, dry_run, &CLI_PLAN)
}

/// Patch the IDE agent (`main.js`).
pub fn patch_ide_agent(path: &Path, dry_run: bool) -> PatchResult {
    run_patch(path, dry_run, &IDE_PLAN)
}

/// Report patch and integrity status without modifying any files.
pub fn check_status(paths: &CursorPaths) -> PatchStatus {
    let mut status = PatchStatus::default();

    if let Some(cli) = paths.cli_index.as_deref().filter(|p| p.is_file()) {
        status.cli = component_status(cli, CLI_PATCH_NAMES);
    }
    if let Some(ide) = paths.ide_main.as_deref().filter(|p| p.is_file()) {
        status.ide = component_status(ide, IDE_PATCH_NAMES);
    }

    if let (Some(product), Some(app)) = (&paths.product_json, &paths.cursor_app)
        && product.is_file()
    {
        status.integrity.product_checksums_match = checksums_all_match(product, app);
    }

    status
}

/// Restore every patched file from its `.bak` backup.
pub fn revert_all(paths: &CursorPaths) -> RevertResult {
    let targets = [
        paths.cli_index.as_deref(),
        paths.ide_main.as_deref(),
        paths.product_json.as_deref(),
    ];
    let files = targets
        .into_iter()
        .flatten()
        .map(|target| RevertFileResult {
            filename: display_name(target),
            restored: restore_from_backup(target).unwrap_or(false),
        })
        .collect();
    RevertResult { files }
}

// ---------------------------------------------------------------------------
//  Plans
// ---------------------------------------------------------------------------

type PatchFn = for<'a> fn(&'a str, &Discovery) -> (Cow<'a, str>, StepResult);

struct PatchPlan {
    label: &'static str,
    patches: &'static [(&'static str, PatchFn)],
    is_fully_patched: fn(&Discovery) -> bool,
}

const CLI_PLAN: PatchPlan = PatchPlan {
    label: "CLI",
    patches: &[
        ("Nu detection", patch_nu_detection),
        ("System nu detection", patch_system_nu_detection),
        ("Naive case", patch_naive_case),
        ("Shell path fallback", patch_shell_path_fallback),
        ("Nu login", patch_nu_login),
    ],
    is_fully_patched: |d| {
        d.has_nu_detection && d.has_system_nu && d.has_naive_case && d.has_shell_path && d.has_nu_login
    },
};

const IDE_PLAN: PatchPlan = PatchPlan {
    label: "IDE",
    patches: &[
        ("Nu detection", patch_nu_detection),
        ("System nu detection", patch_system_nu_detection),
        ("userTerminalHint", patch_user_terminal_hint),
        ("Shell path fallback", patch_shell_path_fallback),
        ("Nu login", patch_nu_login),
    ],
    is_fully_patched: |d| {
        d.has_nu_detection && d.has_system_nu && d.has_user_terminal_hint && d.has_shell_path && d.has_nu_login
    },
};

// ---------------------------------------------------------------------------
//  Patch driver
// ---------------------------------------------------------------------------

/// Read `path`, apply every patch in `plan`, and write the result back.
///
/// The agent is always restored from its backup first so patches apply to
/// pristine (unpatched) code -- this makes re-running after a Cursor update
/// idempotent even when a stale patched file is on disk.
fn run_patch(path: &Path, dry_run: bool, plan: &PatchPlan) -> PatchResult {
    let live = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return PatchResult::failed(StepResult::fail("Read", read_err(plan, e))),
    };

    // Fast path: already fully patched.
    if let Ok(d) = Discovery::analyze(&live)
        && (plan.is_fully_patched)(&d)
    {
        let mut steps = vec![StepResult::ok("Pattern discovery", "Discovered minified names")];
        for &(name, _) in plan.patches {
            steps.push(StepResult::skipped(name, "Already present, skipped"));
        }
        return PatchResult { success: true, steps };
    }
    drop(live);

    if !dry_run {
        if let Err(e) = backup(path) {
            return PatchResult::failed(StepResult::fail("Backup", format!("Failed to back up: {e}")));
        }
        if let Err(e) = restore_from_backup(path) {
            return PatchResult::failed(StepResult::fail("Restore", format!("Failed to restore: {e}")));
        }
    }

    let code = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return PatchResult::failed(StepResult::fail("Read", read_err(plan, e))),
    };
    let discovery = match Discovery::analyze(&code) {
        Ok(d) => d,
        Err(e) => return PatchResult::failed(StepResult::fail("Pattern discovery", e)),
    };

    let mut steps = vec![StepResult::ok("Pattern discovery", "Discovered minified names")
        .with_detail(discovery_detail(&discovery))];

    let mut code = Cow::Borrowed(code.as_str());
    for &(_, patch_fn) in plan.patches {
        let (next, step) = patch_fn(&code, &discovery);
        let ok = step.ok;
        steps.push(step);
        if !ok {
            return PatchResult { success: false, steps };
        }
        if let Cow::Owned(s) = next {
            code = Cow::Owned(s);
        }
    }

    if dry_run {
        steps.push(StepResult::skipped("Write", format!("Would write: {}", display_name(path))));
    } else if let Err(e) = fs::write(path, code.as_bytes()) {
        steps.push(StepResult::fail("Write", format!("Failed to write {} agent: {e}", plan.label)));
        return PatchResult { success: false, steps };
    } else {
        steps.push(StepResult::ok("Write", format!("Written: {}", display_name(path))));
    }

    PatchResult { success: true, steps }
}

// ---------------------------------------------------------------------------
//  Patches: shell-type detection
// ---------------------------------------------------------------------------

/// Insert a hint-level `.includes("nu")?<enum>.Naive:` **before** the
/// PowerShell hint check (anything after it is unreachable on Windows).
fn patch_nu_detection<'a>(code: &'a str, d: &Discovery) -> (Cow<'a, str>, StepResult) {
    const NAME: &str = "Nu detection";
    if d.has_nu_detection {
        return skip(code, NAME);
    }

    let zsh = format!(r#"{}.includes("zsh")"#, d.hint_var);
    let Some(zsh_idx) = code.find(&zsh) else {
        return fail(code, NAME, "Cannot locate detectShellType region");
    };

    let region_end = (zsh_idx + 2000).min(code.len());
    let pwsh = format!(r#"{}.includes("pwsh")"#, d.hint_var);
    let Some(rel) = code[zsh_idx..region_end].find(&pwsh) else {
        return fail(code, NAME, format!("Cannot find {pwsh} in detectShellType"));
    };

    let at = zsh_idx + rel;
    let insertion = format!(r#"{}.includes("nu")?{}.Naive:"#, d.hint_var, d.enum_var);
    if code[at..].starts_with(&insertion) {
        return skip(code, NAME);
    }
    let out = splice(code, at, &insertion);
    let detail = insertion_detail(&insertion, &out, at, 40, 60);
    ok_owned(out, NAME, "Inserted before PowerShell check", detail)
}

/// Insert a system-level `<cmdExists>("nu")?<enum>.Naive:` right after the
/// first (hint-based) `?<enum>.PowerShell:` arm, so it runs before the
/// always-true system PowerShell check on Windows.
fn patch_system_nu_detection<'a>(code: &'a str, d: &Discovery) -> (Cow<'a, str>, StepResult) {
    const NAME: &str = "System nu detection";
    if d.has_system_nu {
        return skip(code, NAME);
    }
    let Some(cmd_exists) = &d.cmd_exists_fn else {
        return fail(code, NAME, "Cannot find commandExists function");
    };

    let zsh = format!(r#"{}.includes("zsh")"#, d.hint_var);
    let Some(zsh_idx) = code.find(&zsh) else {
        return fail(code, NAME, "Cannot find detectShellType region");
    };
    let marker = format!("?{}.PowerShell:", d.enum_var);
    let Some(rel) = code[zsh_idx..].find(&marker) else {
        return fail(code, NAME, format!("Cannot find `{marker}` in detectShellType"));
    };

    let at = zsh_idx + rel + marker.len();
    let insertion = format!(r#"{cmd_exists}("nu")?{}.Naive:"#, d.enum_var);
    let out = splice(code, at, &insertion);
    let detail = insertion_detail(&insertion, &out, at, 40, 40);
    ok_owned(out, NAME, "Inserted PATH-based nu check after hint-based PowerShell", detail)
}

// ---------------------------------------------------------------------------
//  Patches: shell resolution
// ---------------------------------------------------------------------------

/// Wire `userTerminalHint` into the IDE shell resolver (`e?.shell??` gains an
/// `e?.userTerminalHint??` fallback) so the configured terminal profile wins.
fn patch_user_terminal_hint<'a>(code: &'a str, _d: &Discovery) -> (Cow<'a, str>, StepResult) {
    const NAME: &str = "userTerminalHint";
    let Some(caps) = lazy_shell_var(code) else {
        return fail(code, NAME, "Cannot find ?.shell?? pattern");
    };
    let shell_var = caps;
    let find = format!("{shell_var}?.shell??");
    let replace = format!("{shell_var}?.shell??{shell_var}?.userTerminalHint??");
    if code.contains(&replace) {
        return skip(code, NAME);
    }
    let out = code.replacen(&find, &replace, 1);
    let detail = format!("Find:    {find}\nReplace: {replace}");
    ok_owned(out, NAME, format!("{find} -> {replace}"), detail)
}

/// Add `case <enum>.Naive:` to the executor factory, resolving the shell via
/// `userTerminalHint -> findActualExecutable("nu") -> env`.
///
/// Anchored on the factory `switch(<fn>(<opts>?.userTerminalHint...))`: the CLI
/// also has a `case <enum>.Zsh:` inside `getShellExecutablePath`, and targeting
/// that one would route the factory to the wrong executor. The returned value
/// mirrors the raw `createNaiveExecutor` shape exactly -- a bare
/// `new <naive>(cwd,{...opts,shell})` with the spread first so the resolved
/// shell wins, and no LazyExecutor wrapper (whose constructor takes a thunk).
fn patch_naive_case<'a>(code: &'a str, d: &Discovery) -> (Cow<'a, str>, StepResult) {
    const NAME: &str = "Naive case";
    if d.has_naive_case {
        return skip_msg(code, NAME, "Already exists, skipped");
    }
    let Some(naive_exec) = &d.naive_exec else {
        return fail(code, NAME, "Cannot find NaiveTerminalExecutor class");
    };
    let Some(find_exec) = &d.find_exec_call else {
        return fail(code, NAME, "Cannot find findActualExecutable call pattern");
    };

    let re_switch = re(r"switch\(\w+\((\w+)\?\.userTerminalHint").ok();
    let Some(caps) = re_switch.as_ref().and_then(|r| r.captures(code).ok().flatten()) else {
        return fail(code, NAME, "Cannot find executor factory switch");
    };
    let opts = caps.get(1).map(|m| m.as_str()).unwrap_or("t").to_string();
    let switch_start = caps.get(0).map(|m| m.start()).unwrap_or(0);

    let naive_case = format!(
        "case {ev}.Naive:{{const _np={fex}(\"nu\",[]).cmd;\
         return new {naive_exec}(process.cwd(),{{...{opts},\
         shell:{opts}?.userTerminalHint\
         ||(_np!==\"nu\"?_np:void 0)\
         ||{opts}?.shell||process.env.SHELL||\"/bin/sh\"}})}}",
        ev = d.enum_var,
        fex = find_exec,
    );

    let zsh_case = format!("case {}.Zsh:", d.enum_var);
    let Some(rel) = code[switch_start..].find(&zsh_case) else {
        return fail(code, NAME, "Cannot find executor factory");
    };
    let search_from = switch_start + rel;

    let default_idx = code[search_from..].find("default:").filter(|&d| d < 10000);
    let zsh_light_idx = code[search_from..].find(&format!("case {}.ZshLight:", d.enum_var));
    let (at, label) = match (default_idx, zsh_light_idx) {
        (Some(di), _) => (search_from + di, "before default:"),
        (None, Some(zli)) => (search_from + zli, "before ZshLight"),
        (None, None) => return fail(code, NAME, "Cannot find insertion point for Naive case"),
    };

    let out = splice(code, at, &naive_case);
    ok_owned(out, NAME, format!("Inserted {label}"), format!("Insertion: {naive_case}"))
}

/// Fix `getShellExecutablePath` for `ShellType.Naive`: add a `case` that
/// resolves nu from PATH, and make the `default:` return PowerShell on Windows
/// instead of the non-existent `/bin/sh`. Present in both CLI and IDE.
fn patch_shell_path_fallback<'a>(code: &'a str, d: &Discovery) -> (Cow<'a, str>, StepResult) {
    const NAME: &str = "Shell path fallback";
    if code.contains(SHELL_PATH_PATCHED_MARKER) {
        return skip_msg(code, NAME, "Already patched, skipped");
    }
    let Some(find_exec) = &d.find_exec_call else {
        return fail(code, NAME, "Cannot find findActualExecutable call pattern");
    };

    let find = r#"default:return process.env.SHELL||"/bin/sh""#;
    let Some(idx) = code.find(find) else {
        return fail(code, NAME, format!("Cannot find `{find}` pattern"));
    };

    let region = &code[idx.saturating_sub(500)..idx];
    if !region.contains("findActualExecutable") && !region.contains("PowerShell") {
        return fail(code, NAME, "Found pattern but not in getShellExecutablePath context");
    }
    let ps_re = re(&format!(r"case {}\.PowerShell:return (\w+)\(\)", d.enum_var)).ok();
    let Some(ps_fn) = ps_re
        .and_then(|r| r.captures(region).ok().flatten())
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
    else {
        return fail(code, NAME, "Cannot discover PowerShell path resolver function name");
    };

    let replace = format!(
        "case {ev}.Naive:{{const _np={fex}(\"nu\",[]).cmd;\
         if(_np!==\"nu\")return _np}}\
         default:return process.env.SHELL||(\"win32\"===process.platform?{ps_fn}():\"/bin/sh\")",
        ev = d.enum_var,
        fex = find_exec,
    );
    let out = code.replacen(find, &replace, 1);
    let detail = format!("Find:    {find}\nReplace: {replace}");
    ok_owned(out, NAME, "Added Naive case with PATH-based nu discovery", detail)
}

/// Prepend `-l` when the Naive executor spawns `nu`, so `env.nu`/`config.nu`
/// load. Other shells keep plain `shell -c`.
fn patch_nu_login<'a>(code: &'a str, _d: &Discovery) -> (Cow<'a, str>, StepResult) {
    const NAME: &str = "Nu login";
    if code.contains(NU_LOGIN_MARKER) {
        return skip(code, NAME);
    }
    if !code.contains(NAIVE_SPAWN_ARGS) {
        return fail(code, NAME, "Cannot find Naive spawn `[...shellArgs,\"-c\"]`");
    }
    let out = code.replacen(NAIVE_SPAWN_ARGS, NAIVE_SPAWN_ARGS_PATCHED, 1);
    let detail = format!("Find:    {NAIVE_SPAWN_ARGS}\nReplace: {NAIVE_SPAWN_ARGS_PATCHED}");
    ok_owned(out, NAME, "Naive spawn prepends -l when shell is nu", detail)
}

const NAIVE_SPAWN_ARGS: &str = r#"[...this.options?.shellArgs??[],"-c","#;
const NAIVE_SPAWN_ARGS_PATCHED: &str = concat!(
    r#"[...this.options?.shellArgs??[],.../(?:^|[\\/])nu(?:\.exe)?$/i.test("#,
    r#"this.options?.shell||process.env.SHELL||"/bin/sh")?["-l"]:[],"-c","#,
);

// ---------------------------------------------------------------------------
//  Status helpers
// ---------------------------------------------------------------------------

/// A patch's display name paired with the flag that detects it.
type PatchFlag = (&'static str, fn(&Discovery) -> bool);

fn component_status(path: &Path, names: &[PatchFlag]) -> ComponentStatus {
    let mut status = ComponentStatus {
        exists: true,
        path: Some(display_name(path)),
        backup_exists: bak_path(path).exists(),
        patches: HashMap::new(),
    };
    if let Ok(code) = fs::read_to_string(path)
        && let Ok(d) = Discovery::analyze(&code)
    {
        for &(name, flag) in names {
            status.patches.insert(name.to_string(), flag(&d));
        }
    }
    status
}

const CLI_PATCH_NAMES: &[PatchFlag] = &[
    ("Nu detection", |d| d.has_nu_detection),
    ("System nu detection", |d| d.has_system_nu),
    ("Naive case", |d| d.has_naive_case),
    ("Shell path fallback", |d| d.has_shell_path),
    ("Nu login", |d| d.has_nu_login),
];

const IDE_PATCH_NAMES: &[PatchFlag] = &[
    ("Nu detection", |d| d.has_nu_detection),
    ("System nu detection", |d| d.has_system_nu),
    ("userTerminalHint", |d| d.has_user_terminal_hint),
    ("Shell path fallback", |d| d.has_shell_path),
    ("Nu login", |d| d.has_nu_login),
];

// ---------------------------------------------------------------------------
//  Small helpers
// ---------------------------------------------------------------------------

fn splice(code: &str, at: usize, insertion: &str) -> String {
    let mut out = String::with_capacity(code.len() + insertion.len());
    out.push_str(&code[..at]);
    out.push_str(insertion);
    out.push_str(&code[at..]);
    out
}

fn insertion_detail(insertion: &str, out: &str, at: usize, before: usize, after: usize) -> String {
    let start = at.saturating_sub(before);
    let end = (at + insertion.len() + after).min(out.len());
    format!("Insertion: {insertion}\nContext:   ...{}...", &out[start..end])
}

fn discovery_detail(d: &Discovery) -> String {
    format!(
        "hint={} enum={} lazy={:?} naive={:?} cmd_exists={:?} find_exec={:?} \
         uth={} sys_nu={} nu_login={}",
        d.hint_var, d.enum_var, d.lazy_exec, d.naive_exec, d.cmd_exists_fn,
        d.find_exec_call, d.has_user_terminal_hint, d.has_system_nu, d.has_nu_login,
    )
}

fn lazy_shell_var(code: &str) -> Option<String> {
    crate::util::lazy_re!(r"(\w+)\?\.shell\?\?")
        .captures(code)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn read_err(plan: &PatchPlan, e: std::io::Error) -> String {
    format!("Failed to read {} agent: {e}", plan.label)
}

// Result-building shorthands for the patch functions.

fn skip<'a>(code: &'a str, name: &'static str) -> (Cow<'a, str>, StepResult) {
    (Cow::Borrowed(code), StepResult::skipped(name, "Already present, skipped"))
}
fn skip_msg<'a>(code: &'a str, name: &'static str, msg: &'static str) -> (Cow<'a, str>, StepResult) {
    (Cow::Borrowed(code), StepResult::skipped(name, msg))
}
fn fail<'a>(code: &'a str, name: &'static str, msg: impl Into<String>) -> (Cow<'a, str>, StepResult) {
    (Cow::Borrowed(code), StepResult::fail(name, msg))
}
fn ok_owned(
    out: String,
    name: &'static str,
    msg: impl Into<String>,
    detail: String,
) -> (Cow<'static, str>, StepResult) {
    (Cow::Owned(out), StepResult::ok(name, msg).with_detail(detail))
}

// ---------------------------------------------------------------------------
//  Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const CLI_RAW: &str = include_str!("../tests/fixtures/cli_raw.js");
    const IDE_RAW: &str = include_str!("../tests/fixtures/ide_raw.js");

    /// Apply a plan to a string (no file IO) and return the result plus steps.
    fn apply(code: &str, plan: &PatchPlan) -> (String, Vec<StepResult>) {
        let d = Discovery::analyze(code).expect("discovery");
        let mut out = Cow::Borrowed(code);
        let mut steps = Vec::new();
        for &(_, patch_fn) in plan.patches {
            let (next, step) = patch_fn(&out, &d);
            steps.push(step);
            if let Cow::Owned(s) = next {
                out = Cow::Owned(s);
            }
        }
        (out.into_owned(), steps)
    }

    fn all_ok(steps: &[StepResult]) -> bool {
        steps.iter().all(|s| s.ok)
    }

    fn braces_balanced(s: &str) -> bool {
        let mut depth: i64 = 0;
        for c in s.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                return false;
            }
        }
        depth == 0
    }

    #[test]
    fn cli_plan_inserts_all_patches() {
        let (out, steps) = apply(CLI_RAW, &CLI_PLAN);
        assert!(all_ok(&steps), "some CLI patches failed: {steps:?}");

        // Hint-level nu detection, before the PowerShell hint arm.
        assert!(out.contains(r#"t.includes("nu")?Ce.Naive:t.includes("pwsh")"#));
        // System-level nu detection, right after the first hint PowerShell arm.
        assert!(out.contains(r#"?Ce.PowerShell:Ft("nu")?Ce.Naive:"#));
        // Executor-factory Naive case: bare executor, spread first, shell last.
        assert!(out.contains(
            r#"case Ce.Naive:{const _np=(0,s.E)("nu",[]).cmd;return new at(process.cwd(),{...t,shell:t?.userTerminalHint||(_np!=="nu"?_np:void 0)||t?.shell||process.env.SHELL||"/bin/sh"})}"#
        ));
        // getShellExecutablePath fallback with Windows-safe default.
        assert!(out.contains(
            r#"case Ce.Naive:{const _np=(0,s.E)("nu",[]).cmd;if(_np!=="nu")return _np}default:return process.env.SHELL||("win32"===process.platform?rt():"/bin/sh")"#
        ));
        // Nu login flag on the Naive spawn.
        assert!(out.contains(r#"/(?:^|[\\/])nu(?:\.exe)?$/i.test("#));
        assert!(braces_balanced(&out), "unbalanced braces after CLI patch");
    }

    #[test]
    fn ide_plan_inserts_all_patches() {
        let (out, steps) = apply(IDE_RAW, &IDE_PLAN);
        assert!(all_ok(&steps), "some IDE patches failed: {steps:?}");

        assert!(out.contains(r#"t.includes("nu")?ne.Naive:t.includes("pwsh")"#));
        assert!(out.contains(r#"?ne.PowerShell:ct("nu")?ne.Naive:"#));
        // userTerminalHint wired into the shell resolver.
        assert!(out.contains("e?.shell??e?.userTerminalHint??"));
        // getShellExecutablePath fallback resolves via Pe() on Windows.
        assert!(out.contains(
            r#"case ne.Naive:{const _np=(0,i.E)("nu",[]).cmd;if(_np!=="nu")return _np}default:return process.env.SHELL||("win32"===process.platform?Pe():"/bin/sh")"#
        ));
        assert!(out.contains(r#"/(?:^|[\\/])nu(?:\.exe)?$/i.test("#));
        // IDE plan must NOT add an executor-factory Naive case.
        assert!(!out.contains(r#"return new Oe(process.cwd(),{...t,shell:"#));
        assert!(braces_balanced(&out), "unbalanced braces after IDE patch");
    }

    /// The executor-factory Naive case must land in the factory (`qt`), not in
    /// `getShellExecutablePath` (`Ot`), which also has a `case Ce.Zsh:`.
    #[test]
    fn cli_naive_case_targets_factory_not_get_shell_path() {
        let (out, _) = apply(CLI_RAW, &CLI_PLAN);
        let ot = &out[out.find("function Ot").unwrap()..out.find("function Ut").unwrap()];
        assert!(
            !ot.contains("return new at(process.cwd()"),
            "factory Naive case leaked into getShellExecutablePath"
        );
        let qt = &out[out.find("function qt").unwrap()..];
        assert!(qt.contains("case Ce.Naive:{const _np=(0,s.E)(\"nu\",[]).cmd;return new at(process.cwd()"));
    }

    #[test]
    fn cli_plan_is_idempotent() {
        let (once, _) = apply(CLI_RAW, &CLI_PLAN);
        let (twice, steps) = apply(&once, &CLI_PLAN);
        assert_eq!(once, twice, "second CLI apply changed the output");
        assert!(steps.iter().all(|s| s.skipped), "second apply should skip all");
    }

    #[test]
    fn ide_plan_is_idempotent() {
        let (once, _) = apply(IDE_RAW, &IDE_PLAN);
        let (twice, steps) = apply(&once, &IDE_PLAN);
        assert_eq!(once, twice, "second IDE apply changed the output");
        assert!(steps.iter().all(|s| s.skipped), "second apply should skip all");
    }

    #[test]
    fn patched_fixtures_report_fully_patched() {
        let (cli, _) = apply(CLI_RAW, &CLI_PLAN);
        assert!((CLI_PLAN.is_fully_patched)(&Discovery::analyze(&cli).unwrap()));
        let (ide, _) = apply(IDE_RAW, &IDE_PLAN);
        assert!((IDE_PLAN.is_fully_patched)(&Discovery::analyze(&ide).unwrap()));
    }
}

