//! End-to-end patching through the public API against temp files.

use std::fs;

use nupatch::patch::{patch_cli_agent, patch_ide_agent};
use tempfile::tempdir;

const CLI_RAW: &str = include_str!("fixtures/cli_raw.js");
const IDE_RAW: &str = include_str!("fixtures/ide_raw.js");

#[test]
fn cli_agent_patches_and_is_idempotent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("index.js");
    fs::write(&path, CLI_RAW).unwrap();

    let first = patch_cli_agent(&path, false);
    assert!(first.success);
    let patched = fs::read_to_string(&path).unwrap();
    assert!(patched.contains(r#"Ft("nu")?Ce.Naive:"#));
    assert!(patched.contains("case Ce.Naive:"));

    // Backup holds the pristine original.
    assert_eq!(fs::read_to_string(path.with_extension("js.bak")).unwrap(), CLI_RAW);

    // Second run changes nothing and reports every patch as skipped.
    let second = patch_cli_agent(&path, false);
    assert!(second.success);
    assert!(second.steps.iter().filter(|s| s.name != "Pattern discovery").all(|s| s.skipped));
    assert_eq!(fs::read_to_string(&path).unwrap(), patched);
}

#[test]
fn ide_agent_patches_successfully() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("main.js");
    fs::write(&path, IDE_RAW).unwrap();

    let result = patch_ide_agent(&path, false);
    assert!(result.success);
    let patched = fs::read_to_string(&path).unwrap();
    assert!(patched.contains("e?.shell??e?.userTerminalHint??"));
    assert!(patched.contains(r#"ct("nu")?ne.Naive:"#));
}

#[test]
fn dry_run_does_not_modify_the_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("index.js");
    fs::write(&path, CLI_RAW).unwrap();

    let result = patch_cli_agent(&path, true);
    assert!(result.success);
    assert_eq!(fs::read_to_string(&path).unwrap(), CLI_RAW);
    assert!(!path.with_extension("js.bak").exists());
}

#[test]
fn patched_output_matches_snapshot() {
    let dir = tempdir().unwrap();
    let cli = dir.path().join("index.js");
    fs::write(&cli, CLI_RAW).unwrap();
    patch_cli_agent(&cli, false);
    insta::assert_snapshot!("cli_patched", fs::read_to_string(&cli).unwrap());

    let ide = dir.path().join("main.js");
    fs::write(&ide, IDE_RAW).unwrap();
    patch_ide_agent(&ide, false);
    insta::assert_snapshot!("ide_patched", fs::read_to_string(&ide).unwrap());
}
