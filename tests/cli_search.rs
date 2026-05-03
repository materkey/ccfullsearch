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

fn setup_codex_search_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join(".codex/sessions/2026/05/01");
    fs::create_dir_all(&session_dir).unwrap();
    let fixture_content = fs::read_to_string("tests/fixtures/codex_session.jsonl").unwrap();
    fs::write(
        session_dir.join("rollout-2026-05-01T10-00-00-019f0000-0000-7000-8000-000000000001.jsonl"),
        fixture_content,
    )
    .unwrap();
    dir
}

fn setup_codex_subagent_search_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join(".codex/sessions/2026/05/03");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("rollout-2026-05-03T10-00-00-019f0000-0000-7000-8000-000000000001.jsonl"),
        r#"{"timestamp":"2026-05-03T10:00:00Z","type":"session_meta","payload":{"id":"019f0000-0000-7000-8000-000000000001","cwd":"/Users/test/projects/codex-demo","source":"cli"}}
{"timestamp":"2026-05-03T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Parent prompt"}]}}"#,
    )
    .unwrap();
    fs::write(
        session_dir.join("rollout-2026-05-03T10-01-00-019f0000-0000-7000-8000-000000000002.jsonl"),
        r#"{"timestamp":"2026-05-03T10:01:00Z","type":"session_meta","payload":{"id":"019f0000-0000-7000-8000-000000000002","cwd":"/Users/test/projects/codex-demo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"019f0000-0000-7000-8000-000000000001","depth":1,"agent_nickname":"Sagan","agent_role":"default"}}}}}
{"timestamp":"2026-05-03T10:01:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Codex child needle"}]}}"#,
    )
    .unwrap();
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

#[test]
fn search_finds_codex_response_items() {
    let dir = setup_codex_search_dir();
    let search_path = dir.path().join(".codex/sessions");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "codex needle", "--limit", "10"])
        .env("CCFS_SEARCH_PATH", search_path.to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "search should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.is_empty(), "Codex search should produce results");
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(rows.iter().any(|row| row["provider"] == "Codex"));
    assert!(rows
        .iter()
        .any(|row| row["session_id"] == "019f0000-0000-7000-8000-000000000001"));
    assert!(rows.iter().any(|row| row["project"] == "codex-demo"));
    assert!(rows.iter().any(|row| row["content"]
        .as_str()
        .unwrap_or("")
        .contains("codex needle")));
}

#[test]
fn search_resolves_codex_subagent_hits_to_parent_session() {
    let dir = setup_codex_subagent_search_dir();
    let search_path = dir.path().join(".codex/sessions");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "Codex child needle", "--limit", "10"])
        .env("CCFS_SEARCH_PATH", search_path.to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "search should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["session_id"],
        "019f0000-0000-7000-8000-000000000001"
    );
    assert!(rows[0]["file_path"]
        .as_str()
        .unwrap()
        .contains("rollout-2026-05-03T10-00-00-019f0000-0000-7000-8000-000000000001.jsonl"));
    assert!(rows[0]["content"]
        .as_str()
        .unwrap_or("")
        .contains("Codex child needle"));
}
