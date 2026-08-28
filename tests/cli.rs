//! CLI surface tests: exercise the built binary and lock the command output.

use assert_cmd::Command;
use predicates::str::contains;

fn nupatch() -> Command {
    Command::cargo_bin("nupatch").unwrap()
}

#[test]
fn version_prints_name_and_version() {
    nupatch().arg("--version").assert().success().stdout(contains("nupatch v"));
}

#[test]
fn help_lists_commands() {
    nupatch()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("Command"))
        .stdout(contains("patch"))
        .stdout(contains("Flag"))
        .stdout(contains("Usage:"));
}

#[test]
fn status_runs_and_exits_zero() {
    // status is read-only and succeeds whether or not Cursor is installed.
    nupatch().arg("status").assert().success();
}

#[test]
fn unknown_command_fails() {
    nupatch().arg("does-not-exist").assert().failure();
}
