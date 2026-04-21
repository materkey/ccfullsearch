use crate::session;

/// Role of a message in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    /// UI-facing label (`"User"` / `"Claude"`), distinct from the wire-level
    /// `"user"` / `"assistant"` strings found in JSONL records.
    pub fn display_label(self) -> &'static str {
        match self {
            MessageRole::User => "User",
            MessageRole::Assistant => "Claude",
        }
    }
}

/// A single content block within a message.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text(String),
    ToolUse { name: String, input: String },
    ToolResult(String),
    Thinking(String),
}

/// Controls how content blocks are rendered to a string.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentMode {
    /// All block types, newline-joined (for search).
    Full,
    /// Truncated with placeholders for tools, space-joined, XML-stripped (for tree preview).
    Preview { max_chars: usize },
    /// Text blocks only, space-joined (for title extraction).
    TextOnly,
}

/// A parsed JSONL record from a Claude session file.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionRecord {
    Message {
        role: MessageRole,
        content_blocks: Vec<ContentBlock>,
        uuid: Option<String>,
        parent_uuid: Option<String>,
        is_sidechain: bool,
    },
    Summary {
        text: String,
        is_compaction: bool,
        uuid: Option<String>,
        parent_uuid: Option<String>,
        leaf_uuid: Option<String>,
        is_sidechain: bool,
    },
    CustomTitle(String),
    AiTitle(String),
    AgentName(String),
    LastPrompt(String),
    CompactBoundary {
        uuid: Option<String>,
        parent_uuid: Option<String>,
        logical_parent_uuid: Option<String>,
        is_sidechain: bool,
    },
    /// Known record with type field but no specific handling (e.g. system without compact_boundary).
    /// Carries uuid/parent_uuid so it still participates in the DAG as a bridge node.
    Metadata {
        uuid: Option<String>,
        parent_uuid: Option<String>,
        is_sidechain: bool,
    },
    /// Unrecognized record type (e.g. progress, attachment).
    /// Carries uuid/parent_uuid so it still participates in the DAG as a bridge node.
    Other {
        uuid: Option<String>,
        parent_uuid: Option<String>,
        is_sidechain: bool,
    },
}

impl SessionRecord {
    /// Parse a single JSONL line into a typed SessionRecord.
    /// Returns `None` if the line is not valid JSON.
    pub fn from_jsonl(line: &str) -> Option<Self> {
        let json: serde_json::Value = serde_json::from_str(line).ok()?;
        Self::from_value(&json)
    }

    /// Parse a pre-parsed JSON value into a typed SessionRecord.
    /// Returns `None` if the value has no recognized `type` field.
    pub fn from_value(json: &serde_json::Value) -> Option<Self> {
        let record_type = session::extract_record_type(json)?;

        match record_type {
            "user" | "assistant" => {
                let role = if record_type == "user" {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                };
                let content_raw = json.get("message").and_then(|m| m.get("content"));
                let content_blocks = match content_raw {
                    Some(raw) => parse_content_blocks(raw),
                    None => Vec::new(),
                };
                Some(SessionRecord::Message {
                    role,
                    content_blocks,
                    uuid: session::extract_uuid(json),
                    parent_uuid: session::extract_parent_uuid_or_logical(json),
                    is_sidechain: session::is_sidechain(json),
                })
            }
            "summary" | "compaction" => {
                let text = json
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(SessionRecord::Summary {
                    text,
                    is_compaction: record_type == "compaction",
                    uuid: session::extract_uuid(json),
                    parent_uuid: session::extract_parent_uuid_or_logical(json),
                    leaf_uuid: session::extract_leaf_uuid(json),
                    is_sidechain: session::is_sidechain(json),
                })
            }
            "custom-title" => {
                let title = json
                    .get("customTitle")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(SessionRecord::CustomTitle(title))
            }
            "ai-title" => {
                let title = json
                    .get("aiTitle")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(SessionRecord::AiTitle(title))
            }
            "agent-name" => {
                let name = json
                    .get("agentName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(SessionRecord::AgentName(name))
            }
            "last-prompt" => {
                let prompt = json
                    .get("lastPrompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(SessionRecord::LastPrompt(prompt))
            }
            "system" => {
                let subtype = json.get("subtype").and_then(|v| v.as_str());
                if subtype == Some("compact_boundary") {
                    Some(SessionRecord::CompactBoundary {
                        uuid: session::extract_uuid(json),
                        parent_uuid: session::extract_parent_uuid(json),
                        logical_parent_uuid: json
                            .get("logicalParentUuid")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        is_sidechain: session::is_sidechain(json),
                    })
                } else {
                    Some(SessionRecord::Metadata {
                        uuid: session::extract_uuid(json),
                        parent_uuid: session::extract_parent_uuid_or_logical(json),
                        is_sidechain: session::is_sidechain(json),
                    })
                }
            }
            _ => Some(SessionRecord::Other {
                uuid: session::extract_uuid(json),
                parent_uuid: session::extract_parent_uuid_or_logical(json),
                is_sidechain: session::is_sidechain(json),
            }),
        }
    }

    /// Render content blocks to a string according to the given mode.
    pub fn render_content(blocks: &[ContentBlock], mode: &ContentMode) -> String {
        match mode {
            ContentMode::Full => render_full(blocks),
            ContentMode::TextOnly => render_text_only(blocks),
            ContentMode::Preview { max_chars } => render_preview(blocks, *max_chars),
        }
    }

    /// Get the DAG uuid for records that participate in the conversation DAG.
    pub fn dag_uuid(&self) -> Option<&str> {
        match self {
            SessionRecord::Message { uuid, .. }
            | SessionRecord::Summary { uuid, .. }
            | SessionRecord::CompactBoundary { uuid, .. }
            | SessionRecord::Metadata { uuid, .. }
            | SessionRecord::Other { uuid, .. } => uuid.as_deref(),
            _ => None,
        }
    }

    /// Get the DAG parent uuid for records that participate in the conversation DAG.
    /// For CompactBoundary, falls back to logical_parent_uuid when parent_uuid is None.
    pub fn dag_parent_uuid(&self) -> Option<&str> {
        match self {
            SessionRecord::Message { parent_uuid, .. }
            | SessionRecord::Summary { parent_uuid, .. }
            | SessionRecord::Metadata { parent_uuid, .. }
            | SessionRecord::Other { parent_uuid, .. } => parent_uuid.as_deref(),
            SessionRecord::CompactBoundary {
                parent_uuid,
                logical_parent_uuid,
                ..
            } => parent_uuid.as_deref().or(logical_parent_uuid.as_deref()),
            _ => None,
        }
    }

    /// Returns true if this record is marked as a sidechain (subagent) message.
    pub fn is_sidechain(&self) -> bool {
        match self {
            SessionRecord::Message { is_sidechain, .. }
            | SessionRecord::Summary { is_sidechain, .. }
            | SessionRecord::CompactBoundary { is_sidechain, .. }
            | SessionRecord::Metadata { is_sidechain, .. }
            | SessionRecord::Other { is_sidechain, .. } => *is_sidechain,
            _ => false,
        }
    }

    /// Get the content blocks for Message records. Returns empty slice for other types.
    pub fn content_blocks(&self) -> &[ContentBlock] {
        match self {
            SessionRecord::Message { content_blocks, .. } => content_blocks,
            _ => &[],
        }
    }
}

/// Parse raw JSON content value into typed ContentBlocks.
pub(crate) fn parse_content_blocks(raw: &serde_json::Value) -> Vec<ContentBlock> {
    if let Some(s) = raw.as_str() {
        return vec![ContentBlock::Text(s.to_string())];
    }

    let mut blocks = Vec::new();
    if let Some(arr) = raw.as_array() {
        for item in arr {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "text" => {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        blocks.push(ContentBlock::Text(text.to_string()));
                    }
                }
                "tool_use" => {
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let input = item
                        .get("input")
                        .map(|i| serde_json::to_string(i).unwrap_or_default())
                        .unwrap_or_default();
                    blocks.push(ContentBlock::ToolUse { name, input });
                }
                "tool_result" => {
                    let content = if let Some(c) = item.get("content") {
                        if let Some(s) = c.as_str() {
                            s.to_string()
                        } else if let Some(arr) = c.as_array() {
                            let mut parts = Vec::new();
                            for entry in arr {
                                let entry_type =
                                    entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                match entry_type {
                                    "text" => {
                                        if let Some(t) = entry.get("text").and_then(|t| t.as_str())
                                        {
                                            parts.push(t.to_string());
                                        }
                                    }
                                    "image" => parts.push("[image]".to_string()),
                                    "document" => parts.push("[document]".to_string()),
                                    _ => {}
                                }
                            }
                            parts.join("\n")
                        } else {
                            serde_json::to_string(c).unwrap_or_default()
                        }
                    } else {
                        String::new()
                    };
                    blocks.push(ContentBlock::ToolResult(content));
                }
                "thinking" => {
                    if let Some(t) = item.get("thinking").and_then(|t| t.as_str()) {
                        blocks.push(ContentBlock::Thinking(t.to_string()));
                    }
                }
                "image" => {
                    blocks.push(ContentBlock::Text("[image]".to_string()));
                }
                "document" => {
                    blocks.push(ContentBlock::Text("[document]".to_string()));
                }
                "redacted_thinking" => {
                    blocks.push(ContentBlock::Thinking("[redacted]".to_string()));
                }
                "server_tool_use" => {
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    blocks.push(ContentBlock::ToolUse {
                        name,
                        input: String::new(),
                    });
                }
                "connector_text" => {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        blocks.push(ContentBlock::Text(text.to_string()));
                    }
                }
                _ => {}
            }
        }
    }
    blocks
}

/// Full mode: all block types, newline-joined (matches extract_message_content behavior).
fn render_full(blocks: &[ContentBlock]) -> String {
    let parts: Vec<&str> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text(t) => t.as_str(),
            ContentBlock::ToolUse { name, input } => {
                if input.is_empty() {
                    name.as_str()
                } else {
                    input.as_str()
                }
            }
            ContentBlock::ToolResult(c) => c.as_str(),
            ContentBlock::Thinking(t) => t.as_str(),
        })
        .collect();
    parts.join("\n")
}

/// TextOnly mode: text blocks only, space-joined (matches extract_text_content behavior).
fn render_text_only(blocks: &[ContentBlock]) -> String {
    let parts: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => {
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            _ => None,
        })
        .collect();
    parts.join(" ")
}

/// Preview mode: all types with placeholders, space-joined, XML-stripped, truncated.
/// Matches extract_preview behavior from tree/mod.rs.
fn render_preview(blocks: &[ContentBlock], max_chars: usize) -> String {
    let parts: Vec<String> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text(t) => t.clone(),
            ContentBlock::ToolUse { name, .. } => format!("[tool: {}]", name),
            ContentBlock::ToolResult(_) => "[tool_result]".to_string(),
            ContentBlock::Thinking(t) => t.clone(),
        })
        .collect();
    let joined = parts.join(" ");

    let stripped = strip_xml_tags(&joined);
    let sanitized = stripped
        .replace('\n', " ")
        .replace('\r', "")
        .replace('\t', " ");

    let collapsed = collapse_spaces(&sanitized);

    if collapsed.chars().count() > max_chars {
        collapsed.chars().take(max_chars).collect::<String>() + "..."
    } else {
        collapsed
    }
}

/// Strip XML/HTML-like tags from text, replacing them with spaces.
fn strip_xml_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                result.push(' ');
            }
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

/// Parse a raw JSON content value and render it as text-only (TextOnly mode).
/// Returns None if the result is empty.
pub fn render_text_content(content: &serde_json::Value) -> Option<String> {
    let blocks = parse_content_blocks(content);
    let text = SessionRecord::render_content(&blocks, &ContentMode::TextOnly);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Collapse runs of multiple spaces into single spaces.
fn collapse_spaces(text: &str) -> String {
    let mut prev_space = false;
    text.chars()
        .filter(|c| {
            if *c == ' ' {
                if prev_space {
                    return false;
                }
                prev_space = true;
            } else {
                prev_space = false;
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- from_jsonl tests ---

    #[test]
    fn test_from_jsonl_user_message() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello"}]},"sessionId":"s1","uuid":"u1","timestamp":"2025-01-01T00:00:00Z"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        match record {
            SessionRecord::Message {
                role,
                content_blocks,
                uuid,
                ..
            } => {
                assert_eq!(role, MessageRole::User);
                assert_eq!(content_blocks.len(), 1);
                assert_eq!(content_blocks[0], ContentBlock::Text("Hello".into()));
                assert_eq!(uuid, Some("u1".into()));
            }
            other => panic!("Expected Message, got {:?}", other),
        }
    }

    #[test]
    fn test_from_jsonl_assistant_message() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi there"}]},"sessionId":"s1","uuid":"u2","parentUuid":"u1","timestamp":"2025-01-01T00:01:00Z"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        match record {
            SessionRecord::Message {
                role,
                uuid,
                parent_uuid,
                ..
            } => {
                assert_eq!(role, MessageRole::Assistant);
                assert_eq!(uuid, Some("u2".into()));
                assert_eq!(parent_uuid, Some("u1".into()));
            }
            other => panic!("Expected Message, got {:?}", other),
        }
    }

    #[test]
    fn test_from_jsonl_summary() {
        let line = r#"{"type":"summary","summary":"A discussion about Rust","sessionId":"s1","uuid":"su1","parentUuid":"u2","leafUuid":"u2"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        match record {
            SessionRecord::Summary {
                text,
                is_compaction,
                uuid,
                leaf_uuid,
                ..
            } => {
                assert_eq!(text, "A discussion about Rust");
                assert!(!is_compaction);
                assert_eq!(uuid, Some("su1".into()));
                assert_eq!(leaf_uuid, Some("u2".into()));
            }
            other => panic!("Expected Summary, got {:?}", other),
        }
    }

    #[test]
    fn test_from_jsonl_compaction() {
        let line = r#"{"type":"compaction","summary":"Compacted context","sessionId":"s1","uuid":"c1","parentUuid":"u3"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        match record {
            SessionRecord::Summary {
                text,
                is_compaction,
                ..
            } => {
                assert_eq!(text, "Compacted context");
                assert!(is_compaction);
            }
            other => panic!("Expected Summary (compaction), got {:?}", other),
        }
    }

    #[test]
    fn test_from_jsonl_custom_title() {
        let line = r#"{"type":"custom-title","customTitle":"My Session Title","sessionId":"s1"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        assert_eq!(
            record,
            SessionRecord::CustomTitle("My Session Title".into())
        );
    }

    #[test]
    fn test_from_jsonl_ai_title() {
        let line = r#"{"type":"ai-title","aiTitle":"AI Generated Title","sessionId":"s1"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        assert_eq!(record, SessionRecord::AiTitle("AI Generated Title".into()));
    }

    #[test]
    fn test_from_jsonl_agent_name() {
        let line = r#"{"type":"agent-name","agentName":"code-reviewer","sessionId":"s1"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        assert_eq!(record, SessionRecord::AgentName("code-reviewer".into()));
    }

    #[test]
    fn test_from_jsonl_last_prompt() {
        let line =
            r#"{"type":"last-prompt","lastPrompt":"Fix the bug in parser","sessionId":"s1"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        assert_eq!(
            record,
            SessionRecord::LastPrompt("Fix the bug in parser".into())
        );
    }

    #[test]
    fn test_from_jsonl_compact_boundary() {
        let line = r#"{"type":"system","subtype":"compact_boundary","uuid":"cb1","logicalParentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        match record {
            SessionRecord::CompactBoundary {
                uuid,
                parent_uuid,
                logical_parent_uuid,
                ..
            } => {
                assert_eq!(uuid, Some("cb1".into()));
                assert_eq!(parent_uuid, None);
                assert_eq!(logical_parent_uuid, Some("u2".into()));
            }
            other => panic!("Expected CompactBoundary, got {:?}", other),
        }
    }

    #[test]
    fn test_from_jsonl_unknown_type() {
        let line = r#"{"type":"unknown-future-type","data":"something"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        assert!(matches!(record, SessionRecord::Other { .. }));
    }

    #[test]
    fn test_from_jsonl_invalid_json() {
        let line = "not valid json at all";
        assert!(SessionRecord::from_jsonl(line).is_none());
    }

    #[test]
    fn test_from_jsonl_system_non_compact_boundary() {
        let line = r#"{"type":"system","subtype":"other_subtype","sessionId":"s1"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        assert!(matches!(record, SessionRecord::Metadata { .. }));
    }

    #[test]
    fn test_from_jsonl_sidechain_message() {
        let line = r#"{"type":"user","message":{"role":"user","content":"sub-task"},"sessionId":"s1","uuid":"su1","isSidechain":true}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        assert!(record.is_sidechain());
    }

    #[test]
    fn test_from_jsonl_message_plain_string_content() {
        let line = r#"{"type":"user","message":{"role":"user","content":"plain text"},"sessionId":"s1","uuid":"u1"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        match record {
            SessionRecord::Message { content_blocks, .. } => {
                assert_eq!(
                    content_blocks,
                    vec![ContentBlock::Text("plain text".into())]
                );
            }
            other => panic!("Expected Message, got {:?}", other),
        }
    }

    #[test]
    fn test_from_jsonl_message_with_tool_use() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/test.txt"}}]},"sessionId":"s1","uuid":"u1"}"#;
        let record = SessionRecord::from_jsonl(line).unwrap();
        match record {
            SessionRecord::Message { content_blocks, .. } => {
                assert_eq!(content_blocks.len(), 1);
                match &content_blocks[0] {
                    ContentBlock::ToolUse { name, input } => {
                        assert_eq!(name, "Read");
                        assert!(input.contains("file_path"));
                    }
                    other => panic!("Expected ToolUse, got {:?}", other),
                }
            }
            other => panic!("Expected Message, got {:?}", other),
        }
    }

    #[test]
    fn test_from_jsonl_no_type_field() {
        let line = r#"{"data":"no type field here"}"#;
        assert!(SessionRecord::from_jsonl(line).is_none());
    }

    // --- render_content tests ---

    #[test]
    fn test_render_full_text_and_tool_use_and_thinking() {
        let blocks = vec![
            ContentBlock::Thinking("Let me think...".into()),
            ContentBlock::Text("Here is the answer".into()),
            ContentBlock::ToolUse {
                name: "Bash".into(),
                input: r#"{"command":"ls"}"#.into(),
            },
            ContentBlock::ToolResult("file1.txt\nfile2.txt".into()),
        ];
        let result = SessionRecord::render_content(&blocks, &ContentMode::Full);
        assert!(result.contains("Let me think..."));
        assert!(result.contains("Here is the answer"));
        assert!(result.contains(r#"{"command":"ls"}"#));
        assert!(result.contains("file1.txt"));
        // Full mode joins with newlines
        assert!(result.contains('\n'));
    }

    #[test]
    fn test_render_text_only() {
        let blocks = vec![
            ContentBlock::Thinking("internal thought".into()),
            ContentBlock::Text("Hello world".into()),
            ContentBlock::ToolUse {
                name: "Read".into(),
                input: "{}".into(),
            },
            ContentBlock::Text("Goodbye".into()),
        ];
        let result = SessionRecord::render_content(&blocks, &ContentMode::TextOnly);
        assert_eq!(result, "Hello world Goodbye");
    }

    #[test]
    fn test_render_text_only_trims_whitespace() {
        let blocks = vec![
            ContentBlock::Text("  spaced  ".into()),
            ContentBlock::Text("".into()),
            ContentBlock::Text("end".into()),
        ];
        let result = SessionRecord::render_content(&blocks, &ContentMode::TextOnly);
        assert_eq!(result, "spaced end");
    }

    #[test]
    fn test_render_preview_truncated() {
        let blocks = vec![ContentBlock::Text(
            "This is a very long message that should be truncated".into(),
        )];
        let result =
            SessionRecord::render_content(&blocks, &ContentMode::Preview { max_chars: 20 });
        assert!(result.ends_with("..."));
        // 20 chars + "..."
        assert!(result.chars().count() <= 23);
    }

    #[test]
    fn test_render_preview_tool_placeholders() {
        let blocks = vec![
            ContentBlock::Text("Running command".into()),
            ContentBlock::ToolUse {
                name: "Bash".into(),
                input: r#"{"command":"ls -la"}"#.into(),
            },
            ContentBlock::ToolResult("output here".into()),
        ];
        let result =
            SessionRecord::render_content(&blocks, &ContentMode::Preview { max_chars: 200 });
        assert!(result.contains("Running command"));
        assert!(result.contains("[tool: Bash]"));
        assert!(result.contains("[tool_result]"));
    }

    #[test]
    fn test_render_preview_strips_xml() {
        let blocks = vec![ContentBlock::Text(
            "<system-reminder>hidden</system-reminder>visible".into(),
        )];
        let result =
            SessionRecord::render_content(&blocks, &ContentMode::Preview { max_chars: 200 });
        assert!(!result.contains("<system-reminder>"));
        assert!(result.contains("visible"));
    }

    #[test]
    fn test_render_preview_collapses_whitespace() {
        let blocks = vec![ContentBlock::Text("word1   word2\n\nword3".into())];
        let result =
            SessionRecord::render_content(&blocks, &ContentMode::Preview { max_chars: 200 });
        assert!(!result.contains("  "));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn test_render_full_empty_blocks() {
        let result = SessionRecord::render_content(&[], &ContentMode::Full);
        assert_eq!(result, "");
    }

    #[test]
    fn test_render_text_only_no_text_blocks() {
        let blocks = vec![
            ContentBlock::ToolUse {
                name: "Read".into(),
                input: "{}".into(),
            },
            ContentBlock::Thinking("thought".into()),
        ];
        let result = SessionRecord::render_content(&blocks, &ContentMode::TextOnly);
        assert_eq!(result, "");
    }

    // --- convenience method tests ---

    #[test]
    fn test_dag_uuid_message() {
        let record = SessionRecord::Message {
            role: MessageRole::User,
            content_blocks: vec![],
            uuid: Some("u1".into()),
            parent_uuid: None,
            is_sidechain: false,
        };
        assert_eq!(record.dag_uuid(), Some("u1"));
    }

    #[test]
    fn test_dag_uuid_summary() {
        let record = SessionRecord::Summary {
            text: "test".into(),
            is_compaction: false,
            uuid: Some("su1".into()),
            parent_uuid: None,
            leaf_uuid: None,
            is_sidechain: false,
        };
        assert_eq!(record.dag_uuid(), Some("su1"));
    }

    #[test]
    fn test_dag_uuid_compact_boundary() {
        let record = SessionRecord::CompactBoundary {
            uuid: Some("cb1".into()),
            parent_uuid: None,
            logical_parent_uuid: Some("u2".into()),
            is_sidechain: false,
        };
        assert_eq!(record.dag_uuid(), Some("cb1"));
    }

    #[test]
    fn test_dag_uuid_metadata_without_uuid_returns_none() {
        let meta = SessionRecord::Metadata {
            uuid: None,
            parent_uuid: None,
            is_sidechain: false,
        };
        let other = SessionRecord::Other {
            uuid: None,
            parent_uuid: None,
            is_sidechain: false,
        };
        assert_eq!(meta.dag_uuid(), None);
        assert_eq!(other.dag_uuid(), None);
        assert_eq!(SessionRecord::CustomTitle("t".into()).dag_uuid(), None);
    }

    #[test]
    fn test_dag_uuid_metadata_with_uuid_returns_it() {
        let meta = SessionRecord::Metadata {
            uuid: Some("m1".into()),
            parent_uuid: None,
            is_sidechain: false,
        };
        let other = SessionRecord::Other {
            uuid: Some("o1".into()),
            parent_uuid: None,
            is_sidechain: false,
        };
        assert_eq!(meta.dag_uuid(), Some("m1"));
        assert_eq!(other.dag_uuid(), Some("o1"));
    }

    #[test]
    fn test_dag_parent_uuid_compact_boundary_logical_fallback() {
        let record = SessionRecord::CompactBoundary {
            uuid: Some("cb1".into()),
            parent_uuid: None,
            logical_parent_uuid: Some("u2".into()),
            is_sidechain: false,
        };
        assert_eq!(record.dag_parent_uuid(), Some("u2"));
    }

    #[test]
    fn test_dag_parent_uuid_compact_boundary_prefers_parent() {
        let record = SessionRecord::CompactBoundary {
            uuid: Some("cb1".into()),
            parent_uuid: Some("p1".into()),
            logical_parent_uuid: Some("u2".into()),
            is_sidechain: false,
        };
        assert_eq!(record.dag_parent_uuid(), Some("p1"));
    }

    #[test]
    fn test_content_blocks_non_message_returns_empty() {
        let record = SessionRecord::Summary {
            text: "test".into(),
            is_compaction: false,
            uuid: None,
            parent_uuid: None,
            leaf_uuid: None,
            is_sidechain: false,
        };
        assert!(record.content_blocks().is_empty());
    }

    #[test]
    fn test_is_sidechain_false_by_default() {
        let other = SessionRecord::Other {
            uuid: None,
            parent_uuid: None,
            is_sidechain: false,
        };
        assert!(!other.is_sidechain());
        assert!(!SessionRecord::CustomTitle("t".into()).is_sidechain());
    }

    // --- parse_content_blocks new block types ---

    #[test]
    fn test_parse_tool_result_with_array_content() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "tool_result", "content": [
                {"type": "text", "text": "file output"},
                {"type": "image"},
                {"type": "document"}
            ]}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolResult(c) => {
                assert!(c.contains("file output"));
                assert!(c.contains("[image]"));
                assert!(c.contains("[document]"));
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_tool_result_with_array_unknown_entry_type() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "tool_result", "content": [
                {"type": "text", "text": "hello"},
                {"type": "unknown_future_type", "data": "ignored"}
            ]}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolResult(c) => {
                assert_eq!(c, "hello");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_image_block() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "image", "source": {"type": "base64", "data": "..."}}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], ContentBlock::Text("[image]".to_string()));
    }

    #[test]
    fn test_parse_document_block() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "document", "source": {"type": "base64", "data": "..."}}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], ContentBlock::Text("[document]".to_string()));
    }

    #[test]
    fn test_parse_redacted_thinking_block() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "redacted_thinking"}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], ContentBlock::Thinking("[redacted]".to_string()));
    }

    #[test]
    fn test_parse_server_tool_use_block() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "server_tool_use", "name": "web_search", "id": "srvtu_123"}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolUse { name, input } => {
                assert_eq!(name, "web_search");
                assert_eq!(input, "");
            }
            other => panic!("Expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_server_tool_use_without_name() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "server_tool_use", "id": "srvtu_123"}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolUse { name, .. } => {
                assert_eq!(name, "unknown");
            }
            other => panic!("Expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_connector_text_block() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "connector_text", "text": "Connected output"}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ContentBlock::Text("Connected output".to_string())
        );
    }

    #[test]
    fn test_parse_connector_text_without_text_field() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "connector_text"}
        ]);
        let blocks = parse_content_blocks(&raw);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_parse_image_block_visible_in_text_only_mode() {
        let raw: serde_json::Value = serde_json::json!([
            {"type": "text", "text": "See this:"},
            {"type": "image", "source": {"type": "base64", "data": "..."}}
        ]);
        let blocks = parse_content_blocks(&raw);
        let text_only = SessionRecord::render_content(&blocks, &ContentMode::TextOnly);
        assert!(text_only.contains("See this:"));
        assert!(text_only.contains("[image]"));
    }

    #[test]
    fn test_render_full_server_tool_use_uses_name() {
        let blocks = vec![ContentBlock::ToolUse {
            name: "web_search".into(),
            input: String::new(),
        }];
        let result = SessionRecord::render_content(&blocks, &ContentMode::Full);
        assert_eq!(result, "web_search");
    }

    #[test]
    fn test_render_full_tool_use_with_input_uses_input() {
        let blocks = vec![ContentBlock::ToolUse {
            name: "Read".into(),
            input: r#"{"file_path":"/tmp/test.txt"}"#.into(),
        }];
        let result = SessionRecord::render_content(&blocks, &ContentMode::Full);
        assert!(result.contains("file_path"));
        assert!(!result.contains("Read"));
    }
}
