//! Discovery of minified variable names and patched-state detection.
//!
//! Cursor ships heavily minified JS whose identifiers change between versions.
//! Rather than hardcode names, we recover them from stable *structural*
//! patterns (e.g. `X.includes("zsh")?Y.Zsh`). A single [`Discovery::analyze`]
//! pass serves both patching (needs the names) and status checks (needs only
//! the state flags).

use crate::util::{lazy_re, re};

/// The Naive spawn marker that [`Discovery::has_nu_login`] looks for. Also the
/// literal the nu-login patch searches for when producing its output.
pub const NU_LOGIN_MARKER: &str =
    r#"/(?:^|[\\/])nu(?:\.exe)?$/i.test(this.options?.shell"#;

/// Unique marker for the patched `getShellExecutablePath` default case. Emitted
/// only by the shell-path-fallback patch, so it is safe for idempotency checks
/// (unlike a bare `findActualExecutable("nu")` call, which the CLI Naive
/// executor-factory case also emits).
pub const SHELL_PATH_PATCHED_MARKER: &str =
    r#"default:return process.env.SHELL||("win32"===process.platform?"#;

/// Minified names and patched-state flags recovered from an agent file.
#[derive(Debug, Clone)]
pub struct Discovery {
    /// Shell-hint variable, e.g. `t` in `t.includes("zsh")`.
    pub hint_var: String,
    /// `ShellType` enum object, e.g. `Ce` in `Ce.Zsh`.
    pub enum_var: String,
    /// LazyExecutor class name (`case <enum>.Zsh:...new <LazyExec>(`).
    pub lazy_exec: Option<String>,
    /// NaiveTerminalExecutor class name.
    pub naive_exec: Option<String>,
    /// `commandExists`-style PATH probe function name.
    pub cmd_exists_fn: Option<String>,
    /// Full `(0,<mod>.findActualExecutable)` (or bare) call pattern.
    pub find_exec_call: Option<String>,

    /// Hint-level `.includes("nu")?<enum>.Naive` already present.
    pub has_nu_detection: bool,
    /// System-level `<cmdExists>("nu")` check already present.
    pub has_system_nu: bool,
    /// `case <enum>.Naive:` already present in the executor factory.
    pub has_naive_case: bool,
    /// `userTerminalHint` already wired into shell resolution.
    pub has_user_terminal_hint: bool,
    /// `getShellExecutablePath` default fallback already patched.
    pub has_shell_path: bool,
    /// Naive spawn already prepends `-l` for nu.
    pub has_nu_login: bool,
}

impl Discovery {
    /// Recover names and flags from an agent file. Fails only when the file
    /// does not look like Cursor's shell-execution library (no `zsh` arm).
    pub fn analyze(code: &str) -> Result<Discovery, String> {
        let (hint_var, enum_var) = discover_hint_and_enum(code)?;
        let enum_escaped = fancy_regex::escape(&enum_var);

        let lazy_exec = discover_lazy_exec(code, &enum_escaped);
        let naive_exec = discover_naive_exec(code, &enum_var, &enum_escaped);
        let (cmd_exists_fn, find_exec_call) = discover_cmd_exists(code);

        let has_nu_detection = code.contains(&format!(r#".includes("nu")?{enum_var}.Naive"#));
        let has_naive_case = code.contains(&format!("case {enum_var}.Naive:"));
        let has_system_nu = cmd_exists_fn
            .as_ref()
            .map(|f| code.contains(&format!(r#"?{enum_var}.PowerShell:{f}("nu")?{enum_var}.Naive:"#)))
            .unwrap_or(false);
        // The trailing `??` distinguishes our patch from the original
        // `switch(<fn>(<opts>?.userTerminalHint...))` usage.
        let has_user_terminal_hint =
            lazy_re!(r"\.shell\?\?\w+\?\.userTerminalHint\?\?").is_match(code).unwrap_or(false);
        let has_shell_path = code.contains(SHELL_PATH_PATCHED_MARKER);
        let has_nu_login = code.contains(NU_LOGIN_MARKER);

        Ok(Discovery {
            hint_var,
            enum_var,
            lazy_exec,
            naive_exec,
            cmd_exists_fn,
            find_exec_call,
            has_nu_detection,
            has_system_nu,
            has_naive_case,
            has_user_terminal_hint,
            has_shell_path,
            has_nu_login,
        })
    }
}

/// `hintVar` + `enumVar` from `<hint>.includes("zsh")?<enum>.Zsh`.
fn discover_hint_and_enum(code: &str) -> Result<(String, String), String> {
    let re = lazy_re!(r#"(\w+)\.includes\("zsh"\)\?(\w+)\.Zsh"#);
    let caps = re
        .captures(code)
        .ok()
        .flatten()
        .ok_or_else(|| r#"Cannot find includes("zsh")?<enum>.Zsh pattern"#.to_string())?;
    let hint = caps.get(1).map(|m| m.as_str()).unwrap_or_default().to_string();
    let enum_var = caps.get(2).map(|m| m.as_str()).unwrap_or_default().to_string();
    Ok((hint, enum_var))
}

/// LazyExecutor class from `case <enum>.Zsh:...new <LazyExec>(`.
fn discover_lazy_exec(code: &str, enum_escaped: &str) -> Option<String> {
    let re = re(&format!(r"case\s*{enum_escaped}\.Zsh\s*:.*?new\s+(\w+)\(")).ok()?;
    re.captures(code)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// NaiveTerminalExecutor class, tried from three structural anchors.
fn discover_naive_exec(code: &str, enum_var: &str, enum_escaped: &str) -> Option<String> {
    // A: from an existing `case <enum>.Naive:`.
    if code.contains(&format!("case {enum_var}.Naive:"))
        && let Ok(re) =
            re(&format!(r"case\s*{enum_escaped}\.Naive\s*:.*?new\s+\w+\(.*?new\s+(\w+)\("))
        && let Some(c) = re.captures(code).ok().flatten()
    {
        return c.get(1).map(|m| m.as_str().to_string());
    }
    // B: from `new <X>(process.cwd(),{shell:`.
    if let Some(c) = lazy_re!(r"new\s+(\w+)\(process\.cwd\(\)\s*,\s*\{shell:")
        .captures(code)
        .ok()
        .flatten()
    {
        return c.get(1).map(|m| m.as_str().to_string());
    }
    // C: from `new <X>(n,{...e,shell:r})`.
    lazy_re!(r"new\s+(\w+)\(\w+,\s*\{\.\.\.\w+\s*,\s*shell\s*:")
        .captures(code)
        .ok()
        .flatten()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// `commandExists` function name and its `findActualExecutable` call pattern.
///
/// Handles both the wrapped `(0,<mod>.<fn>)(arg,[])` and bare `<fn>(arg,[])`
/// forms.
fn discover_cmd_exists(code: &str) -> (Option<String>, Option<String>) {
    let re = lazy_re!(
        r"function\s+(\w+)\(\w+\)\{try\{return\s*(\(0,\w+\.\w+\)|\w+)\(\w+,\[\]\)\.cmd!==\w+\}"
    );
    match re.captures(code).ok().flatten() {
        Some(c) => (
            c.get(1).map(|m| m.as_str().to_string()),
            c.get(2).map(|m| m.as_str().to_string()),
        ),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI_RAW: &str = include_str!("../tests/fixtures/cli_raw.js");
    const IDE_RAW: &str = include_str!("../tests/fixtures/ide_raw.js");

    #[test]
    fn discovers_cli_names() {
        let d = Discovery::analyze(CLI_RAW).unwrap();
        assert_eq!(d.hint_var, "t");
        assert_eq!(d.enum_var, "Ce");
        assert_eq!(d.lazy_exec.as_deref(), Some("Nt"));
        assert_eq!(d.naive_exec.as_deref(), Some("at"));
        assert_eq!(d.cmd_exists_fn.as_deref(), Some("Ft"));
        assert_eq!(d.find_exec_call.as_deref(), Some("(0,s.E)"));
    }

    #[test]
    fn discovers_ide_names() {
        let d = Discovery::analyze(IDE_RAW).unwrap();
        assert_eq!(d.hint_var, "t");
        assert_eq!(d.enum_var, "ne");
        assert_eq!(d.lazy_exec.as_deref(), Some("Qe"));
        assert_eq!(d.naive_exec.as_deref(), Some("Oe"));
        assert_eq!(d.cmd_exists_fn.as_deref(), Some("ct"));
        assert_eq!(d.find_exec_call.as_deref(), Some("(0,i.E)"));
    }

    #[test]
    fn raw_fixtures_report_no_patches() {
        for raw in [CLI_RAW, IDE_RAW] {
            let d = Discovery::analyze(raw).unwrap();
            assert!(!d.has_nu_detection);
            assert!(!d.has_system_nu);
            assert!(!d.has_naive_case);
            assert!(!d.has_user_terminal_hint);
            assert!(!d.has_shell_path);
            assert!(!d.has_nu_login);
        }
    }

    #[test]
    fn analyze_fails_on_unrelated_code() {
        assert!(Discovery::analyze("function main(){return 42}").is_err());
    }
}

