pub mod record;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Source of the Claude session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSource {
    /// Claude Code CLI sessions stored in ~/.claude/projects/
    ClaudeCodeCLI,
    /// Claude Desktop app sessions stored in ~/Library/Application Support/Claude/
    ClaudeDesktop,
}

impl SessionSource {
    /// Detect session source from file path
    pub fn from_path(path: &str) -> Self {
        if path.contains("local-agent-mode-sessions") {
            SessionSource::ClaudeDesktop
        } else {
            SessionSource::ClaudeCodeCLI
        }
    }

    /// Returns display name for the source
    pub fn display_name(&self) -> &'static str {
        match self {
            SessionSource::ClaudeCodeCLI => "CLI",
            SessionSource::ClaudeDesktop => "Desktop",
        }
    }
}

/// Extract session ID from a JSON record.
/// Supports both CLI format (`sessionId`) and Desktop format (`session_id`).
pub fn extract_session_id(json: &serde_json::Value) -> Option<String> {
    json.get("sessionId")
        .or_else(|| json.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract timestamp from a JSON record.
/// Supports both CLI format (`timestamp`) and Desktop format (`_audit_timestamp`).
pub fn extract_timestamp(json: &serde_json::Value) -> Option<DateTime<Utc>> {
    json.get("timestamp")
        .or_else(|| json.get("_audit_timestamp"))
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Extract uuid from a JSON record.
pub fn extract_uuid(json: &serde_json::Value) -> Option<String> {
    json.get("uuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract parentUuid from a JSON record.
pub fn extract_parent_uuid(json: &serde_json::Value) -> Option<String> {
    json.get("parentUuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract parentUuid, falling back to logicalParentUuid.
/// At compact_boundary points, parentUuid is null but logicalParentUuid preserves
/// the logical link to the pre-boundary chain.
pub fn extract_parent_uuid_or_logical(json: &serde_json::Value) -> Option<String> {
    extract_parent_uuid(json).or_else(|| {
        json.get("logicalParentUuid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Extract leafUuid from a JSON record.
pub fn extract_leaf_uuid(json: &serde_json::Value) -> Option<String> {
    json.get("leafUuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract record type from a JSON record.
pub fn extract_record_type(json: &serde_json::Value) -> Option<&str> {
    json.get("type").and_then(|v| v.as_str())
}

/// Returns true if the JSON record has `isSidechain: true`.
/// Sidechain messages are subagent messages that should not participate in the main chain.
pub fn is_sidechain(json: &serde_json::Value) -> bool {
    json.get("isSidechain")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

const RALPHEX_MARKER: &str = "<<<RALPHEX:";
const SCHEDULED_TASK_MARKER: &str = "<scheduled-task";
fn matches_scheduled_task_marker(content: &str) -> bool {
    content.trim_start().starts_with(SCHEDULED_TASK_MARKER)
}

fn matches_ralphex_marker(content: &str) -> bool {
    content.contains(RALPHEX_MARKER)
}

/// Check if `dir` contains `<session_id>.jsonl` or an `audit.jsonl` whose first
/// 50 lines mention `session_id`. Returns the matching file path if found.
fn check_dir_for_session(
    dir: &std::path::Path,
    target_filename: &str,
    session_id: &str,
) -> Option<String> {
    use std::io::{BufRead, BufReader};

    let candidate = dir.join(target_filename);
    if candidate.exists() {
        return Some(candidate.to_string_lossy().to_string());
    }
    let audit = dir.join("audit.jsonl");
    if audit.exists() {
        if let Ok(file) = std::fs::File::open(&audit) {
            let reader = BufReader::new(file);
            for line in reader.lines().take(50).flatten() {
                if line.contains(session_id) {
                    return Some(audit.to_string_lossy().to_string());
                }
            }
        }
    }
    None
}

/// Search for a JSONL file by session ID across the given search paths.
/// Checks CLI format (projects/<encoded>/<id>.jsonl) and Desktop format.
/// Desktop paths can be up to 3 levels deep:
///   local-agent-mode-sessions/<uuid>/<uuid>/local_xxx/audit.jsonl
pub fn find_session_file_in_paths(session_id: &str, search_paths: &[String]) -> Option<String> {
    use std::fs;
    let target_filename = format!("{}.jsonl", session_id);

    for search_path in search_paths {
        if let Ok(entries) = fs::read_dir(search_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Level 1: search_root/<dir>/
                if let Some(found) = check_dir_for_session(&path, &target_filename, session_id) {
                    return Some(found);
                }
                // Level 2: search_root/<dir>/<subdir>/
                if let Ok(subentries) = fs::read_dir(&path) {
                    for subentry in subentries.flatten() {
                        let subpath = subentry.path();
                        if !subpath.is_dir() {
                            continue;
                        }
                        if let Some(found) =
                            check_dir_for_session(&subpath, &target_filename, session_id)
                        {
                            return Some(found);
                        }
                        // Level 3: search_root/<dir>/<subdir>/<subsubdir>/
                        // Desktop: <uuid>/<uuid>/local_xxx/audit.jsonl
                        if let Ok(deep_entries) = fs::read_dir(&subpath) {
                            for deep_entry in deep_entries.flatten() {
                                let deep_path = deep_entry.path();
                                if deep_path.is_dir() {
                                    if let Some(found) = check_dir_for_session(
                                        &deep_path,
                                        &target_filename,
                                        session_id,
                                    ) {
                                        return Some(found);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Resolve the correct session ID and file path for `claude --resume`.
///
/// Claude CLI matches sessions by filename: it looks for `<session-id>.jsonl`.
/// But search results may come from auxiliary files (agent files, metadata files)
/// whose filename doesn't match the `sessionId` in their content.
///
/// This function handles three cases:
/// 1. **Subagent file** (`../session-id/subagents/agent-xxx.jsonl`):
///    resolves to the parent `session-id.jsonl`
/// 2. **Mismatched filename** (file's stem != session_id):
///    looks for `<session_id>.jsonl` in the same directory
/// 3. **Normal file**: returns as-is
pub fn resolve_parent_session(session_id: &str, file_path: &str) -> (String, String) {
    let path = std::path::Path::new(file_path);

    // Case 1: subagent file under .../session-id/subagents/
    if let Some(parent) = path.parent() {
        if parent.file_name().and_then(|n| n.to_str()) == Some("subagents") {
            if let Some(session_dir) = parent.parent() {
                let parent_jsonl = session_dir.with_extension("jsonl");
                if parent_jsonl.exists() {
                    let dir_name = session_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(session_id);
                    return (
                        dir_name.to_string(),
                        parent_jsonl.to_string_lossy().to_string(),
                    );
                }
            }
        }
    }

    // Case 2: filename stem doesn't match session_id — find the correct file
    let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if file_stem != session_id {
        if let Some(parent_dir) = path.parent() {
            let correct_file = parent_dir.join(format!("{}.jsonl", session_id));
            if correct_file.exists() {
                return (
                    session_id.to_string(),
                    correct_file.to_string_lossy().to_string(),
                );
            }
        }
    }

    // Case 3: normal file
    (session_id.to_string(), file_path.to_string())
}

/// Detect whether message content was produced by a known automation tool.
/// Returns the tool name if a marker is found, `None` otherwise.
pub fn detect_automation(content: &str) -> Option<&'static str> {
    if matches_scheduled_task_marker(content) {
        return Some("scheduled");
    }

    if matches_ralphex_marker(content) {
        return Some("ralphex");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_source_from_cli_path() {
        let path = "/Users/user/.claude/projects/-Users-user-myproject/abc123.jsonl";
        assert_eq!(SessionSource::from_path(path), SessionSource::ClaudeCodeCLI);
    }

    #[test]
    fn test_session_source_from_desktop_path() {
        let path = "/Users/user/Library/Application Support/Claude/local-agent-mode-sessions/uuid1/uuid2/local_session/audit.jsonl";
        assert_eq!(SessionSource::from_path(path), SessionSource::ClaudeDesktop);
    }

    #[test]
    fn test_session_source_display_name() {
        assert_eq!(SessionSource::ClaudeCodeCLI.display_name(), "CLI");
        assert_eq!(SessionSource::ClaudeDesktop.display_name(), "Desktop");
    }

    #[test]
    fn test_extract_session_id_cli_format() {
        let json: serde_json::Value = serde_json::json!({"sessionId": "abc123", "type": "user"});
        assert_eq!(extract_session_id(&json), Some("abc123".to_string()));
    }

    #[test]
    fn test_extract_session_id_desktop_format() {
        let json: serde_json::Value =
            serde_json::json!({"session_id": "desktop-456", "type": "user"});
        assert_eq!(extract_session_id(&json), Some("desktop-456".to_string()));
    }

    #[test]
    fn test_extract_session_id_cli_takes_precedence() {
        let json: serde_json::Value =
            serde_json::json!({"sessionId": "cli", "session_id": "desktop"});
        assert_eq!(extract_session_id(&json), Some("cli".to_string()));
    }

    #[test]
    fn test_extract_timestamp_cli_format() {
        let json: serde_json::Value = serde_json::json!({"timestamp": "2025-01-09T10:00:00Z"});
        let ts = extract_timestamp(&json).unwrap();
        assert_eq!(ts.to_rfc3339(), "2025-01-09T10:00:00+00:00");
    }

    #[test]
    fn test_extract_timestamp_desktop_format() {
        let json: serde_json::Value =
            serde_json::json!({"_audit_timestamp": "2025-01-09T10:00:00.000Z"});
        assert!(extract_timestamp(&json).is_some());
    }

    #[test]
    fn test_extract_uuid() {
        let json: serde_json::Value = serde_json::json!({"uuid": "u1"});
        assert_eq!(extract_uuid(&json), Some("u1".to_string()));
    }

    #[test]
    fn test_extract_uuid_missing() {
        let json: serde_json::Value = serde_json::json!({"type": "user"});
        assert_eq!(extract_uuid(&json), None);
    }

    #[test]
    fn test_extract_parent_uuid() {
        let json: serde_json::Value = serde_json::json!({"parentUuid": "p1"});
        assert_eq!(extract_parent_uuid(&json), Some("p1".to_string()));
    }

    #[test]
    fn test_extract_leaf_uuid() {
        let json: serde_json::Value = serde_json::json!({"leafUuid": "l1"});
        assert_eq!(extract_leaf_uuid(&json), Some("l1".to_string()));
    }

    #[test]
    fn test_extract_record_type() {
        let json: serde_json::Value = serde_json::json!({"type": "user"});
        assert_eq!(extract_record_type(&json), Some("user"));
    }

    #[test]
    fn test_detect_automation_ralphex_marker() {
        let content = "Path A - No issues: Output <<<RALPHEX:REVIEW_DONE>>>";
        assert_eq!(detect_automation(content), Some("ralphex"));
    }

    #[test]
    fn test_detect_automation_ralphex_all_tasks_done() {
        let content = "Follow the plan and emit <<<RALPHEX:ALL_TASKS_DONE>>> when complete";
        assert_eq!(detect_automation(content), Some("ralphex"));
    }

    #[test]
    fn test_detect_automation_scheduled_task() {
        let content = r#"<scheduled-task name="chezmoi-sync" file="/Users/user/.claude/scheduled-tasks/chezmoi-sync/SCHEDULE.md">"#;
        assert_eq!(detect_automation(content), Some("scheduled"));
    }

    #[test]
    fn test_detect_automation_no_marker() {
        let content = "How do I sort a list in Python?";
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_detect_automation_matches_ralphex_marker_anywhere() {
        let content = "Ralphex uses <<<RALPHEX:ALL_TASKS_DONE>>> signals.";
        assert_eq!(detect_automation(content), Some("ralphex"));
    }

    #[test]
    fn test_detect_automation_scheduled_task_not_at_start() {
        // scheduled-task must start the message (trim_start), not appear mid-text
        let content = r#"такие тоже надо детектить <scheduled-task name="chezmoi-sync">"#;
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_detect_automation_empty_content() {
        assert_eq!(detect_automation(""), None);
    }

    #[test]
    fn test_detect_automation_partial_marker_no_match() {
        // Just "RALPHEX" without the <<< prefix should not match
        let content = "discussing RALPHEX in a conversation";
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_resolve_parent_for_subagent_uses_parent_session_id() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("aaa-bbb-ccc");
        let subagents_dir = session_dir.join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();

        // Create parent JSONL
        let parent_jsonl = dir.path().join("aaa-bbb-ccc.jsonl");
        fs::write(&parent_jsonl, "{}").unwrap();

        // Create subagent JSONL
        let agent_file = subagents_dir.join("agent-xyz.jsonl");
        fs::write(&agent_file, "{}").unwrap();

        let (sid, fpath) = resolve_parent_session("wrong-id", agent_file.to_str().unwrap());
        assert_eq!(sid, "aaa-bbb-ccc");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_top_level_agent_uses_filename_session_id() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-Users-proj");
        fs::create_dir_all(&project_dir).unwrap();

        // Parent session file
        let parent_jsonl = project_dir.join("64cd6570-parent.jsonl");
        fs::write(&parent_jsonl, r#"{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        // Top-level agent file with sessionId pointing to parent
        let agent_file = project_dir.join("agent-abc123.jsonl");
        fs::write(&agent_file, r#"{"type":"user","message":{"role":"user","content":"sub"},"sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        let (sid, fpath) = resolve_parent_session("64cd6570-parent", agent_file.to_str().unwrap());
        assert_eq!(sid, "64cd6570-parent");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_exact_user_scenario() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("-Users-Shared-projects-avito-android");
        fs::create_dir_all(&project_dir).unwrap();

        // Main session file
        let main_file = project_dir.join("64cd6570-3475-47fd-a2cc-2da718d0dcb3.jsonl");
        fs::write(&main_file, r#"{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"64cd6570-3475-47fd-a2cc-2da718d0dcb3","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        // Auxiliary metadata file (different UUID filename, same sessionId inside)
        let aux_file = project_dir.join("68483698-e6fc-4ea8-a85e-989e6dfa5c2f.jsonl");
        fs::write(&aux_file, r#"{"type":"last-prompt","lastPrompt":"test","sessionId":"64cd6570-3475-47fd-a2cc-2da718d0dcb3"}"#).unwrap();

        let (sid, fpath) = resolve_parent_session(
            "64cd6570-3475-47fd-a2cc-2da718d0dcb3",
            aux_file.to_str().unwrap(),
        );
        assert_eq!(sid, "64cd6570-3475-47fd-a2cc-2da718d0dcb3");
        assert_eq!(fpath, main_file.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_auxiliary_file_finds_main_session() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-Users-proj");
        fs::create_dir_all(&project_dir).unwrap();

        // Parent session file
        let parent_jsonl = project_dir.join("64cd6570-parent.jsonl");
        fs::write(&parent_jsonl, "{}").unwrap();

        // Auxiliary file with different filename but sessionId pointing to parent
        let aux_file = project_dir.join("1630cd72-auxiliary.jsonl");
        fs::write(&aux_file, "{}").unwrap();

        let (sid, fpath) = resolve_parent_session("64cd6570-parent", aux_file.to_str().unwrap());
        assert_eq!(sid, "64cd6570-parent");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }
}
