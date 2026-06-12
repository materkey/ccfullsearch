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

/// Session with a single long message: filler text surrounds a needle phrase
/// so snippet extraction has something to trim on both sides.
fn setup_long_content_search_dir() -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("-test-project");
    fs::create_dir_all(&session_dir).unwrap();

    let filler = "lorem ipsum dolor sit amet consectetur adipiscing elit ".repeat(20);
    let long_text = format!("BEGIN_MARKER {filler} secret needle_42 phrase {filler} END_MARKER");
    let line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{long_text}"}}]}},"uuid":"u1","sessionId":"sess-snippet-001","timestamp":"2025-06-01T10:00:00Z"}}"#
    );
    fs::write(session_dir.join("session.jsonl"), line).unwrap();
    (dir, long_text)
}

fn setup_regex_offset_search_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("-test-project");
    fs::create_dir_all(&session_dir).unwrap();

    let filler = "between ".repeat(80);
    let text = format!("START_MARKER foobar {filler} MATCH_MARKER foo END_MARKER");
    let text_json = serde_json::to_string(&text).unwrap();
    let line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":{text_json}}}]}},"uuid":"u1","sessionId":"sess-regex-offset-001","timestamp":"2025-06-01T10:00:00Z"}}"#
    );
    fs::write(session_dir.join("session.jsonl"), line).unwrap();
    dir
}

fn setup_unicode_expansion_search_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("-test-project");
    fs::create_dir_all(&session_dir).unwrap();

    let text = "\u{0130}\u{00e9}";
    let text_json = serde_json::to_string(text).unwrap();
    let line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":{text_json}}}]}},"uuid":"u1","sessionId":"sess-unicode-001","timestamp":"2025-06-01T10:00:00Z"}}"#
    );
    fs::write(session_dir.join("session.jsonl"), line).unwrap();
    dir
}

/// Split stdout into result rows and the trailing summary row (if any).
/// Result rows have type == "match"; the summary row has type == "summary".
fn split_rows(stdout: &str) -> (Vec<serde_json::Value>, Option<serde_json::Value>) {
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let (results, summary) = match rows.last() {
        Some(last) if last["type"] == "summary" => (
            rows[..rows.len() - 1].to_vec(),
            Some(rows.last().cloned().unwrap()),
        ),
        _ => (rows, None),
    };
    for row in &results {
        assert_eq!(row["type"], "match", "Result rows should have type=match");
    }
    (results, summary)
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
        let _parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("Each line should be valid JSON: {}\nLine: {}", e, line));
    }
    let (results, _summary) = split_rows(&stdout);
    for parsed in &results {
        assert!(parsed.get("session_id").is_some(), "Should have session_id");
        assert!(parsed.get("provider").is_some(), "Should have provider");
        assert!(parsed.get("role").is_some(), "Should have role");
        assert!(parsed.get("content").is_some(), "Should have content");
    }
}

#[test]
fn search_no_matches_emits_summary_only() {
    let dir = setup_search_dir("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "nonexistent_query_xyz"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (results, summary) = split_rows(&stdout);
    assert!(results.is_empty(), "No result rows expected");
    let summary = summary.expect("Summary should be emitted even with zero matches");
    assert_eq!(summary["shown"], 0);
    assert_eq!(summary["total_matches"], 0);
    assert_eq!(summary["sessions"], 0);
    assert_eq!(summary["truncated"], false);
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
    let (results, _summary) = split_rows(&stdout);
    assert!(
        results.len() <= 1,
        "Should respect --limit 1, got {} result rows",
        results.len()
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
    // Pins line_number semantics for Codex rollouts: the first user
    // response_item sits on the file's 2nd line, after session_meta.
    assert!(
        rows.iter()
            .filter(|row| row["type"] == "match")
            .any(|row| row["line_number"] == 2),
        "Codex matches should carry the JSONL line number"
    );
}

#[test]
fn search_emits_summary_line_after_results() {
    let dir = setup_search_dir("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "sort"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (results, summary) = split_rows(&stdout);

    let summary = summary.expect("Last line should be a summary record");
    assert!(!results.is_empty(), "Should have result rows");
    for row in &results {
        assert!(
            row["line_number"].as_u64().unwrap() >= 1,
            "Each match should carry a 1-based line_number"
        );
        assert!(
            row["message_uuid"].is_string(),
            "Each match should carry message_uuid when the JSONL has uuids"
        );
    }
    assert_eq!(summary["shown"], results.len());
    assert_eq!(summary["truncated"], false);
    assert_eq!(summary["total_matches"], results.len());
    assert!(
        summary["sessions"].as_u64().unwrap() >= 1,
        "Summary should count distinct sessions"
    );
}

#[test]
fn search_summary_reports_truncation_when_limit_cuts_results() {
    let dir = setup_search_dir("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "sort", "--limit", "1"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (results, summary) = split_rows(&stdout);

    let summary = summary.expect("Last line should be a summary record");
    assert_eq!(results.len(), 1, "Only one result should be shown");
    assert_eq!(summary["shown"], 1);
    assert_eq!(summary["truncated"], true);
    assert!(
        summary["total_matches"].as_u64().unwrap() > 1,
        "total_matches should count matches beyond the limit"
    );
}

#[test]
fn search_long_content_returns_snippet_around_match() {
    let (dir, long_text) = setup_long_content_search_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "needle_42"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (rows, _summary) = split_rows(&stdout);
    assert_eq!(rows.len(), 1);

    let content = rows[0]["content"].as_str().unwrap();
    assert!(
        content.contains("needle_42"),
        "Snippet should contain the match"
    );
    assert!(
        content.chars().count() < long_text.chars().count() / 2,
        "Snippet should be much shorter than full content: {} vs {} chars",
        content.chars().count(),
        long_text.chars().count()
    );
    assert!(
        content.starts_with("...") && content.ends_with("..."),
        "Snippet in the middle of content should be marked with ellipses on both sides"
    );
}

#[test]
fn search_full_content_flag_returns_full_text() {
    let (dir, _long_text) = setup_long_content_search_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "needle_42", "--full-content"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (rows, _summary) = split_rows(&stdout);
    assert_eq!(rows.len(), 1);

    let content = rows[0]["content"].as_str().unwrap();
    assert!(
        content.contains("BEGIN_MARKER") && content.contains("END_MARKER"),
        "--full-content should return the entire message text"
    );
}

#[test]
fn search_regex_snippet_centers_on_matched_text() {
    let (dir, _long_text) = setup_long_content_search_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "needle_[0-9]+", "--regex"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (rows, _summary) = split_rows(&stdout);
    assert_eq!(rows.len(), 1);

    let content = rows[0]["content"].as_str().unwrap();
    assert!(
        content.contains("needle_42"),
        "Regex snippet should contain the actually matched text, got: {}",
        content
    );
    assert!(
        !content.contains("BEGIN_MARKER"),
        "Regex snippet should be centered on the match, not the start of content"
    );
}

#[test]
fn search_regex_snippet_uses_actual_match_offset() {
    let dir = setup_regex_offset_search_dir();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", r"\bfoo\b", "--regex"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "search should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (rows, _summary) = split_rows(&stdout);
    assert_eq!(rows.len(), 1);

    let content = rows[0]["content"].as_str().unwrap();
    assert!(
        content.contains("MATCH_MARKER foo"),
        "Regex snippet should include the actual regex match, got: {}",
        content
    );
    assert!(
        !content.contains("START_MARKER foobar"),
        "Regex snippet should not be anchored on an earlier literal occurrence"
    );
}

#[test]
fn search_snippet_is_safe_when_lowercase_expands_before_match() {
    let dir = setup_unicode_expansion_search_dir();
    let query = "\u{00e9}";

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", query])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "search should succeed without panicking, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (rows, _summary) = split_rows(&stdout);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["content"].as_str().unwrap(), "\u{0130}\u{00e9}");
}

#[test]
fn search_resolves_codex_subagent_hits_to_parent_session() {
    let dir = setup_codex_subagent_search_dir();
    let search_path = dir.path().join(".codex/sessions");
    let child_path = search_path
        .join("2026/05/03/rollout-2026-05-03T10-01-00-019f0000-0000-7000-8000-000000000002.jsonl");

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
    let (rows, _summary) = split_rows(&stdout);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["session_id"],
        "019f0000-0000-7000-8000-000000000001"
    );
    assert_eq!(rows[0]["file_path"], child_path.to_string_lossy().as_ref());
    assert_eq!(rows[0]["line_number"], 2);
    assert!(rows[0]["content"]
        .as_str()
        .unwrap_or("")
        .contains("Codex child needle"));
}

#[test]
fn search_resolves_claude_agent_hits_to_parent_session_but_emits_hit_path() {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("-test-project");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("parent-session.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Parent prompt"}]},"uuid":"parent-u1","sessionId":"parent-session","timestamp":"2025-06-01T10:00:00Z"}"#,
    )
    .unwrap();
    let agent_path = session_dir.join("agent-task.jsonl");
    fs::write(
        &agent_path,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Claude agent drilldown needle"}]},"uuid":"agent-a1","sessionId":"parent-session","timestamp":"2025-06-01T10:01:00Z"}"#,
    )
    .unwrap();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["search", "Claude agent drilldown needle", "--limit", "10"])
        .env("CCFS_SEARCH_PATH", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "search should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (rows, _summary) = split_rows(&stdout);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], "parent-session");
    assert_eq!(rows[0]["file_path"], agent_path.to_string_lossy().as_ref());
    assert_eq!(rows[0]["line_number"], 1);
    assert_eq!(rows[0]["message_uuid"], "agent-a1");
}
