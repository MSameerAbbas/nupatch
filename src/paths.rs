//! Cross-platform detection of Cursor installation paths.

use std::env;
use std::path::{Path, PathBuf};

/// Resolve `%LOCALAPPDATA%`, falling back to `%USERPROFILE%\AppData\Local`.
fn local_app_data() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE")
            .map(|p| PathBuf::from(p).join("AppData").join("Local")))
}

/// Resolved paths for the Cursor installation.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct CursorPaths {
    pub cursor_app: Option<PathBuf>,
    pub cli_agent_dir: Option<PathBuf>,
    pub cli_index: Option<PathBuf>,
    pub ide_main: Option<PathBuf>,
    /// Path to `extensionHostProcess.js` (the extension host bootstrap script).
    pub ehp: Option<PathBuf>,
    pub product_json: Option<PathBuf>,
}

/// Find the Cursor IDE installation directory.
fn detect_cursor_app() -> Option<PathBuf> {
    let is_candidate = |p: &Path| p.join("product.json").is_file();

    if cfg!(target_os = "windows") {
        if let Some(local) = local_app_data() {
            let p = local
                .join("Programs")
                .join("cursor")
                .join("resources")
                .join("app");
            if is_candidate(&p) {
                return Some(p);
            }
        }
    } else if cfg!(target_os = "macos") {
        let p = PathBuf::from("/Applications/Cursor.app/Contents/Resources/app");
        if is_candidate(&p) {
            return Some(p);
        }
        if let Ok(home) = env::var("HOME") {
            let p = Path::new(&home)
                .join("Applications")
                .join("Cursor.app")
                .join("Contents")
                .join("Resources")
                .join("app");
            if is_candidate(&p) {
                return Some(p);
            }
        }
    } else {
        let p = PathBuf::from("/opt/Cursor/resources/app");
        if is_candidate(&p) {
            return Some(p);
        }
        let p = PathBuf::from("/usr/share/cursor/resources/app");
        if is_candidate(&p) {
            return Some(p);
        }
        if let Ok(home) = env::var("HOME") {
            let p = Path::new(&home)
                .join(".local")
                .join("share")
                .join("cursor")
                .join("resources")
                .join("app");
            if is_candidate(&p) {
                return Some(p);
            }
        }
    }

    None
}

/// Find the Cursor CLI agent versions directory.
fn detect_cli_agent_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        if let Some(local) = local_app_data() {
            let p = local
                .join("cursor-agent")
                .join("versions");
            if p.is_dir() {
                return Some(p);
            }
        }
    } else if let Ok(home) = env::var("HOME") {
        let p = Path::new(&home)
            .join(".cursor-agent")
            .join("versions");
        if p.is_dir() {
            return Some(p);
        }
    }

    None
}

/// Find the latest CLI agent index.js.
fn find_cli_index(cli_dir: &Path) -> Option<PathBuf> {
    if !cli_dir.is_dir() {
        return None;
    }

    let index = std::fs::read_dir(cli_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .max_by(|a, b| {
            let mtime = |p: &PathBuf| p.metadata().and_then(|m| m.modified()).ok();
            mtime(a).cmp(&mtime(b))
        })?
        .join("index.js");
    if index.is_file() {
        Some(index)
    } else {
        None
    }
}

/// Detect all Cursor-related paths on this system.
pub fn detect_paths() -> CursorPaths {
    let cursor_app = detect_cursor_app();
    let cli_agent_dir = detect_cli_agent_dir();
    let cli_index = cli_agent_dir
        .as_ref()
        .and_then(|d| find_cli_index(d));

    let (ide_main, ehp, product_json) = if let Some(ref app) = cursor_app {
        let ide = app
            .join("extensions")
            .join("cursor-agent-exec")
            .join("dist")
            .join("main.js");
        let e = app
            .join("out")
            .join("vs")
            .join("workbench")
            .join("api")
            .join("node")
            .join("extensionHostProcess.js");
        let pj = app.join("product.json");

        (
            if ide.is_file() { Some(ide) } else { None },
            if e.is_file() { Some(e) } else { None },
            if pj.is_file() { Some(pj) } else { None },
        )
    } else {
        (None, None, None)
    };

    CursorPaths {
        cursor_app,
        cli_agent_dir,
        cli_index,
        ide_main,
        ehp,
        product_json,
    }
}
