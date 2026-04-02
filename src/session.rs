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
}
