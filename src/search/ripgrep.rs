use super::{Message, SessionSource};
use regex::RegexBuilder;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// A match from ripgrep search
#[derive(Debug, Clone)]
pub struct RipgrepMatch {
    pub file_path: String,
    pub message: Option<Message>,
    pub source: SessionSource,
}

/// Search for query in JSONL files using ripgrep
/// If use_regex is true, treats query as a regex pattern; otherwise uses literal string matching
#[cfg(test)]
pub fn search(query: &str, search_path: &str) -> Result<Vec<RipgrepMatch>, String> {
    search_with_options(query, search_path, false)
}

/// Search with explicit regex mode option (single path)
#[cfg(test)]
fn search_with_options(query: &str, search_path: &str, use_regex: bool) -> Result<Vec<RipgrepMatch>, String> {
    search_multiple_paths(query, &[search_path.to_string()], use_regex)
}

/// Search multiple paths with explicit regex mode option
pub fn search_multiple_paths(query: &str, search_paths: &[String], use_regex: bool) -> Result<Vec<RipgrepMatch>, String> {
    let mut all_results = Vec::new();

    for search_path in search_paths {
        if search_path.is_empty() {
            continue;
        }

        // Check if path exists
        if !std::path::Path::new(search_path).exists() {
            continue;
        }

        let results = search_single_path(query, search_path, use_regex)?;
        all_results.extend(results);
    }

    Ok(all_results)
}

/// Search a single path
fn search_single_path(query: &str, search_path: &str, use_regex: bool) -> Result<Vec<RipgrepMatch>, String> {
    let mut args = vec![
        "--json".to_string(),
        "--glob".to_string(), "*.jsonl".to_string(),
        "--max-count".to_string(), "1000".to_string(),
    ];

    // Use fixed-strings for literal search, omit for regex
    if !use_regex {
        args.push("--fixed-strings".to_string());
    }

    args.push(query.to_string());
    args.push(search_path.to_string());

    let output = Command::new("rg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ripgrep: {}", e))?;

    let reader = BufReader::new(&output.stdout[..]);
    let mut results = Vec::new();

    // Build matcher: regex or literal (case-insensitive)
    let regex_matcher = if use_regex {
        RegexBuilder::new(query)
            .case_insensitive(true)
            .build()
            .ok()
    } else {
        None
    };
    let query_lower = query.to_lowercase();

    for line in reader.lines() {
        if let Ok(line) = line {
            if let Some(m) = parse_ripgrep_json(&line) {
                // Post-filter: only keep matches where the MESSAGE CONTENT actually contains the query
                // This filters out false positives where query matched file path or metadata
                if let Some(ref msg) = m.message {
                    let matches = if let Some(ref re) = regex_matcher {
                        re.is_match(&msg.content)
                    } else {
                        msg.content.to_lowercase().contains(&query_lower)
                    };
                    if matches {
                        results.push(m);
                    }
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
    let source = SessionSource::from_path(&file_path);

    Some(RipgrepMatch { file_path, message, source })
}

/// Extract project/session name from file path
/// CLI format: /Users/user/.claude/projects/-Users-user-projects-myapp/session.jsonl
/// Desktop format: .../local-agent-mode-sessions/.../local_xxx/.claude/projects/-sessions-cool-name/xxx.jsonl
/// Desktop audit: .../local-agent-mode-sessions/.../local_xxx/audit.jsonl
pub fn extract_project_from_path(path: &str) -> String {
    // Check for Desktop session name in path (e.g., -sessions-wizardly-vibrant-dirac)
    if let Some(sessions_idx) = path.find("-sessions-") {
        let after_sessions = &path[sessions_idx + 10..]; // Skip "-sessions-"
        // Get the name (before the next /)
        let name = after_sessions.split('/').next().unwrap_or("");
        if !name.is_empty() {
            return name.to_string();
        }
    }

    // Check for CLI project name (e.g., -Users-user-projects-myapp)
    if let Some(projects_idx) = path.find("projects/") {
        let after_projects = &path[projects_idx + 9..]; // Skip "projects/"

        // Get the directory name (before the next /)
        let dir_name = after_projects.split('/').next().unwrap_or("");

        // The project name is the last part after splitting by -projects-
        // e.g., "-Users-user-projects-myapp" -> "myapp"
        if let Some(last_projects_idx) = dir_name.rfind("-projects-") {
            return dir_name[last_projects_idx + 10..].to_string();
        }
    }

    // Desktop audit.jsonl fallback: extract session name from local_xxx part
    if path.contains("local-agent-mode-sessions") {
        // Path like: .../local_40338476-098c-4b67-b9be-4b05f12c3800/audit.jsonl
        for part in path.split('/') {
            if part.starts_with("local_") {
                // Return shortened session ID
                let session_id = part.trim_start_matches("local_");
                if session_id.len() > 8 {
                    return format!("Desktop:{}", &session_id[..8]);
                }
                return format!("Desktop:{}", session_id);
            }
        }
        return "Desktop".to_string();
    }

    // Fallback: return basename without extension
    path.rsplit('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".jsonl")
        .to_string()
}

/// Sanitize content by removing ANSI escape codes and control characters
/// This prevents terminal corruption when displaying content that contains escape sequences
pub fn sanitize_content(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        // Skip ANSI escape sequences starting with ESC
        if c == '\x1b' {
            match chars.peek() {
                // CSI sequence: ESC [ ... (letter)
                Some(&'[') => {
                    chars.next(); // consume '['
                    // Skip until we hit a letter (the terminator)
                    // This handles colors, cursor movement, etc.
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                // OSC sequence: ESC ] ... (BEL or ESC \)
                Some(&']') => {
                    chars.next(); // consume ']'
                    // Skip until BEL (\x07) or ST (ESC \)
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break; // BEL terminator
                        }
                        if next == '\x1b' {
                            // Check for ST (ESC \)
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                }
                // DEC special: ESC ( or ESC ) followed by a char
                Some(&'(') | Some(&')') => {
                    chars.next(); // consume '(' or ')'
                    chars.next(); // consume the character set selector
                }
                // SS2/SS3: ESC N or ESC O
                Some(&'N') | Some(&'O') => {
                    chars.next();
                }
                // Other single-char escapes: ESC followed by one char
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }

        // Handle carriage return
        if c == '\r' {
            // If followed by \n, convert to just \n (CRLF -> LF)
            if chars.peek() == Some(&'\n') {
                // Let the next iteration handle the \n
                continue;
            }
            // Standalone \r - replace with space to prevent cursor jumping back
            result.push(' ');
            continue;
        }

        // Skip control characters except newline and tab
        if c.is_control() && c != '\n' && c != '\t' {
            continue;
        }

        result.push(c);
    }

    result
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
    fn test_search_filters_metadata_matches() {
        // Test that searching for "abc123" (which is in sessionId) does NOT return results
        // because the actual message content doesn't contain "abc123"
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        // Search for sessionId which appears in metadata but NOT in message content
        let results = search("abc123", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");

        assert!(results.is_empty(), "Should NOT match sessionId, only message content");
    }

    #[test]
    fn test_search_matches_content_only() {
        let temp_dir = TempDir::new().unwrap();
        // Message content contains "warmup" but session ID contains "adb"
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Running warmup"}]},"sessionId":"adb-test-session","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        // Search for "adb" - should NOT match because it's only in sessionId
        let adb_results = search("adb", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");
        assert!(adb_results.is_empty(), "Should NOT match 'adb' in sessionId");

        // Search for "warmup" - should match because it's in content
        let warmup_results = search("warmup", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");
        assert!(!warmup_results.is_empty(), "Should match 'warmup' in content");
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
    fn test_extract_project_from_desktop_session_name() {
        // Desktop nested path with session name
        let path = "/Users/user/Library/Application Support/Claude/local-agent-mode-sessions/uuid1/uuid2/local_xxx/.claude/projects/-sessions-wizardly-vibrant-dirac/session.jsonl";

        let project = extract_project_from_path(path);

        assert_eq!(project, "wizardly-vibrant-dirac");
    }

    #[test]
    fn test_extract_project_from_desktop_audit() {
        // Desktop audit.jsonl without session name
        let path = "/Users/user/Library/Application Support/Claude/local-agent-mode-sessions/uuid1/uuid2/local_40338476-098c-4b67-b9be-4b05f12c3800/audit.jsonl";

        let project = extract_project_from_path(path);

        assert_eq!(project, "Desktop:40338476");
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

    #[test]
    fn test_sanitize_content_plain_text() {
        let content = "Hello world";
        let result = sanitize_content(content);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_sanitize_content_ansi_color() {
        // Red text: ESC[31m Hello ESC[0m
        let content = "\x1b[31mHello\x1b[0m world";
        let result = sanitize_content(content);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_sanitize_content_ansi_cursor_movement() {
        // Cursor movement sequences like ESC[2J (clear screen), ESC[H (home)
        let content = "\x1b[2J\x1b[HHello world";
        let result = sanitize_content(content);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_sanitize_content_control_chars() {
        // Control characters like backspace, bell
        let content = "Hello\x08\x07 world";
        let result = sanitize_content(content);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_sanitize_content_preserves_newlines() {
        let content = "Hello\nworld\ttab";
        let result = sanitize_content(content);
        assert_eq!(result, "Hello\nworld\ttab");
    }

    #[test]
    fn test_sanitize_content_complex_ansi() {
        // Multiple ANSI codes: bold, color, reset
        let content = "\x1b[1m\x1b[32mGreen Bold\x1b[0m Normal";
        let result = sanitize_content(content);
        assert_eq!(result, "Green Bold Normal");
    }

    #[test]
    fn test_sanitize_content_preserves_cyrillic() {
        let content = "\x1b[31mПривет\x1b[0m мир";
        let result = sanitize_content(content);
        assert_eq!(result, "Привет мир");
    }

    #[test]
    fn test_sanitize_content_ansi_256_color() {
        // 256 color: ESC[38;5;123m
        let content = "\x1b[38;5;123mColored\x1b[0m text";
        let result = sanitize_content(content);
        assert_eq!(result, "Colored text");
    }

    #[test]
    fn test_sanitize_content_carriage_return() {
        // Carriage return without newline overwrites text
        let content = "Hello\rWorld";
        let result = sanitize_content(content);
        // Should strip \r to prevent cursor jumping back
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_sanitize_content_crlf_to_lf() {
        // Windows line endings should become Unix line endings
        let content = "Hello\r\nWorld";
        let result = sanitize_content(content);
        assert_eq!(result, "Hello\nWorld");
    }

    #[test]
    fn test_sanitize_content_multiple_carriage_returns() {
        // Multiple carriage returns in sequence - each becomes a space
        let content = "First\r\r\rSecond";
        let result = sanitize_content(content);
        assert_eq!(result, "First   Second");
    }

    #[test]
    fn test_sanitize_content_osc_sequence() {
        // Operating System Command: ESC ] ... BEL
        let content = "\x1b]0;Window Title\x07Normal text";
        let result = sanitize_content(content);
        assert_eq!(result, "Normal text");
    }

    #[test]
    fn test_sanitize_content_osc_st_terminator() {
        // OSC with ST terminator: ESC ] ... ESC \
        let content = "\x1b]0;Title\x1b\\Normal text";
        let result = sanitize_content(content);
        assert_eq!(result, "Normal text");
    }

    #[test]
    fn test_sanitize_content_cursor_position() {
        // Cursor position: ESC[row;colH
        let content = "\x1b[10;20HText at position";
        let result = sanitize_content(content);
        assert_eq!(result, "Text at position");
    }

    #[test]
    fn test_sanitize_content_private_csi() {
        // Private CSI: ESC[?...
        let content = "\x1b[?25lHidden cursor\x1b[?25h";
        let result = sanitize_content(content);
        assert_eq!(result, "Hidden cursor");
    }

    #[test]
    fn test_sanitize_content_dec_special() {
        // DEC special: ESC ( or ESC )
        let content = "\x1b(0Line drawing\x1b(B";
        let result = sanitize_content(content);
        assert_eq!(result, "Line drawing");
    }
}
