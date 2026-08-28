//! Cross-platform detection of Cursor installation paths.

use std::env;
use std::path::{Path, PathBuf};

/// Resolved paths for the Cursor installation. Any field is `None` when that
/// file or directory is absent.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct CursorPaths {
    pub cursor_app: Option<PathBuf>,
    pub cli_agent_dir: Option<PathBuf>,
    pub cli_index: Option<PathBuf>,
    pub ide_main: Option<PathBuf>,
    pub product_json: Option<PathBuf>,
}

/// Detect all Cursor-related paths on this system.
pub fn detect_paths() -> CursorPaths {
    let cursor_app = detect_cursor_app();
    let cli_agent_dir = detect_cli_agent_dir();
    let cli_index = cli_agent_dir.as_deref().and_then(find_cli_index);

    let file = |p: PathBuf| p.is_file().then_some(p);
    let (ide_main, product_json) = match &cursor_app {
        Some(app) => (
            file(app.join("extensions").join("cursor-agent-exec").join("dist").join("main.js")),
            file(app.join("product.json")),
        ),
        None => (None, None),
    };

    CursorPaths { cursor_app, cli_agent_dir, cli_index, ide_main, product_json }
}

/// Find the Cursor IDE installation directory (the one containing `product.json`).
fn detect_cursor_app() -> Option<PathBuf> {
    let is_app = |p: &Path| p.join("product.json").is_file();

    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        if let Some(local) = local_app_data() {
            candidates.push(local.join("Programs").join("cursor").join("resources").join("app"));
        }
    } else if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/Applications/Cursor.app/Contents/Resources/app"));
        if let Ok(home) = env::var("HOME") {
            candidates.push(Path::new(&home).join("Applications/Cursor.app/Contents/Resources/app"));
        }
    } else {
        candidates.push(PathBuf::from("/opt/Cursor/resources/app"));
        candidates.push(PathBuf::from("/usr/share/cursor/resources/app"));
        if let Ok(home) = env::var("HOME") {
            candidates.push(Path::new(&home).join(".local/share/cursor/resources/app"));
        }
    }
    candidates.into_iter().find(|p| is_app(p))
}

/// Find the Cursor CLI agent `versions` directory.
fn detect_cli_agent_dir() -> Option<PathBuf> {
    let dir = if cfg!(target_os = "windows") {
        local_app_data()?.join("cursor-agent").join("versions")
    } else {
        Path::new(&env::var("HOME").ok()?).join(".cursor-agent").join("versions")
    };
    dir.is_dir().then_some(dir)
}

/// Find the newest CLI agent `index.js` (versions are timestamped directories).
fn find_cli_index(cli_dir: &Path) -> Option<PathBuf> {
    let newest = std::fs::read_dir(cli_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())?
        .join("index.js");
    newest.is_file().then_some(newest)
}

/// Resolve `%LOCALAPPDATA%`, falling back to `%USERPROFILE%\AppData\Local`.
fn local_app_data() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA").map(PathBuf::from).or_else(|| {
        env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Local"))
    })
}
