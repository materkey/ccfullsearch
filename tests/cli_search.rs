#![allow(deprecated)]
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn setup_search_dir(fixture: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("-test-project");
    fs::create_dir_all(&session_dir).unwrap();
    let fixture_content = fs::read_to_string(format!("tests/fixtures/{}", fixture)).unwrap();
    fs::write(session_dir.join("session.jsonl"), fixture_content).unwrap();
    dir
}

#[test]
fn search_finds_matching_content() {
    let dir = setup_search_dir("linear_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "sort"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("sort"));
}

#[test]
fn search_returns_json_lines() {
    let dir = setup_search_dir("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "Python"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    for line in stdout.lines() {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("Each line should be valid JSON: {}\nLine: {}", e, line));
        assert!(parsed.get("session_id").is_some(), "Should have session_id");
        assert!(parsed.get("provider").is_some(), "Should have provider");
        assert!(parsed.get("role").is_some(), "Should have role");
        assert!(parsed.get("content").is_some(), "Should have content");
    }
}

#[test]
fn search_no_matches_produces_empty_output() {
    let dir = setup_search_dir("linear_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "nonexistent_query_xyz"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn search_respects_limit() {
    let dir = setup_search_dir("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "sort", "--limit", "1"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 1,
        "Should respect --limit 1, got {} lines",
        line_count
    );
}

#[test]
fn search_regex_mode() {
    let dir = setup_search_dir("linear_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "sort.*list", "--regex"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("sort"));
}

#[test]
fn search_missing_query_prints_usage() {
    Command::cargo_bin("ccs")
        .unwrap()
        .args(["search"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn search_ansi_content_is_searchable() {
    let dir = setup_search_dir("ansi_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "Compiling"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("Compiling"));
}
