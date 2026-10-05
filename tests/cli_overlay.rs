#![allow(deprecated)]
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn overlay_flag_appears_in_help() {
    Command::cargo_bin("ccs")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--overlay"));
}

#[test]
fn overlay_flag_description_in_help() {
    Command::cargo_bin("ccs")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Overlay mode"));
}

/// Direct-resume invocation against a `codex` stub that fails at once.
#[cfg(unix)]
fn direct_resume_with_failing_codex(dir: &std::path::Path) -> Command {
    use std::os::unix::fs::PermissionsExt;

    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sessions = dir.join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout =
        sessions.join("rollout-2026-09-06T10-00-00-019f0000-0000-7000-8000-000000000001.jsonl");
    std::fs::copy("tests/fixtures/codex_session.jsonl", &rollout).unwrap();

    let mut command = Command::cargo_bin("ccs").unwrap();
    command
        .env("PATH", bin)
        .env("CCFS_SEARCH_PATH", sessions)
        .arg("--tree")
        .arg(rollout)
        .args(["--resume-uuid", "unused"]);
    command
}

#[cfg(unix)]
#[test]
fn codex_bootstrap_failure_when_resuming_in_overlay_then_asks_for_enter_before_exit() {
    let dir = tempfile::tempdir().unwrap();

    direct_resume_with_failing_codex(dir.path())
        .arg("--overlay")
        .write_stdin("\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Codex exited with status 1"))
        .stderr(predicate::str::contains("Press Enter to close"));
}

#[cfg(unix)]
#[test]
fn codex_bootstrap_failure_when_resuming_without_overlay_then_fails_without_prompt() {
    let dir = tempfile::tempdir().unwrap();

    direct_resume_with_failing_codex(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Codex exited with status 1"))
        .stderr(predicate::str::contains("Press Enter").not());
}
