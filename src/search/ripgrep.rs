use super::Message;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// A match from ripgrep search
#[derive(Debug, Clone)]
pub struct RipgrepMatch {
    pub file_path: String,
    pub message: Option<Message>,
}

/// Search for query in JSONL files using ripgrep
pub fn search(query: &str, search_path: &str) -> Result<Vec<RipgrepMatch>, String> {
    let output = Command::new("rg")
        .args([
            "--json",
            "--glob", "*.jsonl",
            "--fixed-strings",
            "--max-count", "1000",
            query,
            search_path,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ripgrep: {}", e))?;

    let reader = BufReader::new(&output.stdout[..]);
    let mut results = Vec::new();

    for line in reader.lines() {
        if let Ok(line) = line {
            if let Some(m) = parse_ripgrep_json(&line) {
                if m.message.is_some() {
                    results.push(m);
                }
            }
        }
    }

    Ok(results)
}

/// Parse ripgrep JSON output into RipgrepMatch
pub fn parse_ripgrep_json(json: &str) -> Option<RipgrepMatch> {
    let parsed: serde_json::Value = serde_json::from_str(json).ok()?;

    // Only process "match" type entries
    let msg_type = parsed.get("type")?.as_str()?;
    if msg_type != "match" {
        return None;
    }

    let data = parsed.get("data")?;
    let file_path = data.get("path")?.get("text")?.as_str()?.to_string();
    let line_text = data.get("lines")?.get("text")?.as_str()?;
    let line_number = data.get("line_number")?.as_u64()? as usize;

    // Parse the JSONL line content
    let message = Message::from_jsonl(line_text.trim(), line_number);

    Some(RipgrepMatch { file_path, message })
}

/// Extract project name from file path
/// Path format: /Users/user/.claude/projects/-Users-user-projects-myapp/session.jsonl
pub fn extract_project_from_path(path: &str) -> String {
    // Find the project directory name (the part after "projects/")
    if let Some(projects_idx) = path.find("projects/") {
        let after_projects = &path[projects_idx + 9..]; // Skip "projects/"

        // Get the directory name (before the next /)
        let dir_name = after_projects.split('/').next().unwrap_or("");

        // The project name is the last part after splitting by -
        // e.g., "-Users-user-projects-myapp" -> "myapp"
        // e.g., "-Users-user-projects-my-cool-app" -> "my-cool-app"
        if let Some(last_projects_idx) = dir_name.rfind("-projects-") {
            return dir_name[last_projects_idx + 10..].to_string();
        }
    }

    // Fallback: return basename without extension
    path.rsplit('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".jsonl")
        .to_string()
}

/// Extract context around query match in content
/// Uses character-safe slicing to handle UTF-8 properly
pub fn extract_context(content: &str, query: &str, context_chars: usize) -> String {
    let content_lower = content.to_lowercase();
    let query_lower = query.to_lowercase();

    // Find the character position of the query (case-insensitive)
    if let Some(byte_pos) = content_lower.find(&query_lower) {
        // Convert byte position to character position
        let char_pos = content[..byte_pos].chars().count();
        let total_chars = content.chars().count();
        let query_char_len = query.chars().count();

        // Calculate character boundaries
        let start_char = char_pos.saturating_sub(context_chars);
        let end_char = (char_pos + query_char_len + context_chars).min(total_chars);

        // Extract substring using character indices
        let result: String = content
            .chars()
            .skip(start_char)
            .take(end_char - start_char)
            .collect();

        let mut output = String::new();
        if start_char > 0 {
            output.push_str("...");
        }
        output.push_str(&result);
        if end_char < total_chars {
            output.push_str("...");
        }
        output
    } else {
        // If not found, return truncated content (character-safe)
        let total_chars = content.chars().count();
        if total_chars > context_chars * 2 {
            let truncated: String = content.chars().take(context_chars * 2).collect();
            format!("{}...", truncated)
        } else {
            content.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_session(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_search_finds_matches() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello! How can I help you today?"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:01:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results = search("Hello", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");

        assert!(!results.is_empty(), "Should find matches");
        assert!(results.iter().any(|r| r.message.is_some()));
    }

    #[test]
    fn test_search_returns_empty_for_no_matches() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results = search("nonexistent_query_xyz", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");

        assert!(results.is_empty(), "Should return empty for no matches");
    }

    #[test]
    fn test_parse_ripgrep_json_match() {
        let rg_json = r#"{"type":"match","data":{"path":{"text":"/path/to/session.jsonl"},"lines":{"text":"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Hello Claude\"}]},\"sessionId\":\"abc123\",\"timestamp\":\"2025-01-09T10:00:00Z\"}\n"},"line_number":1}}"#;

        let result = parse_ripgrep_json(rg_json);

        assert!(result.is_some(), "Should parse ripgrep match");
        let m = result.unwrap();
        assert_eq!(m.file_path, "/path/to/session.jsonl");
        assert!(m.message.is_some());
        assert_eq!(m.message.as_ref().unwrap().session_id, "abc123");
    }

    #[test]
    fn test_parse_ripgrep_json_skips_non_match() {
        let rg_json = r#"{"type":"begin","data":{"path":{"text":"/path/to/session.jsonl"}}}"#;

        let result = parse_ripgrep_json(rg_json);

        assert!(result.is_none(), "Should skip non-match types");
    }

    #[test]
    fn test_extract_project_from_path() {
        let path = "/Users/user/.claude/projects/-Users-user-projects-myapp/session.jsonl";

        let project = extract_project_from_path(path);

        assert_eq!(project, "myapp");
    }

    #[test]
    fn test_extract_project_from_path_with_dashes() {
        let path = "/Users/user/.claude/projects/-Users-user-projects-my-cool-app/session.jsonl";

        let project = extract_project_from_path(path);

        assert_eq!(project, "my-cool-app");
    }

    #[test]
    fn test_extract_context_basic() {
        let content = "This is some text with the word hello in the middle of it";

        let context = extract_context(content, "hello", 10);

        assert!(context.contains("hello"), "Should contain the query");
        assert!(context.len() <= 50, "Should be reasonably short"); // includes "..." ellipsis
    }

    #[test]
    fn test_extract_context_at_start() {
        let content = "hello world this is a test";

        let context = extract_context(content, "hello", 10);

        assert!(context.starts_with("hello"), "Should start with match");
    }

    #[test]
    fn test_extract_context_case_insensitive() {
        let content = "This is HELLO world";

        let context = extract_context(content, "hello", 10);

        assert!(context.to_lowercase().contains("hello"), "Should find case-insensitive");
    }

    #[test]
    fn test_extract_context_cyrillic() {
        let content = "Сделаю: 1. Preview режим 2. Индикатор compacted";

        let context = extract_context(content, "Preview", 5);

        assert!(context.contains("Preview"), "Should find match in Cyrillic text");
        // Should not panic on UTF-8 boundaries
    }

    #[test]
    fn test_extract_context_cyrillic_truncate() {
        let content = "Привет мир это тестовая строка на русском языке";

        // Should truncate without panicking on UTF-8 boundaries
        let context = extract_context(content, "nonexistent", 10);

        assert!(!context.is_empty(), "Should return truncated content");
    }
}
