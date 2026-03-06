use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Re-export SessionSource from the shared session module
pub use crate::session::SessionSource;

/// Represents a message from Claude Code JSONL session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub branch: Option<String>,
    pub line_number: usize,
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
}

impl Message {
    /// Parse a JSONL line into a Message
    /// Supports both Claude Code CLI format (sessionId, timestamp) and
    /// Claude Desktop format (session_id, _audit_timestamp)
    pub fn from_jsonl(line: &str, line_number: usize) -> Option<Self> {
        use crate::session;

        let json: serde_json::Value = serde_json::from_str(line).ok()?;

        // Skip non-message types (summary, etc.)
        let msg_type = session::extract_record_type(&json)?;
        if msg_type != "user" && msg_type != "assistant" {
            return None;
        }

        let message = json.get("message")?;
        let role = message.get("role")?.as_str()?.to_string();
        let content_raw = message.get("content")?;
        let content = Self::extract_content(content_raw);

        // Skip empty content
        if content.trim().is_empty() {
            return None;
        }

        let session_id = session::extract_session_id(&json)?;
        let timestamp = session::extract_timestamp(&json)?;

        // Branch is CLI-only, Desktop doesn't have it
        let branch = json
            .get("branch")
            .or_else(|| json.get("gitBranch"))
            .and_then(|b| b.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let uuid = session::extract_uuid(&json);
        let parent_uuid = session::extract_parent_uuid(&json);

        Some(Message {
            session_id,
            role,
            content,
            timestamp,
            branch,
            line_number,
            uuid,
            parent_uuid,
        })
    }

    /// Extract text content from message content blocks
    /// Handles both array format [{"type":"text","text":"..."}] and plain string format
    pub fn extract_content(raw: &serde_json::Value) -> String {
        // Handle plain string content (e.g., user messages with "content": "text")
        if let Some(s) = raw.as_str() {
            return s.to_string();
        }

        let mut parts: Vec<String> = Vec::new();

        if let Some(arr) = raw.as_array() {
            for item in arr {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match item_type {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            parts.push(text.to_string());
                        }
                    }
                    "tool_use" => {
                        // Include tool input for searchability
                        if let Some(input) = item.get("input") {
                            if let Ok(json_str) = serde_json::to_string(input) {
                                parts.push(json_str);
                            }
                        }
                    }
                    "tool_result" => {
                        if let Some(content) = item.get("content") {
                            if let Some(s) = content.as_str() {
                                parts.push(s.to_string());
                            } else if let Ok(json_str) = serde_json::to_string(content) {
                                parts.push(json_str);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_user_message() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse user message");

        assert_eq!(msg.session_id, "abc123");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "Hello Claude");
        assert_eq!(msg.line_number, 1);
    }

    #[test]
    fn test_parse_assistant_message() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello! How can I help?"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:01:00Z"}"#;

        let msg = Message::from_jsonl(jsonl, 2).expect("Should parse assistant message");

        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, "Hello! How can I help?");
    }

    #[test]
    fn test_parse_message_with_branch() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix bug"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z","cwd":"/projects/myapp","branch":"feature/fix-bug"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse message with branch");

        assert_eq!(msg.branch, Some("feature/fix-bug".to_string()));
    }

    #[test]
    fn test_parse_message_multiple_text_blocks() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Part 1"},{"type":"text","text":"Part 2"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse multiple text blocks");

        assert_eq!(msg.content, "Part 1\nPart 2");
    }

    #[test]
    fn test_parse_tool_use_message() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/test.txt"}}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse tool_use message");

        assert!(msg.content.contains("file_path"));
        assert!(msg.content.contains("/tmp/test.txt"));
    }

    #[test]
    fn test_skip_summary_type() {
        let jsonl = r#"{"type":"summary","summary":"Session summary text","sessionId":"abc123"}"#;

        let msg = Message::from_jsonl(jsonl, 1);

        assert!(msg.is_none(), "Should skip summary type messages");
    }

    #[test]
    fn test_skip_invalid_json() {
        let jsonl = "not valid json {{{";

        let msg = Message::from_jsonl(jsonl, 1);

        assert!(msg.is_none(), "Should skip invalid JSON");
    }

    #[test]
    fn test_extract_content_from_text_block() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "text", "text": "Hello world"}
        ]);

        let content = Message::extract_content(&raw);

        assert_eq!(content, "Hello world");
    }

    #[test]
    fn test_extract_content_from_tool_result() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "tool_result", "content": "File contents here"}
        ]);

        let content = Message::extract_content(&raw);

        assert!(content.contains("File contents here"));
    }

    #[test]
    fn test_parse_desktop_format_message() {
        // Desktop format uses session_id (underscore) and _audit_timestamp
        let jsonl = r#"{"type":"assistant","message":{"model":"claude-opus-4-5-20251101","id":"msg_01Q4WpB2jNfsHuijFwLGLFwN","type":"message","role":"assistant","content":[{"type":"text","text":"I'd love to help you analyze how you're spending your time!"}],"stop_reason":null,"stop_sequence":null},"parent_tool_use_id":null,"session_id":"0c2f5015-9457-491f-8f35-218d6c34ff68","uuid":"aa419e76-930a-4970-85c3-b5f03e85f6e0","_audit_timestamp":"2026-01-13T13:31:31.268Z"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse Desktop format message");

        assert_eq!(msg.session_id, "0c2f5015-9457-491f-8f35-218d6c34ff68");
        assert_eq!(msg.role, "assistant");
        assert!(msg.content.contains("I'd love to help you analyze"));
        assert!(msg.branch.is_none()); // Desktop doesn't have branch
        assert_eq!(
            msg.uuid,
            Some("aa419e76-930a-4970-85c3-b5f03e85f6e0".to_string())
        );
    }

    #[test]
    fn test_parse_desktop_user_message() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello from Desktop"}]},"session_id":"desktop-session-123","_audit_timestamp":"2026-01-13T10:00:00.000Z"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse Desktop user message");

        assert_eq!(msg.session_id, "desktop-session-123");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "Hello from Desktop");
    }

    #[test]
    fn test_parse_string_content() {
        // Some user messages have content as a plain string instead of array
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"Hello plain string"},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse plain string content");

        assert_eq!(msg.content, "Hello plain string");
        assert_eq!(msg.role, "user");
    }

    #[test]
    fn test_parse_uuid_and_parent_uuid() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z","uuid":"uuid-111","parentUuid":"uuid-000"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse message with uuid");

        assert_eq!(msg.uuid, Some("uuid-111".to_string()));
        assert_eq!(msg.parent_uuid, Some("uuid-000".to_string()));
    }

    #[test]
    fn test_parse_message_without_uuid() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        let msg = Message::from_jsonl(jsonl, 1).expect("Should parse message without uuid");

        assert_eq!(msg.uuid, None);
        assert_eq!(msg.parent_uuid, None);
    }

    #[test]
    fn test_extract_content_string() {
        let raw: serde_json::Value = serde_json::json!("plain text content");

        let content = Message::extract_content(&raw);

        assert_eq!(content, "plain text content");
    }
}
