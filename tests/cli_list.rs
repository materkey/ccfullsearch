#![allow(deprecated)]
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn setup_list_dir() -> TempDir {
    let dir = TempDir::new().unwrap();

    // Create two session directories with different sessions
    let session1_dir = dir.path().join("-test-project-alpha");
    fs::create_dir_all(&session1_dir).unwrap();
    fs::copy(
        "tests/fixtures/linear_session.jsonl",
        session1_dir.join("session.jsonl"),
    )
    .unwrap();

    let session2_dir = dir.path().join("-test-project-beta");
    fs::create_dir_all(&session2_dir).unwrap();
    fs::copy(
        "tests/fixtures/branched_session.jsonl",
        session2_dir.join("session.jsonl"),
    )
    .unwrap();

    dir
}

fn setup_codex_list_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join(".codex/sessions/2026/05/01");
    fs::create_dir_all(&session_dir).unwrap();
    fs::copy(
        "tests/fixtures/codex_session.jsonl",
        session_dir.join("rollout-2026-05-01T10-00-00-019f0000-0000-7000-8000-000000000001.jsonl"),
    )
    .unwrap();
    fs::write(
        session_dir.join("rollout-2026-05-01T10-01-00-019f0000-0000-7000-8000-000000000002.jsonl"),
        r#"{"timestamp":"2026-05-01T10:01:00Z","type":"session_meta","payload":{"id":"019f0000-0000-7000-8000-000000000002","cwd":"/Users/test/projects/codex-demo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"019f0000-0000-7000-8000-000000000001","depth":1,"agent_nickname":"Sagan","agent_role":"default"}}}}}
{"timestamp":"2026-05-01T10:01:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Subagent-only prompt"}]}}"#,
    )
    .unwrap();
    dir
}

#[test]
fn list_returns_json_lines() {
    let dir = setup_list_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["list"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.is_empty(), "Should list sessions");

    for line in stdout.lines() {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("Each line should be valid JSON: {}\nLine: {}", e, line));
        assert!(parsed.get("session_id").is_some(), "Should have session_id");
        assert!(parsed.get("provider").is_some(), "Should have provider");
        assert!(
            parsed.get("message_count").is_some(),
            "Should have message_count"
        );
        assert!(
            parsed.get("last_active").is_some(),
            "Should have last_active"
        );
    }
}

#[test]
fn list_finds_multiple_sessions() {
    let dir = setup_list_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["list"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert_eq!(line_count, 2, "Should find exactly 2 sessions");
}

#[test]
fn list_respects_limit() {
    let dir = setup_list_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["list", "--limit", "1"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert_eq!(line_count, 1, "Should respect --limit 1");
}

#[test]
fn list_sorted_by_last_active_descending() {
    let dir = setup_list_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["list"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let timestamps: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("last_active")
                        .and_then(|t| t.as_str().map(String::from))
                })
        })
        .collect();

    for window in timestamps.windows(2) {
        assert!(
            window[0] >= window[1],
            "Sessions should be sorted by last_active descending: {} < {}",
            window[0],
            window[1]
        );
    }
}

#[test]
fn list_empty_directory_produces_empty_output() {
    let dir = TempDir::new().unwrap();

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["list"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn list_includes_codex_sessions() {
    let dir = setup_codex_list_dir();
    let search_path = dir.path().join(".codex/sessions");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["list"])
        .env("CCFS_SEARCH_PATH", search_path.to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "list should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["provider"], "Codex");
    assert_eq!(
        rows[0]["session_id"],
        "019f0000-0000-7000-8000-000000000001"
    );
    assert_eq!(rows[0]["project"], "codex-demo");
    assert_eq!(rows[0]["message_count"], 2);
}
