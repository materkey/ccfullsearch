use super::Message;
use crate::session::resolve_parent_session;
use crate::session::SessionSource;
use regex::RegexBuilder;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;

static DESKTOP_TITLE_CACHE: std::sync::LazyLock<Mutex<HashMap<String, Option<String>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// A match from ripgrep search
#[derive(Debug, Clone)]
pub struct RipgrepMatch {
    pub file_path: String,
    pub message: Option<Message>,
    pub source: SessionSource,
}

/// Result of a search operation.
#[derive(Debug)]
#[non_exhaustive]
pub struct SearchResult {
    /// The matches found by the search.
    pub matches: Vec<RipgrepMatch>,
    /// Whether any file hit the per-file match limit, meaning results may be incomplete.
    pub truncated: bool,
}

fn build_regex_matcher(query: &str, use_regex: bool) -> Result<Option<regex::Regex>, String> {
    if use_regex {
        RegexBuilder::new(query)
            .case_insensitive(true)
            .build()
            .map(Some)
            .map_err(|e| format!("Invalid regex '{query}': {e}"))
    } else {
        Ok(None)
    }
}

fn build_ripgrep_args(query: &str, search_path: &str, use_regex: bool) -> Vec<String> {
    let mut args = vec![
        "--json".to_string(),
        "--glob".to_string(),
        "*.jsonl".to_string(),
        "--max-count".to_string(),
        MAX_COUNT_PER_FILE.to_string(),
        "--ignore-case".to_string(),
    ];

    if !use_regex {
        args.push("--fixed-strings".to_string());
    }

    let prefilter_patterns = build_prefilter_patterns(query, use_regex);
    if prefilter_patterns.len() == 1 {
        args.push(query.to_string());
    } else {
        for pattern in prefilter_patterns {
            args.push("-e".to_string());
            args.push(pattern);
        }
    }
    args.push(search_path.to_string());
    args
}

fn build_prefilter_patterns(query: &str, use_regex: bool) -> Vec<String> {
    let mut patterns = vec![query.to_string()];

    if use_regex {
        if is_regex_slash_command_query(query) {
            push_command_tag_prefilters(&mut patterns);
        }
        return patterns;
    }

    let Some(command_head) = slash_command_search_head(query) else {
        return patterns;
    };

    if command_head.is_empty() {
        push_command_tag_prefilters(&mut patterns);
        return patterns;
    }

    push_unique(&mut patterns, &format!("<command-message>{command_head}"));
    push_unique(&mut patterns, &format!("<command-name>{command_head}"));
    patterns
}

fn is_regex_slash_command_query(query: &str) -> bool {
    query.starts_with('/') || query.starts_with("^/")
}

fn slash_command_search_head(query: &str) -> Option<&str> {
    let command = query.strip_prefix('/')?;
    Some(command.split_whitespace().next().unwrap_or(""))
}

fn push_command_tag_prefilters(patterns: &mut Vec<String>) {
    push_unique(patterns, "<command-message>");
    push_unique(patterns, "<command-name>");
}

fn push_unique(patterns: &mut Vec<String>, pattern: &str) {
    if !patterns.iter().any(|existing| existing == pattern) {
        patterns.push(pattern.to_string());
    }
}

/// Search for query in JSONL files using ripgrep (test helper, discards truncation flag)
#[cfg(test)]
pub fn search(query: &str, search_path: &str) -> Result<Vec<RipgrepMatch>, String> {
    search_with_options(query, search_path, false)
}

/// Search with explicit regex mode option (single path, test helper, discards truncation flag)
#[cfg(test)]
fn search_with_options(
    query: &str,
    search_path: &str,
    use_regex: bool,
) -> Result<Vec<RipgrepMatch>, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    search_multiple_paths(query, &[search_path.to_string()], use_regex, &cancel)
        .map(|result| result.matches)
}

/// Search multiple paths with explicit regex mode option.
/// Returns a `SearchResult` with matches and a truncation flag.
///
/// `cancel` is a cooperative cancellation token: when set to `true` before invocation
/// the function returns `Err("cancelled")` immediately without spawning ripgrep; when
/// flipped mid-flight the spawned `rg` child is killed and the function returns
/// `Err("cancelled")` after the child is reaped.
pub fn search_multiple_paths(
    query: &str,
    search_paths: &[String],
    use_regex: bool,
    cancel: &Arc<AtomicBool>,
) -> Result<SearchResult, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    // Build the regex matcher once and reuse it across every path. Returns
    // a fast `Invalid regex` error before spawning ripgrep when `use_regex`
    // is true and `query` doesn't compile.
    let regex_matcher = build_regex_matcher(query, use_regex)?;

    let mut all_results = Vec::new();
    let mut any_truncated = false;

    for search_path in search_paths {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }

        if search_path.is_empty() {
            continue;
        }

        if !std::path::Path::new(search_path).exists() {
            continue;
        }

        let (results, truncated) = search_single_path(
            query,
            search_path,
            use_regex,
            regex_matcher.as_ref(),
            cancel,
        )?;
        all_results.extend(results);
        any_truncated |= truncated;
    }

    Ok(SearchResult {
        matches: all_results,
        truncated: any_truncated,
    })
}

const MAX_COUNT_PER_FILE: usize = 1000;

/// Maximum bytes of `rg` stderr we keep in memory for the failure-message
/// diagnostic. Beyond this we keep draining the pipe (so the child does not
/// block on a full pipe buffer) but discard the additional bytes. 64 KiB is
/// roughly the Linux pipe buffer size and far more than typical `rg`
/// warning output; truncated diagnostics are noted explicitly in the error.
const STDERR_KEEP_BYTES: usize = 64 * 1024;

/// Search a single path.
/// Returns (matches, truncated) where truncated is true if any file hit the per-file match limit.
///
/// Streams ripgrep stdout line-by-line so memory does not balloon on broad queries
/// (e.g. single-character searches against a multi-GB corpus). On each line the
/// `cancel` flag is polled; when set, the spawned child is killed, reaped, and the
/// function returns `Err("cancelled")`.
fn search_single_path(
    query: &str,
    search_path: &str,
    use_regex: bool,
    regex_matcher: Option<&regex::Regex>,
    cancel: &Arc<AtomicBool>,
) -> Result<(Vec<RipgrepMatch>, bool), String> {
    let args = build_ripgrep_args(query, search_path, use_regex);

    let mut child = Command::new("rg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run ripgrep: {}", e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ripgrep stdout was not captured".to_string())?;
    let reader = BufReader::new(stdout);

    // Drain stderr on a dedicated thread so a chatty `rg` (warnings about
    // unreadable files, regex diagnostics, etc.) cannot fill the pipe buffer
    // (~64 KiB on Linux) and block the child while we are stuck waiting for
    // stdout / `child.wait()`. We cap the *retained* bytes at
    // `STDERR_KEEP_BYTES` (~64 KiB) — beyond that we keep reading (and
    // discarding) so the pipe never fills, but stop appending. The
    // captured prefix is surfaced in any failure message, restoring the
    // diagnostics that the previous `Command::output()` path included.
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ripgrep stderr was not captured".to_string())?;
    let stderr_handle = thread::spawn(move || {
        let mut keep: Vec<u8> = Vec::new();
        let mut discard = [0u8; 8 * 1024];
        let mut reader = BufReader::new(stderr);
        let mut truncated = false;
        // First, fill `keep` up to the cap.
        while keep.len() < STDERR_KEEP_BYTES {
            let remaining = STDERR_KEEP_BYTES - keep.len();
            let buf_len = discard.len().min(remaining);
            match reader.read(&mut discard[..buf_len]) {
                Ok(0) => return (keep, false),
                Ok(n) => keep.extend_from_slice(&discard[..n]),
                Err(_) => return (keep, false),
            }
        }
        // Then, drain (and discard) anything else so the pipe never blocks.
        loop {
            match reader.read(&mut discard) {
                Ok(0) => break,
                Ok(_) => truncated = true,
                Err(_) => break,
            }
        }
        (keep, truncated)
    });

    let mut results = Vec::new();
    let mut truncated = false;
    let mut file_match_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    let query_lower = query.to_lowercase();
    let mut resolve_cache: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();

    for line in reader.lines().map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            // Reap the drainer so we do not leak the thread or its pipe end.
            let _ = stderr_handle.join();
            return Err("cancelled".into());
        }

        if let Some(mut m) = parse_ripgrep_json(&line) {
            // Track raw ripgrep matches per file to detect --max-count truncation.
            // Uses a HashMap so interleaved multi-threaded ripgrep output is handled
            // correctly. Count uses the original file path (before agent resolution)
            // because that is what ripgrep's --max-count applies to.
            let count = file_match_counts.entry(m.file_path.clone()).or_insert(0);
            *count += 1;
            if *count >= MAX_COUNT_PER_FILE {
                truncated = true;
            }

            // Resolve agent/subagent files to their parent session
            if is_agent_or_subagent_path(&m.file_path) {
                let Some(ref msg) = m.message else {
                    continue; // No message to resolve — skip
                };
                let (resolved_sid, resolved_path) = resolve_cache
                    .entry(m.file_path.clone())
                    .or_insert_with(|| resolve_parent_session(&msg.session_id, &m.file_path))
                    .clone();
                m.file_path = resolved_path;
                if let Some(msg) = m.message.as_mut() {
                    msg.session_id = resolved_sid;
                }
            }
            // Post-filter: only keep matches where the MESSAGE CONTENT actually contains the query
            // This filters out false positives where query matched file path or metadata
            if let Some(ref msg) = m.message {
                let matches = if let Some(re) = regex_matcher {
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

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for ripgrep: {}", e))?;

    // Wait for the stderr drainer to finish; the child has exited so the
    // pipe is closed and `read_to_end` returns. Failures here only mean the
    // thread panicked, which is not actionable for the caller.
    let (stderr_bytes, stderr_truncated) = stderr_handle.join().unwrap_or_default();

    // ripgrep exit code 1 means "no matches" — that is success for our purposes.
    if !status.success() && status.code() != Some(1) {
        let stderr_text = String::from_utf8_lossy(&stderr_bytes);
        let trimmed = stderr_text.trim();
        let suffix = if stderr_truncated {
            " [stderr truncated]"
        } else {
            ""
        };
        if trimmed.is_empty() {
            return Err(format!(
                "ripgrep search failed: exit status {}{}",
                status, suffix
            ));
        }
        return Err(format!(
            "ripgrep search failed: exit status {}: {}{}",
            status, trimmed, suffix
        ));
    }

    Ok((results, truncated))
}

/// Check if a file path belongs to an agent or subagent session.
/// Returns true for paths containing `/subagents/` or filenames starting with `agent-`.
fn is_agent_or_subagent_path(path: &str) -> bool {
    if path.contains("/subagents/") {
        return true;
    }
    if let Some(filename) = path.rsplit('/').next() {
        if filename.starts_with("agent-") {
            return true;
        }
    }
    false
}

/// Parse ripgrep JSON output into RipgrepMatch
pub fn parse_ripgrep_json(json: &str) -> Option<RipgrepMatch> {
    let parsed: serde_json::Value = serde_json::from_str(json).ok()?;

    let msg_type = parsed.get("type")?.as_str()?;
    if msg_type != "match" {
        return None;
    }

    let data = parsed.get("data")?;
    let file_path = data.get("path")?.get("text")?.as_str()?.to_string();
    let line_text = data.get("lines")?.get("text")?.as_str()?;
    let line_number = data.get("line_number")?.as_u64()? as usize;

    let message = Message::from_jsonl(line_text.trim(), line_number);
    let source = SessionSource::from_path(&file_path);

    Some(RipgrepMatch {
        file_path,
        message,
        source,
    })
}

/// Decode an encoded directory name into a short readable project label.
/// Strips common home-dir prefixes and converts `--` to `/.` (hidden dirs), `-` to `/`.
/// Examples:
///   "-Users-user" -> "~"
///   "-Users-user--claude-skills-gist" -> "~/.claude/skills/gist"
///   "-private-tmp" -> "/private/tmp"
fn decode_dir_name_short(dir_name: &str) -> String {
    let stripped = dir_name.strip_prefix('-').unwrap_or(dir_name);
    let decoded = stripped
        .replace("--", "\x00")
        .replace('-', "/")
        .replace('\x00', "/.");
    let full_path = format!("/{}", decoded);

    // Try to shorten home directory prefix to ~
    if let Some(home) = dirs::home_dir() {
        if let Some(home_str) = home.to_str() {
            if let Some(rest) = full_path.strip_prefix(home_str) {
                if rest.is_empty() {
                    return "~".to_string();
                }
                return format!("~{}", rest);
            }
        }
    }

    full_path
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

        if !dir_name.is_empty() {
            // The project name is the last part after splitting by -projects-
            // e.g., "-Users-user-projects-myapp" -> "myapp"
            if let Some(last_projects_idx) = dir_name.rfind("-projects-") {
                return dir_name[last_projects_idx + 10..].to_string();
            }

            // No -projects- segment: extract last meaningful part from encoded dir name
            // e.g., "-Users-user" -> "~"
            // e.g., "-Users-user--claude-skills-gist" -> "~/.claude/skills/gist"
            // e.g., "-private-tmp" -> "/tmp"
            return decode_dir_name_short(dir_name);
        }
    }

    // Desktop audit.jsonl: try to read title from sibling JSON metadata file
    if path.contains("local-agent-mode-sessions") {
        if let Some(title) = read_desktop_session_title(path) {
            return title;
        }

        // Fallback: extract from local_xxx part
        for part in path.split('/') {
            if part.starts_with("local_") {
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

/// Read Desktop session title from the sibling JSON metadata file (cached).
/// Path: .../local_xxx/audit.jsonl -> .../local_xxx.json
fn read_desktop_session_title(audit_path: &str) -> Option<String> {
    if let Ok(cache) = DESKTOP_TITLE_CACHE.lock() {
        if let Some(cached) = cache.get(audit_path) {
            return cached.clone();
        }
    }

    let result = read_desktop_session_title_uncached(audit_path);

    if let Ok(mut cache) = DESKTOP_TITLE_CACHE.lock() {
        cache.insert(audit_path.to_string(), result.clone());
    }

    result
}

fn read_desktop_session_title_uncached(audit_path: &str) -> Option<String> {
    use std::path::Path;

    let path = Path::new(audit_path);

    let local_dir = path.parent()?;
    let local_dir_name = local_dir.file_name()?.to_str()?;

    let parent_of_local = local_dir.parent()?;
    let json_path = parent_of_local.join(format!("{}.json", local_dir_name));

    let content = std::fs::read_to_string(&json_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    json.get("title")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
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
    fn test_build_ripgrep_args_fixed_string_search_is_case_insensitive() {
        let args = build_ripgrep_args("hello", "/tmp/search", false);
        assert!(args.contains(&"--ignore-case".to_string()));
        assert!(args.contains(&"--fixed-strings".to_string()));
        assert_eq!(args.last(), Some(&"/tmp/search".to_string()));
    }

    #[test]
    fn test_build_ripgrep_args_regex_search_is_case_insensitive() {
        let args = build_ripgrep_args("hello.*world", "/tmp/search", true);
        assert!(args.contains(&"--ignore-case".to_string()));
        assert!(!args.contains(&"--fixed-strings".to_string()));
    }

    #[test]
    fn test_search_multiple_paths_invalid_regex_returns_error() {
        let cancel = Arc::new(AtomicBool::new(false));
        let result = search_multiple_paths(
            "(",
            &["/path/that/does/not/exist".to_string()],
            true,
            &cancel,
        );
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.contains("Invalid regex"), "unexpected error: {}", err);
    }

    #[test]
    fn test_search_finds_matches() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello! How can I help you today?"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:01:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results =
            search("Hello", temp_dir.path().to_str().unwrap()).expect("Search should succeed");

        assert!(!results.is_empty(), "Should find matches");
        assert!(results.iter().any(|r| r.message.is_some()));
    }

    #[test]
    fn test_search_is_case_insensitive() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results =
            search("hello", temp_dir.path().to_str().unwrap()).expect("Search should succeed");

        assert!(!results.is_empty(), "Should match regardless of case");
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
        let results =
            search("abc123", temp_dir.path().to_str().unwrap()).expect("Search should succeed");

        assert!(
            results.is_empty(),
            "Should NOT match sessionId, only message content"
        );
    }

    #[test]
    fn test_search_matches_content_only() {
        let temp_dir = TempDir::new().unwrap();
        // Message content contains "warmup" but session ID contains "adb"
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Running warmup"}]},"sessionId":"adb-test-session","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        // Search for "adb" - should NOT match because it's only in sessionId
        let adb_results =
            search("adb", temp_dir.path().to_str().unwrap()).expect("Search should succeed");
        assert!(
            adb_results.is_empty(),
            "Should NOT match 'adb' in sessionId"
        );

        // Search for "warmup" - should match because it's in content
        let warmup_results =
            search("warmup", temp_dir.path().to_str().unwrap()).expect("Search should succeed");
        assert!(
            !warmup_results.is_empty(),
            "Should match 'warmup' in content"
        );
    }

    #[test]
    fn test_search_finds_command_message_by_rendered_slash_command() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<command-message>revdiff:revdiff</command-message><command-args></command-args>"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results = search("/revdiff:revdiff", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].message.as_ref().unwrap().content,
            "/revdiff:revdiff"
        );
    }

    #[test]
    fn test_search_finds_command_name_args_by_rendered_slash_command() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<command-name>foo</command-name><command-args>bar baz</command-args>"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results =
            search("/foo bar", temp_dir.path().to_str().unwrap()).expect("Search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message.as_ref().unwrap().content, "/foo bar baz");
    }

    #[test]
    fn test_search_regex_finds_command_name_args_by_rendered_slash_command() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<command-name>foo</command-name><command-args>bar baz</command-args>"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results = search_with_options("^/foo bar", temp_dir.path().to_str().unwrap(), true)
            .expect("Search should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message.as_ref().unwrap().content, "/foo bar baz");
    }

    #[test]
    fn test_search_command_prefilter_keeps_normalized_post_filter() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<command-name>foo</command-name><command-args>different</command-args>"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;

        create_test_session(&temp_dir, "session.jsonl", session_content);

        let results =
            search("/foo bar", temp_dir.path().to_str().unwrap()).expect("Search should succeed");

        assert!(
            results.is_empty(),
            "raw command tag prefilter must not bypass normalized content filtering"
        );
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

        assert!(
            context.to_lowercase().contains("hello"),
            "Should find case-insensitive"
        );
    }

    #[test]
    fn test_extract_context_cyrillic() {
        let content = "Сделаю: 1. Preview режим 2. Индикатор compacted";

        let context = extract_context(content, "Preview", 5);

        assert!(
            context.contains("Preview"),
            "Should find match in Cyrillic text"
        );
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

    #[test]
    fn test_search_resolves_agent_files_to_parent() {
        let temp_dir = TempDir::new().unwrap();
        let parent_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Parent session"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;
        let agent_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Agent unique content"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:01:00Z"}"#;

        // Create parent session file and an agent file with same sessionId
        create_test_session(&temp_dir, "abc123.jsonl", parent_content);
        create_test_session(&temp_dir, "agent-task1.jsonl", agent_content);

        let results = search("Agent unique content", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");

        // Match from agent file should be resolved to parent session
        assert!(!results.is_empty(), "Should find match from agent file");
        for r in &results {
            assert!(
                r.file_path.contains("abc123.jsonl"),
                "Agent match should resolve to parent file, got: {}",
                r.file_path
            );
            assert!(
                !r.file_path.contains("agent-"),
                "File path should not be the agent file, got: {}",
                r.file_path
            );
        }
    }

    #[test]
    fn test_search_resolves_subagent_files_to_parent() {
        let temp_dir = TempDir::new().unwrap();
        let parent_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Parent session"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;
        let agent_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Subagent unique content"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:01:00Z"}"#;

        // Create parent session file and a subagent file
        create_test_session(&temp_dir, "abc123.jsonl", parent_content);
        let subagents_dir = temp_dir.path().join("abc123").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let subagent_path = subagents_dir.join("agent-xyz.jsonl");
        std::fs::write(&subagent_path, agent_content).unwrap();

        let results = search("Subagent unique content", temp_dir.path().to_str().unwrap())
            .expect("Search should succeed");

        // Match from subagent file should resolve to parent session
        assert!(!results.is_empty(), "Should find match from subagent file");
        for r in &results {
            assert!(
                r.file_path.contains("abc123.jsonl"),
                "Subagent match should resolve to parent file, got: {}",
                r.file_path
            );
            assert!(
                !r.file_path.contains("/subagents/"),
                "File path should not be the subagent file, got: {}",
                r.file_path
            );
        }
    }

    #[test]
    fn test_is_agent_or_subagent_path() {
        assert!(is_agent_or_subagent_path(
            "/home/user/.claude/projects/foo/agent-task1.jsonl"
        ));
        assert!(is_agent_or_subagent_path(
            "/home/user/.claude/projects/foo/abc123/subagents/sub.jsonl"
        ));
        assert!(!is_agent_or_subagent_path(
            "/home/user/.claude/projects/foo/session.jsonl"
        ));
        assert!(!is_agent_or_subagent_path(
            "/home/user/.claude/projects/foo/abc123.jsonl"
        ));
    }

    #[test]
    fn test_search_returns_truncated_flag_when_max_count_hit() {
        let temp_dir = TempDir::new().unwrap();

        // Create a JSONL file with MAX_COUNT_PER_FILE + 10 lines all containing the query.
        // ripgrep --max-count will stop at MAX_COUNT_PER_FILE matches, and we should detect truncation.
        let mut content = String::new();
        for i in 0..(MAX_COUNT_PER_FILE + 10) {
            content.push_str(&format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"findme line {}"}}]}},"sessionId":"sess1","timestamp":"2025-01-09T10:00:{:02}Z"}}"#,
                i, i % 60
            ));
            content.push('\n');
        }
        create_test_session(&temp_dir, "big_session.jsonl", &content);

        let cancel = Arc::new(AtomicBool::new(false));
        let (_, truncated) = search_single_path(
            "findme",
            temp_dir.path().to_str().unwrap(),
            false,
            None,
            &cancel,
        )
        .expect("Search should succeed");

        assert!(
            truncated,
            "Should detect truncation when file has more matches than max-count"
        );
    }

    #[test]
    fn test_search_no_truncation_flag_for_small_results() {
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;
        create_test_session(&temp_dir, "session.jsonl", session_content);

        let cancel = Arc::new(AtomicBool::new(false));
        let (_, truncated) = search_single_path(
            "Hello",
            temp_dir.path().to_str().unwrap(),
            false,
            None,
            &cancel,
        )
        .expect("Search should succeed");

        assert!(
            !truncated,
            "Should not flag truncation for small result sets"
        );
    }

    #[test]
    fn test_search_multiple_paths_cancel_before_invocation_returns_immediately() {
        // When the cancel flag is already set, the function must return Err("cancelled")
        // without spawning ripgrep. We rely on the early-exit check at the top of
        // search_multiple_paths — if it weren't there the bogus path below would still
        // be silently skipped (existence check), so we add a real path too: even when
        // a real fixture is present, no work should happen because cancel was preset.
        let temp_dir = TempDir::new().unwrap();
        let session_content = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello Claude"}]},"sessionId":"abc123","timestamp":"2025-01-09T10:00:00Z"}"#;
        create_test_session(&temp_dir, "session.jsonl", session_content);

        let cancel = Arc::new(AtomicBool::new(true));
        let start = std::time::Instant::now();
        let result = search_multiple_paths(
            "Hello",
            &[temp_dir.path().to_str().unwrap().to_string()],
            false,
            &cancel,
        );
        let elapsed = start.elapsed();

        assert!(result.is_err(), "Pre-cancelled search must return Err");
        let err = result.err().unwrap();
        assert_eq!(err, "cancelled", "unexpected error: {}", err);
        // Pre-cancelled path should be effectively instant (no rg spawn).
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "Pre-cancelled search took too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_search_mid_flight_cancellation_kills_child() {
        // Build a large fixture so an uncancelled run takes long enough for the
        // cancel-mid-flight check to be meaningful. We spread matching lines across
        // many files (ripgrep's --max-count caps per-file output at MAX_COUNT_PER_FILE,
        // so a single huge file would simply truncate quickly). With 60 files of
        // ~MAX_COUNT_PER_FILE matching lines each, ripgrep streams ~60k JSON
        // records that we parse + post-filter in user space — wall-clock for this
        // is on the order of 100 ms even on M-series CPUs in release mode.
        let temp_dir = TempDir::new().unwrap();
        let line_template = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"findme PADDING_PADDING_PADDING_PADDING line "#;
        for f in 0..60 {
            let mut content = String::with_capacity(200 * MAX_COUNT_PER_FILE);
            for i in 0..MAX_COUNT_PER_FILE {
                content.push_str(line_template);
                content.push_str(&format!(
                    r#"{}-{}"}}]}},"sessionId":"sess{}","timestamp":"2025-01-09T10:00:00Z"}}"#,
                    f, i, f
                ));
                content.push('\n');
            }
            create_test_session(&temp_dir, &format!("huge_{:02}.jsonl", f), &content);
        }

        // Measure baseline so we can reason about whether the cancel actually
        // stopped work instead of arriving after the search naturally finished.
        let baseline_cancel = Arc::new(AtomicBool::new(false));
        let baseline_start = std::time::Instant::now();
        let baseline = search_multiple_paths(
            "findme",
            &[temp_dir.path().to_str().unwrap().to_string()],
            false,
            &baseline_cancel,
        )
        .expect("baseline search should succeed");
        let baseline_elapsed = baseline_start.elapsed();
        assert!(
            !baseline.matches.is_empty(),
            "fixture should produce matches"
        );

        // Sanity-check that the baseline was substantial enough for our
        // cancel-during-iteration window to be meaningful. If the baseline
        // is somehow under 20 ms (truly extreme hardware), the test below
        // would race; surface that with a clear message rather than a
        // confusing cancel-not-observed failure.
        assert!(
            baseline_elapsed >= std::time::Duration::from_millis(20),
            "fixture is too small for the cancel-mid-flight assertion to be meaningful (baseline={:?}); enlarge the fixture",
            baseline_elapsed
        );

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_worker = cancel.clone();
        let path = temp_dir.path().to_str().unwrap().to_string();

        let handle = std::thread::spawn(move || {
            search_multiple_paths("findme", &[path], false, &cancel_for_worker)
        });

        // Cancel after a fraction of the observed baseline (or 1 ms,
        // whichever is larger). The fraction ensures even fast hosts give
        // the worker enough time to spawn `rg` and start the read loop —
        // we want to land inside `BufReader::lines()`, not before the
        // child has been spawned. 1/4 of baseline is conservative; the
        // baseline assertion above guarantees this is at least 5 ms.
        let cancel_after = std::cmp::max(std::time::Duration::from_millis(1), baseline_elapsed / 4);
        std::thread::sleep(cancel_after);
        cancel.store(true, Ordering::Relaxed);

        let join_start = std::time::Instant::now();
        let result = handle.join().expect("worker thread panicked");
        let join_elapsed = join_start.elapsed();

        assert!(
            result.is_err(),
            "Mid-flight cancelled search must return Err (baseline took {:?}, cancel_after={:?}, join took {:?})",
            baseline_elapsed,
            cancel_after,
            join_elapsed
        );
        assert_eq!(result.err().unwrap(), "cancelled");
        // After cancel, the worker should drop out of its read loop quickly because
        // the rg child was killed and stdout EOF'd. The deadline is a generous
        // 500 ms — the actual time is typically a couple of ms.
        assert!(
            join_elapsed < std::time::Duration::from_millis(500),
            "Cancelled search did not unwind quickly: join took {:?}, baseline {:?}, cancel_after {:?}",
            join_elapsed,
            baseline_elapsed,
            cancel_after
        );
    }

    /// Probe the actual pipe buffer size on Linux via `F_GETPIPE_SZ` so
    /// the chatty-stderr test can scale its fixture accordingly. On
    /// non-Linux unix (macOS, BSD) and on probe failures the canonical
    /// 64 KiB Linux default is returned.
    #[cfg(unix)]
    fn probe_pipe_buffer_bytes() -> usize {
        const DEFAULT_PIPE_BUF: usize = 64 * 1024;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
            // SAFETY: `pipe2` writes two valid file descriptors into `fds`
            // on success. We immediately wrap them in `OwnedFd` so they
            // are closed on drop even if `fcntl` fails.
            let mut fds: [libc::c_int; 2] = [0; 2];
            let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
            if rc != 0 {
                return DEFAULT_PIPE_BUF;
            }
            let _read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
            let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
            let size = unsafe { libc::fcntl(write.as_raw_fd(), libc::F_GETPIPE_SZ) };
            if size > 0 {
                size as usize
            } else {
                DEFAULT_PIPE_BUF
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            DEFAULT_PIPE_BUF
        }
    }

    #[test]
    fn test_search_does_not_deadlock_on_chatty_stderr() {
        // Regression for a stderr-deadlock: when `rg` produces enough
        // stderr output to fill the pipe buffer, it blocks on `write()`
        // until something drains the pipe. The wrapper must drain stderr
        // concurrently with stdout, otherwise the child hangs forever and
        // `child.wait()` never returns.
        //
        // We trigger chatty stderr by creating many `*.jsonl` files with
        // mode `0o000` so the user (running this test) cannot read them.
        // `rg` prints "Permission denied (os error 13)" once per file.
        // The number of files is sized off the *actual* pipe buffer
        // (probed via `F_GETPIPE_SZ` on Linux, falling back to the
        // canonical 64 KiB default on macOS/BSD/probe failure) so the
        // fixture deterministically clears the buffer even on hosts with
        // larger pipes (some BSDs configure 1 MiB).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let temp_dir = TempDir::new().unwrap();
            // Verify chmod 0o000 actually blocks reads for the test
            // runner (root bypasses POSIX read perms; in that case rg
            // produces no Permission-denied lines and the deadlock
            // condition cannot be triggered, so we skip).
            let probe = temp_dir.path().join("__perm_probe.jsonl");
            std::fs::write(&probe, "{}\n").unwrap();
            let mut probe_perms = std::fs::metadata(&probe).unwrap().permissions();
            probe_perms.set_mode(0o000);
            std::fs::set_permissions(&probe, probe_perms).unwrap();
            let probe_readable = std::fs::read_to_string(&probe).is_ok();
            // Restore so the file can be deleted later by TempDir.
            let mut restore = std::fs::metadata(&probe).unwrap().permissions();
            restore.set_mode(0o644);
            let _ = std::fs::set_permissions(&probe, restore);
            let _ = std::fs::remove_file(&probe);
            if probe_readable {
                eprintln!(
                    "skipping test_search_does_not_deadlock_on_chatty_stderr: chmod 0o000 did not block reads (likely running as root)"
                );
                return;
            }

            // Each `rg` "Permission denied" line is roughly 80 bytes
            // (path + boilerplate). Write ~2x the actual pipe buffer so
            // we are guaranteed to exceed it even on systems with larger
            // pipes. Floor at 1500 files to preserve the original
            // fixture's strength on the common 64 KiB Linux/macOS case.
            const APPROX_LINE_BYTES: usize = 80;
            let pipe_buf = probe_pipe_buffer_bytes();
            let target_stderr_bytes = pipe_buf.saturating_mul(2);
            let file_count = (target_stderr_bytes / APPROX_LINE_BYTES).max(1500);
            for i in 0..file_count {
                let path = temp_dir.path().join(format!(
                    "unreadable_with_a_long_filename_to_pad_stderr_{:06}.jsonl",
                    i
                ));
                std::fs::write(&path, "{\"type\":\"summary\"}\n").unwrap();
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o000);
                std::fs::set_permissions(&path, perms).unwrap();
            }

            let cancel = Arc::new(AtomicBool::new(false));
            let start = std::time::Instant::now();
            // The search itself will not match anything but must complete.
            // The 30 s deadline is generous; the actual time on a working
            // implementation is well under 1 s. Without the stderr
            // drain, the old code blocked indefinitely.
            let result = search_multiple_paths(
                "findme",
                &[temp_dir.path().to_str().unwrap().to_string()],
                false,
                &cancel,
            );
            let elapsed = start.elapsed();

            // Restore permissions so TempDir cleanup can remove the files.
            // Per-entry errors are ignored so a single failure does not
            // panic the loop and leave subsequent files unreadable, which
            // would break TempDir cleanup.
            if let Ok(entries) = std::fs::read_dir(temp_dir.path()) {
                for entry in entries.flatten() {
                    if let Ok(meta) = std::fs::metadata(entry.path()) {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o644);
                        let _ = std::fs::set_permissions(entry.path(), perms);
                    }
                }
            }

            assert!(
                elapsed < std::time::Duration::from_secs(30),
                "search hung — likely a stderr-pipe deadlock (took {:?})",
                elapsed
            );
            // The unreadable files cause `rg` to exit with a non-zero
            // status; we must surface that as an Err with the captured
            // stderr text included (regression check for the previous
            // opaque "exit status N" message).
            match result {
                Err(msg) => {
                    assert!(
                        msg.contains("exit status"),
                        "error must mention exit status, got: {}",
                        msg
                    );
                    assert!(
                        msg.to_lowercase().contains("permission") || msg.contains("os error 13"),
                        "error must include captured stderr text, got: {}",
                        msg
                    );
                }
                Ok(_) => {
                    // Some rg builds may treat read errors as exit code 0
                    // with warnings on stderr only. In that case the
                    // test still proves no deadlock occurred, which is
                    // the primary regression we are guarding against.
                }
            }
        }
        #[cfg(not(unix))]
        {
            // chmod 0o000 is meaningless on non-unix platforms; the
            // deadlock guarantee is enforced by the same code path
            // regardless. Skip rather than introduce a Windows-specific
            // stderr-flooding fixture.
            eprintln!("skipping test_search_does_not_deadlock_on_chatty_stderr: non-unix platform");
        }
    }
}
