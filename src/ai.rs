use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::recent::{extract_non_meta_user_text, is_real_user_prompt, truncate_summary};

/// Context extracted from a single session for AI ranking.
pub struct SessionContext {
    pub session_id: String,
    pub project: String,
    pub summary: String,
    pub user_messages: Vec<String>,
}

/// Result of an AI ranking request.
pub struct AiRankResult {
    pub ranked_ids: Vec<String>,
    pub error: Option<String>,
}

pub(crate) const AI_NO_RELEVANT_SESSIONS_MSG: &str =
    "no relevant sessions found; press Enter to retry or refine query";

/// Extract up to 3 real user messages from first 50 lines of a session JSONL file.
pub fn collect_session_context(
    file_path: &str,
    session_id: &str,
    project: &str,
    summary: &str,
) -> SessionContext {
    let mut user_messages = Vec::new();

    if let Ok(file) = std::fs::File::open(file_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().take(50).flatten() {
            if user_messages.len() >= 3 {
                break;
            }

            let json: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(text) = extract_non_meta_user_text(&json) {
                if is_real_user_prompt(&text) {
                    user_messages.push(truncate_summary(&text, 200));
                }
            }
        }
    }

    SessionContext {
        session_id: session_id.to_string(),
        project: project.to_string(),
        summary: summary.to_string(),
        user_messages,
    }
}

/// Build a prompt for Claude to rank sessions by relevance to a query.
pub fn build_prompt(query: &str, sessions: &[SessionContext]) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are a session relevance ranker. Given a user query and a list of Claude sessions, ",
    );
    prompt.push_str("return a JSON array of session IDs ranked by relevance to the query (most relevant first). ");
    prompt.push_str("Only include sessions that are at least somewhat relevant. ");
    prompt.push_str("If no session is relevant, return []. ");
    prompt.push_str("Do NOT wrap in code fences, do NOT add prose. ");
    prompt.push_str("Return ONLY a JSON array of strings, no other text.\n\n");
    let _ = writeln!(prompt, "Query: {}\n\nSessions:", query);

    for (i, s) in sessions.iter().enumerate() {
        let _ = writeln!(prompt, "{}. ID: {}", i + 1, s.session_id);
        if !s.project.is_empty() {
            let _ = writeln!(prompt, "   Project: {}", s.project);
        }
        if !s.summary.is_empty() {
            let _ = writeln!(prompt, "   Summary: {}", s.summary);
        }
        for (j, msg) in s.user_messages.iter().enumerate() {
            let _ = writeln!(prompt, "   Message {}: {}", j + 1, msg);
        }
        prompt.push('\n');
    }

    prompt.push_str("Return JSON array of session IDs ranked by relevance:");
    prompt
}

#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
enum ParseError {
    NoJsonArray,
    UnterminatedArray,
    NotStringArray,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ParseError::NoJsonArray => "no JSON array",
            ParseError::UnterminatedArray => "unterminated array",
            ParseError::NotStringArray => "not a string array",
        };
        f.write_str(s)
    }
}

const EMPTY_STDOUT_MARKER: &str = "<empty stdout>";
const SAMPLE_LIMIT: usize = 100;

/// Parse AI response to extract a JSON array of session IDs.
/// Finds the first `[` and its matching `]`, then tries to parse as Vec<String>.
fn parse_ai_response(output: &str) -> Result<Vec<String>, ParseError> {
    let start = output.find('[').ok_or(ParseError::NoJsonArray)?;

    let mut depth = 0;
    let mut end = None;
    for (i, ch) in output[start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    let end = end.ok_or(ParseError::UnterminatedArray)?;

    serde_json::from_str::<Vec<String>>(&output[start..end]).map_err(|_| ParseError::NotStringArray)
}

/// Format a parse error with a normalized, truncated sample of the raw stdout so
/// users can diagnose why Claude's response couldn't be parsed straight from the
/// status bar. Uses char-based (not byte-based) truncation to stay UTF-8 safe.
fn format_parse_error(err: ParseError, stdout: &str) -> String {
    let normalized: String = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    let sample = if normalized.is_empty() {
        EMPTY_STDOUT_MARKER.to_string()
    } else if normalized.chars().count() > SAMPLE_LIMIT {
        let mut truncated: String = normalized.chars().take(SAMPLE_LIMIT).collect();
        truncated.push('…');
        truncated
    } else {
        normalized
    };

    format!("{}: {}", err, sample)
}

/// Lightweight session descriptor for passing to the background thread.
/// Contains only the data needed to locate and identify sessions — no file content.
pub struct SessionInfo {
    pub file_path: String,
    pub session_id: String,
    pub project: String,
    pub summary: String,
}

/// Spawn a background thread that collects session context (file I/O), builds the prompt,
/// calls `claude -p`, and returns the parsed ranking result via a one-shot channel.
pub fn spawn_ai_rank(
    query: String,
    sessions: Vec<SessionInfo>,
) -> Result<Receiver<AiRankResult>, String> {
    let claude_path =
        which::which("claude").map_err(|_| "Claude binary not found in PATH".to_string())?;

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        // Collect context from JSONL files (file I/O happens here, off the main thread)
        let contexts: Vec<SessionContext> = sessions
            .iter()
            .map(|s| collect_session_context(&s.file_path, &s.session_id, &s.project, &s.summary))
            .collect();

        let prompt = build_prompt(&query, &contexts);

        let result = match Command::new(&claude_path)
            .args(["-p", &prompt])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(output) => {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    match parse_ai_response(&stdout) {
                        Ok(ranked_ids) if ranked_ids.is_empty() => AiRankResult {
                            ranked_ids: Vec::new(),
                            error: Some(AI_NO_RELEVANT_SESSIONS_MSG.to_string()),
                        },
                        Ok(ranked_ids) => AiRankResult {
                            ranked_ids,
                            error: None,
                        },
                        Err(kind) => AiRankResult {
                            ranked_ids: Vec::new(),
                            error: Some(format_parse_error(kind, &stdout)),
                        },
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    AiRankResult {
                        ranked_ids: Vec::new(),
                        error: Some(format!("claude exited with error: {}", stderr.trim())),
                    }
                }
            }
            Err(e) => AiRankResult {
                ranked_ids: Vec::new(),
                error: Some(format!("Failed to run claude: {}", e)),
            },
        };

        let _ = tx.send(result);
    });

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_user_line(text: &str, is_meta: bool) -> String {
        if is_meta {
            format!(
                r#"{{"type":"user","isMeta":true,"message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#,
                text
            )
        } else {
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#,
                text
            )
        }
    }

    fn make_assistant_line(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#,
            text
        )
    }

    #[test]
    fn test_collect_session_context_extracts_messages() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{}", make_user_line("Hello world", false)).unwrap();
        writeln!(f, "{}", make_assistant_line("Hi there")).unwrap();
        writeln!(f, "{}", make_user_line("Second question", false)).unwrap();
        writeln!(f, "{}", make_user_line("Third question", false)).unwrap();

        let ctx = collect_session_context(
            f.path().to_str().unwrap(),
            "s1",
            "my-project",
            "A test session",
        );

        assert_eq!(ctx.session_id, "s1");
        assert_eq!(ctx.project, "my-project");
        assert_eq!(ctx.summary, "A test session");
        assert_eq!(ctx.user_messages.len(), 3);
        assert_eq!(ctx.user_messages[0], "Hello world");
        assert_eq!(ctx.user_messages[1], "Second question");
        assert_eq!(ctx.user_messages[2], "Third question");
    }

    #[test]
    fn test_collect_session_context_skips_meta_and_system_reminder() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{}", make_user_line("init message", true)).unwrap();
        writeln!(
            f,
            "{}",
            make_user_line("<system-reminder>hook output</system-reminder>", false)
        )
        .unwrap();
        writeln!(f, "{}", make_user_line("Real question here", false)).unwrap();

        let ctx = collect_session_context(f.path().to_str().unwrap(), "s1", "proj", "summary");

        assert_eq!(ctx.user_messages.len(), 1);
        assert_eq!(ctx.user_messages[0], "Real question here");
    }

    #[test]
    fn test_collect_session_context_truncates_long_messages() {
        let long_msg = "a".repeat(300);
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{}", make_user_line(&long_msg, false)).unwrap();

        let ctx = collect_session_context(f.path().to_str().unwrap(), "s1", "", "");

        assert_eq!(ctx.user_messages.len(), 1);
        assert!(ctx.user_messages[0].len() <= 200 + 3); // 200 chars + "..."
        assert!(ctx.user_messages[0].ends_with("..."));
    }

    #[test]
    fn test_collect_session_context_max_three_messages() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..10 {
            writeln!(f, "{}", make_user_line(&format!("Message {}", i), false)).unwrap();
        }

        let ctx = collect_session_context(f.path().to_str().unwrap(), "s1", "", "");

        assert_eq!(ctx.user_messages.len(), 3);
    }

    #[test]
    fn test_build_prompt_contains_query_and_ids() {
        let sessions = vec![
            SessionContext {
                session_id: "id-001".to_string(),
                project: "proj-a".to_string(),
                summary: "Working on feature X".to_string(),
                user_messages: vec!["How to sort?".to_string()],
            },
            SessionContext {
                session_id: "id-002".to_string(),
                project: "proj-b".to_string(),
                summary: "Bug fix session".to_string(),
                user_messages: vec![],
            },
        ];

        let prompt = build_prompt("sorting algorithm", &sessions);

        assert!(prompt.contains("sorting algorithm"));
        assert!(prompt.contains("id-001"));
        assert!(prompt.contains("id-002"));
        assert!(prompt.contains("proj-a"));
        assert!(prompt.contains("Working on feature X"));
        assert!(prompt.contains("How to sort?"));
        assert!(prompt.contains("Bug fix session"));
        assert!(prompt.contains("JSON array"));
    }

    #[test]
    fn test_parse_ai_response_clean_json() {
        let output = r#"["id-001", "id-003", "id-002"]"#;
        let result = parse_ai_response(output).unwrap();
        assert_eq!(result, vec!["id-001", "id-003", "id-002"]);
    }

    #[test]
    fn test_parse_ai_response_wrapped_in_prose() {
        let output = r#"Based on the query, here are the sessions ranked by relevance:

["sess-abc", "sess-def"]

These sessions are most relevant because..."#;
        let result = parse_ai_response(output).unwrap();
        assert_eq!(result, vec!["sess-abc", "sess-def"]);
    }

    #[test]
    fn test_parse_ai_response_invalid() {
        assert_eq!(
            parse_ai_response("no json here"),
            Err(ParseError::NoJsonArray)
        );
        assert_eq!(parse_ai_response(""), Err(ParseError::NoJsonArray));
        assert_eq!(
            parse_ai_response("[unclosed"),
            Err(ParseError::UnterminatedArray)
        );
        assert_eq!(
            parse_ai_response("[1, 2, 3]"),
            Err(ParseError::NotStringArray)
        );
    }

    #[test]
    fn test_parse_ai_response_empty_array() {
        assert_eq!(parse_ai_response("[]"), Ok(Vec::<String>::new()));
        assert_eq!(
            parse_ai_response("prose before []\nthen prose after"),
            Ok(Vec::<String>::new())
        );
    }

    #[test]
    fn test_parse_ai_response_slice_wrapped() {
        // Observed behaviour: `claude -p` sometimes wraps the array in JS-like
        // `.slice(0,0)` prose. Parser should catch the first valid array.
        let output = "[\"x\",\"y\"].slice(0,0)\n\n[]";
        assert_eq!(parse_ai_response(output), Ok(vec!["x".into(), "y".into()]));
    }

    #[test]
    fn test_format_parse_error_truncates() {
        // Longest category "not a string array" (18) + ": " + 100 + "…" = 121, leave headroom.
        fn assert_truncated(err: ParseError, stdout: &str, prefix: &str) {
            let msg = format_parse_error(err, stdout);
            assert!(msg.starts_with(prefix), "missing prefix in {msg:?}");
            assert!(msg.ends_with('…'), "missing ellipsis in {msg:?}");
            assert!(
                msg.chars().count() <= 125,
                "msg too long ({} chars): {msg:?}",
                msg.chars().count()
            );
        }

        let ascii = format!("preamble\n\n{}", "a".repeat(300));
        assert_truncated(ParseError::NoJsonArray, &ascii, "no JSON array: ");

        // Multi-byte UTF-8 on the char-100 boundary must not panic on byte-slicing.
        let utf8 = "я".repeat(200);
        assert_truncated(ParseError::NotStringArray, &utf8, "not a string array: ");
    }

    #[test]
    fn test_format_parse_error_empty_stdout() {
        let msg = format_parse_error(ParseError::NoJsonArray, "");
        assert_eq!(msg, "no JSON array: <empty stdout>");

        // Whitespace-only stdout also collapses to empty.
        let msg = format_parse_error(ParseError::UnterminatedArray, "  \n\t  ");
        assert_eq!(msg, "unterminated array: <empty stdout>");
    }
}
