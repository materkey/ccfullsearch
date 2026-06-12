#![allow(deprecated)]
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn setup_session_file(fixture: &str) -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let session_dir = dir.path().join("-test-project");
    fs::create_dir_all(&session_dir).unwrap();
    let fixture_content = fs::read_to_string(format!("tests/fixtures/{}", fixture)).unwrap();
    let file_path = session_dir.join("session.jsonl");
    fs::write(&file_path, fixture_content).unwrap();
    let path_str = file_path.to_str().unwrap().to_string();
    (dir, path_str)
}

/// Split stdout into message rows and the trailing summary row.
fn split_rows(stdout: &str) -> (Vec<serde_json::Value>, serde_json::Value) {
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summary = rows.last().expect("output should not be empty").clone();
    assert_eq!(summary["type"], "summary", "Last row should be the summary");
    let messages = rows[..rows.len() - 1].to_vec();
    for row in &messages {
        assert_eq!(
            row["type"], "message",
            "Non-summary rows should be messages"
        );
    }
    (messages, summary)
}

#[test]
fn show_returns_context_window_around_line() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--line", "3", "--context", "1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "show should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, summary) = split_rows(&stdout);

    assert_eq!(messages.len(), 3, "1 before + target + 1 after");
    assert_eq!(messages[0]["line_number"], 2);
    assert_eq!(messages[1]["line_number"], 3);
    assert_eq!(messages[2]["line_number"], 4);
    assert_eq!(summary["shown"], 3);
    assert_eq!(summary["target_line"], 3);
    assert_eq!(summary["session_id"], "sess-linear-001");
    assert_eq!(summary["provider"], "Claude");
}

#[test]
fn show_marks_target_row_only() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--line", "3", "--context", "1"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, _summary) = split_rows(&stdout);

    assert!(messages[0].get("is_target").is_none());
    assert_eq!(messages[1]["is_target"], true);
    assert!(messages[2].get("is_target").is_none());
    assert_eq!(messages[1]["message_uuid"], "u3");
    assert_eq!(messages[1]["parent_uuid"], "u2");
}

#[test]
fn show_clamps_at_file_boundaries() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--line", "1", "--context", "10"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, summary) = split_rows(&stdout);

    assert_eq!(
        messages.len(),
        4,
        "Window is clamped to the 4 messages in the file"
    );
    assert_eq!(messages[0]["is_target"], true);
    assert_eq!(summary["shown"], 4);
}

#[test]
fn show_clamps_oversized_context_without_overflow() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args([
            "show",
            &file_path,
            "--line",
            "1",
            "--context",
            &usize::MAX.to_string(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "show should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, summary) = split_rows(&stdout);

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["is_target"], true);
    assert_eq!(summary["shown"], 4);
}

#[test]
fn show_by_uuid_on_branched_session() {
    let (_dir, file_path) = setup_session_file("branched_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--uuid", "b2", "--context", "1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "show should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, summary) = split_rows(&stdout);

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["message_uuid"], "b2");
    assert_eq!(messages[1]["is_target"], true);
    assert_eq!(summary["target_line"], 5);
}

#[test]
fn show_context_counts_messages_not_lines() {
    // compaction_session.jsonl has a summary record on line 3 between messages;
    // it must not eat the context window.
    let (_dir, file_path) = setup_session_file("compaction_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--line", "4", "--context", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, _summary) = split_rows(&stdout);

    assert_eq!(messages.len(), 3);
    // The previous message is on line 2; line 3 (summary record) is skipped.
    assert_eq!(messages[0]["line_number"], 2);
    assert_eq!(messages[1]["line_number"], 4);
    assert_eq!(messages[1]["is_target"], true);
    assert_eq!(messages[2]["line_number"], 5);
}

#[test]
fn show_line_at_non_message_record_errors() {
    let (_dir, file_path) = setup_session_file("compaction_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--line", "3"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a message record"))
        .stderr(predicate::str::contains("summary"));
}

#[test]
fn show_line_out_of_range_errors() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--line", "99"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("beyond end of file"));
}

#[test]
fn show_unknown_uuid_errors() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--uuid", "no-such-uuid"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no message with uuid"));
}

#[test]
fn show_max_chars_truncates_and_sets_flag() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args([
            "show",
            &file_path,
            "--line",
            "2",
            "--context",
            "0",
            "--max-chars",
            "10",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, _summary) = split_rows(&stdout);

    assert_eq!(messages.len(), 1);
    let content = messages[0]["content"].as_str().unwrap();
    assert_eq!(content.chars().count(), 10);
    assert_eq!(messages[0]["content_truncated"], true);
}

#[test]
fn show_short_content_has_no_truncated_flag() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path, "--line", "1", "--context", "0"])
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, _summary) = split_rows(&stdout);
    assert!(messages[0].get("content_truncated").is_none());
}

#[test]
fn show_requires_line_or_uuid() {
    let (_dir, file_path) = setup_session_file("linear_session.jsonl");

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &file_path])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--line or --uuid"));
}

/// Build a minimal Opencode SQLite database with one session and three
/// messages. Same sqlite3-CLI seeding pattern as tests/cli_list.rs.
fn setup_opencode_db_fixture() -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("opencode.db");

    let schema = r#"
        CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, name TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
        CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, slug TEXT NOT NULL DEFAULT '', directory TEXT, title TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
        CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
        CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);

        INSERT INTO project VALUES ('projTEST', '/tmp/integration-test', 'integration-test', 1, 2);
        INSERT INTO session VALUES ('ses_SHOW1', 'projTEST', '', '/tmp/integration-test', 'Show test session', 1769762431585, 1769762509666);
        INSERT INTO message VALUES ('msg_user1', 'ses_SHOW1', 1769762431591, 1769762431591, '{"role":"user","time":{"created":1769762431591}}');
        INSERT INTO message VALUES ('msg_asst1', 'ses_SHOW1', 1769762431596, 1769762431596, '{"role":"assistant","parentID":"msg_user1","time":{"created":1769762431596}}');
        INSERT INTO message VALUES ('msg_user2', 'ses_SHOW1', 1769762431601, 1769762431601, '{"role":"user","time":{"created":1769762431601}}');
        INSERT INTO part VALUES ('prt_u1', 'msg_user1', 'ses_SHOW1', 1, 1, '{"type":"text","text":"first question"}');
        INSERT INTO part VALUES ('prt_a1', 'msg_asst1', 'ses_SHOW1', 2, 2, '{"type":"text","text":"the answer with details"}');
        INSERT INTO part VALUES ('prt_u2', 'msg_user2', 'ses_SHOW1', 3, 3, '{"type":"text","text":"follow-up question"}');
    "#;

    let status = std::process::Command::new("sqlite3")
        .arg(&db_path)
        .arg(schema)
        .status()
        .expect("sqlite3 must be available to seed the integration fixture");
    assert!(status.success(), "sqlite3 seeding failed");

    let synthetic_path = format!("{}#ses_SHOW1", db_path.to_str().unwrap());
    (dir, synthetic_path)
}

#[test]
fn show_opencode_by_uuid() {
    let (_dir, synthetic_path) = setup_opencode_db_fixture();

    let output = Command::cargo_bin("ccs")
        .unwrap()
        .args([
            "show",
            &synthetic_path,
            "--uuid",
            "msg_asst1",
            "--context",
            "1",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "show should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (messages, summary) = split_rows(&stdout);

    assert_eq!(messages.len(), 3);
    assert!(messages[0]["line_number"].is_null());
    assert_eq!(messages[0]["message_uuid"], "msg_user1");
    assert_eq!(messages[1]["message_uuid"], "msg_asst1");
    assert_eq!(messages[1]["is_target"], true);
    assert_eq!(messages[1]["parent_uuid"], "msg_user1");
    assert!(messages[1]["content"]
        .as_str()
        .unwrap()
        .contains("the answer with details"));
    assert_eq!(messages[2]["message_uuid"], "msg_user2");

    assert_eq!(summary["provider"], "Opencode");
    assert_eq!(summary["session_id"], "ses_SHOW1");
    assert_eq!(summary["target_uuid"], "msg_asst1");
    assert!(summary.get("target_line").is_none());
}

#[test]
fn show_opencode_without_uuid_errors() {
    let (_dir, synthetic_path) = setup_opencode_db_fixture();

    Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", &synthetic_path, "--line", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass --uuid"));
}

#[test]
fn show_missing_file_errors() {
    Command::cargo_bin("ccs")
        .unwrap()
        .args(["show", "/nonexistent/session.jsonl", "--line", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot open"));
}
