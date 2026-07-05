use crate::search::{
    extract_context, extract_context_around_span, extract_project_from_path, group_by_session,
    search_multiple_paths, Message, SessionGroup,
};
use crate::session::{collect_session_jsonl_files, SessionProvider, SessionSource};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Serialize)]
struct CliSearchResult {
    #[serde(rename = "type")]
    record_type: &'static str,
    session_id: String,
    project: String,
    provider: String,
    source: String,
    file_path: String,
    /// 1-based line in the session JSONL; None for Opencode rows, whose
    /// messages live in SQLite and have no line position.
    line_number: Option<usize>,
    message_uuid: Option<String>,
    timestamp: String,
    role: String,
    content: String,
}

/// Trailing record emitted after search results so machine consumers can
/// tell whether the result set is complete without parsing stderr.
#[derive(Serialize)]
struct CliSearchSummary {
    #[serde(rename = "type")]
    record_type: &'static str,
    shown: usize,
    total_matches: usize,
    sessions: usize,
    truncated: bool,
}

#[derive(Serialize)]
struct ListResult {
    session_id: String,
    project: String,
    provider: String,
    source: String,
    file_path: String,
    last_active: String,
    message_count: usize,
}

/// Characters of context kept on each side of the match when rendering snippets
const SNIPPET_CONTEXT_CHARS: usize = 200;

/// Run CLI search command
pub fn cli_search(
    query: &str,
    search_paths: &[String],
    use_regex: bool,
    limit: usize,
    full_content: bool,
) {
    // CLI search is one-shot and runs to completion; no cancellation is needed,
    // but the lower-level API requires a token, so we pass a permanently-false one.
    let cancel = Arc::new(AtomicBool::new(false));
    let search_result = match search_multiple_paths(query, search_paths, use_regex, &cancel) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Search error: {}", e);
            std::process::exit(1);
        }
    };

    if search_result.truncated {
        eprintln!("Warning: results may be incomplete (per-file match limit reached)");
    }

    let snippet_regex = build_snippet_regex(query, use_regex, full_content);

    let groups = group_by_session(search_result.matches);

    let total_matches: usize = groups
        .iter()
        .map(|g| g.matches.iter().filter(|m| m.message.is_some()).count())
        .sum();

    let (shown, sessions_shown) =
        print_group_matches(&groups, query, full_content, limit, snippet_regex.as_ref());

    // The summary is always the last line, even with zero matches, so machine
    // consumers can tell "nothing found" from "search did not run".
    let summary = CliSearchSummary {
        record_type: "summary",
        shown,
        total_matches,
        sessions: sessions_shown.len(),
        truncated: search_result.truncated || total_matches > shown,
    };
    print_json_line(&summary);
}

/// In regex mode the query pattern rarely appears literally in the content,
/// so the snippet is anchored on the first actual regex match instead.
fn build_snippet_regex(query: &str, use_regex: bool, full_content: bool) -> Option<regex::Regex> {
    if use_regex && !full_content {
        regex::RegexBuilder::new(query)
            .case_insensitive(true)
            .build()
            .ok()
    } else {
        None
    }
}

/// Render the content field of a search result: the full message, or a
/// snippet anchored on the regex match (regex mode) / the literal query.
fn render_match_content(
    content: &str,
    query: &str,
    full_content: bool,
    snippet_regex: Option<&regex::Regex>,
) -> String {
    if full_content {
        content.to_string()
    } else if let Some(found) = snippet_regex.and_then(|re| re.find(content)) {
        extract_context_around_span(content, found.start(), found.end(), SNIPPET_CONTEXT_CHARS)
    } else {
        extract_context(content, query, SNIPPET_CONTEXT_CHARS)
    }
}

/// Print one JSON line per match, stopping at `limit`. Returns the number of
/// rows printed and the set of session ids they belong to.
fn print_group_matches(
    groups: &[SessionGroup],
    query: &str,
    full_content: bool,
    limit: usize,
    snippet_regex: Option<&regex::Regex>,
) -> (usize, std::collections::HashSet<String>) {
    let mut shown = 0;
    let mut sessions_shown = std::collections::HashSet::new();

    'groups: for group in groups {
        let project = extract_project_from_path(&group.file_path);
        let provider = SessionProvider::from_path(&group.file_path);
        let source = SessionSource::from_path(&group.file_path);
        let is_opencode = crate::session::opencode::parse_session_path(&group.file_path).is_some();

        for m in &group.matches {
            if shown >= limit {
                break 'groups;
            }

            if let Some(ref msg) = m.message {
                let result = CliSearchResult {
                    record_type: "match",
                    session_id: msg.session_id.clone(),
                    project: project.clone(),
                    provider: provider.display_name().to_string(),
                    source: source.display_name().to_string(),
                    file_path: msg.file_path.as_deref().unwrap_or(&m.file_path).to_string(),
                    line_number: if is_opencode {
                        None
                    } else {
                        Some(msg.line_number)
                    },
                    message_uuid: msg.uuid.clone(),
                    timestamp: msg.timestamp.to_rfc3339(),
                    role: msg.role.clone(),
                    content: render_match_content(&msg.content, query, full_content, snippet_regex),
                };

                if print_json_line(&result) {
                    shown += 1;
                    sessions_shown.insert(msg.session_id.clone());
                }
            }
        }
    }

    (shown, sessions_shown)
}

/// Serialize `value` and print it as a single line. Returns whether the line
/// was printed (serialization can fail, in which case nothing is emitted).
fn print_json_line<T: Serialize>(value: &T) -> bool {
    match serde_json::to_string(value) {
        Ok(json) => {
            println!("{}", json);
            true
        }
        Err(_) => false,
    }
}

/// Per-message record emitted by `ccs show`.
#[derive(Serialize)]
struct ShowMessageRow {
    #[serde(rename = "type")]
    record_type: &'static str,
    line_number: Option<usize>,
    message_uuid: Option<String>,
    parent_uuid: Option<String>,
    role: String,
    timestamp: String,
    content: String,
    #[serde(skip_serializing_if = "is_false")]
    is_target: bool,
    #[serde(skip_serializing_if = "is_false")]
    content_truncated: bool,
}

/// Trailing record emitted by `ccs show` with session-level metadata.
#[derive(Serialize)]
struct ShowSummary {
    #[serde(rename = "type")]
    record_type: &'static str,
    session_id: String,
    file_path: String,
    provider: String,
    source: String,
    project: String,
    shown: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_uuid: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Run CLI show command — print a window of messages around a search hit.
///
/// `file_path`, `--line` and `--uuid` come straight from `ccs search` output
/// (`file_path`, `line_number`, `message_uuid`). The window is `context`
/// messages before and after the target, in file order.
pub fn cli_show(
    file_path: &str,
    line: Option<usize>,
    uuid: Option<&str>,
    context: usize,
    max_chars: usize,
) {
    let result =
        if let Some((db, session_id)) = crate::session::opencode::parse_session_path(file_path) {
            show_opencode(&db, &session_id, file_path, uuid, context, max_chars)
        } else {
            show_jsonl(file_path, line, uuid, context, max_chars)
        };
    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn show_jsonl(
    file_path: &str,
    line: Option<usize>,
    uuid: Option<&str>,
    context: usize,
    max_chars: usize,
) -> Result<(), String> {
    if line.is_none() && uuid.is_none() {
        return Err(
            "pass --line or --uuid to anchor the window (both come from search output)".to_string(),
        );
    }

    let file =
        fs::File::open(file_path).map_err(|e| format!("cannot open {}: {}", file_path, e))?;
    let reader = BufReader::new(file);

    let mut messages: Vec<Message> = Vec::new();
    // If --line points at a record that isn't a message, remember what it is
    // so the error names the record type instead of a bare "not found".
    let mut anchor_line_kind: Option<String> = None;
    let mut total_lines = 0usize;

    for (idx, raw) in reader.lines().enumerate() {
        let line_no = idx + 1;
        total_lines = line_no;
        let raw = raw.map_err(|e| format!("cannot read {}: {}", file_path, e))?;
        if let Some(msg) = Message::from_jsonl_with_path(&raw, line_no, Some(file_path)) {
            messages.push(msg);
        } else if line == Some(line_no) {
            anchor_line_kind = Some(classify_non_message_line(&raw));
        }
    }

    let target_idx = if let Some(n) = line {
        messages
            .iter()
            .position(|m| m.line_number == n)
            .ok_or_else(|| match &anchor_line_kind {
                Some(kind) => format!("line {} is not a message record ({})", n, kind),
                None => format!("line {} is beyond end of file ({} lines)", n, total_lines),
            })?
    } else {
        let u = uuid.expect("checked above");
        messages
            .iter()
            .position(|m| m.uuid.as_deref() == Some(u))
            .ok_or_else(|| format!("no message with uuid {} in {}", u, file_path))?
    };

    let start = target_idx.saturating_sub(context);
    let end = target_idx
        .saturating_add(context)
        .saturating_add(1)
        .min(messages.len());

    for (i, msg) in messages[start..end].iter().enumerate() {
        let (content, content_truncated) = truncate_chars(&msg.content, max_chars);
        let row = ShowMessageRow {
            record_type: "message",
            line_number: Some(msg.line_number),
            message_uuid: msg.uuid.clone(),
            parent_uuid: msg.parent_uuid.clone(),
            role: msg.role.clone(),
            timestamp: msg.timestamp.to_rfc3339(),
            content,
            is_target: start + i == target_idx,
            content_truncated,
        };
        if let Ok(json) = serde_json::to_string(&row) {
            println!("{}", json);
        }
    }

    let target = &messages[target_idx];
    let summary = ShowSummary {
        record_type: "summary",
        session_id: target.session_id.clone(),
        file_path: file_path.to_string(),
        provider: SessionProvider::from_path(file_path)
            .display_name()
            .to_string(),
        source: SessionSource::from_path(file_path)
            .display_name()
            .to_string(),
        project: extract_project_from_path(file_path),
        shown: end - start,
        target_line: Some(target.line_number),
        target_uuid: None,
    };
    if let Ok(json) = serde_json::to_string(&summary) {
        println!("{}", json);
    }
    Ok(())
}

/// Show a window of messages around a target in an Opencode SQLite session.
/// Opencode messages have no line numbers, so the anchor must be `--uuid`
/// (the `message_uuid` from search output, which is the SQLite message id).
fn show_opencode(
    db_path: &Path,
    session_id: &str,
    file_path: &str,
    uuid: Option<&str>,
    context: usize,
    max_chars: usize,
) -> Result<(), String> {
    use crate::session::record::{ContentMode, MessageRole, SessionRecord};

    let uuid = uuid.ok_or_else(|| {
        "Opencode sessions have no line numbers; pass --uuid from the search result's message_uuid"
            .to_string()
    })?;

    let messages = crate::session::opencode::load_messages(db_path, session_id);
    if messages.is_empty() {
        return Err(format!(
            "no messages found for session {} in {}",
            session_id,
            db_path.display()
        ));
    }

    let target_idx = messages
        .iter()
        .position(|m| m.id == uuid)
        .ok_or_else(|| format!("no message with uuid {} in session {}", uuid, session_id))?;

    let start = target_idx.saturating_sub(context);
    let end = target_idx
        .saturating_add(context)
        .saturating_add(1)
        .min(messages.len());

    for (i, msg) in messages[start..end].iter().enumerate() {
        let rendered = SessionRecord::render_content(&msg.content_blocks, &ContentMode::Full);
        let (content, content_truncated) = truncate_chars(&rendered, max_chars);
        let role = match msg.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        };
        let row = ShowMessageRow {
            record_type: "message",
            line_number: None,
            message_uuid: Some(msg.id.clone()),
            parent_uuid: msg.parent_id.clone(),
            role: role.to_string(),
            timestamp: msg.created_at.to_rfc3339(),
            content,
            is_target: start + i == target_idx,
            content_truncated,
        };
        if let Ok(json) = serde_json::to_string(&row) {
            println!("{}", json);
        }
    }

    let summary = ShowSummary {
        record_type: "summary",
        session_id: session_id.to_string(),
        file_path: file_path.to_string(),
        provider: SessionProvider::from_path(file_path)
            .display_name()
            .to_string(),
        source: SessionSource::from_path(file_path)
            .display_name()
            .to_string(),
        project: extract_project_from_path(file_path),
        shown: end - start,
        target_line: None,
        target_uuid: Some(uuid.to_string()),
    };
    if let Ok(json) = serde_json::to_string(&summary) {
        println!("{}", json);
    }
    Ok(())
}

/// Name the record type on a line that didn't parse as a message, for error text.
fn classify_non_message_line(raw: &str) -> String {
    use crate::session::record::SessionRecord;
    let Ok(json) = serde_json::from_str::<serde_json::Value>(raw) else {
        return "invalid JSON".to_string();
    };
    match SessionRecord::from_value(&json) {
        Some(record) => classify_record_kind(&record).to_string(),
        None => "unparseable record".to_string(),
    }
}

/// Human-readable name for a parsed record kind, for error text.
fn classify_record_kind(record: &crate::session::record::SessionRecord) -> &'static str {
    use crate::session::record::SessionRecord;
    match record {
        SessionRecord::Message { .. } => "message with empty content",
        SessionRecord::Summary { .. } => "type=summary",
        SessionRecord::CustomTitle(_) => "type=custom_title",
        SessionRecord::AiTitle(_) => "type=ai_title",
        SessionRecord::AgentName(_) => "type=agent_name",
        SessionRecord::LastPrompt(_) => "type=last_prompt",
        SessionRecord::CompactBoundary { .. } => "type=compact_boundary",
        SessionRecord::Metadata { .. } => "metadata record",
        SessionRecord::Other { .. } => "unrecognized record",
    }
}

/// Truncate to at most `max` chars on a char boundary.
/// Returns the (possibly shortened) string and whether truncation happened.
fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => (s[..byte_idx].to_string(), true),
        None => (s.to_string(), false),
    }
}

/// Run CLI list command — enumerate all sessions with metadata
pub fn cli_list(search_paths: &[String], limit: usize) {
    let mut sessions: Vec<ListResult> = collect_session_jsonl_files(search_paths)
        .into_iter()
        .filter_map(|path| extract_session_metadata(&path))
        .collect();

    // Opencode storage isn't JSONL-shaped — pull those sessions from their
    // own layout so `ccs list` is provider-complete. Only walk when an
    // Opencode database is reachable via the caller's search paths, matching
    // `collect_recent_sessions` so tests with synthetic temp roots don't
    // pick up the user's real DB.
    if search_paths
        .iter()
        .any(|p| crate::session::opencode::is_opencode_session_path(p))
    {
        sessions.extend(collect_opencode_list_entries(search_paths, limit));
    }

    // Sort by last_active descending
    sessions.sort_by(|a, b| b.last_active.cmp(&a.last_active));

    // Deduplicate by session_id (keep newest)
    let mut seen = std::collections::HashSet::new();
    sessions.retain(|s| seen.insert(s.session_id.clone()));

    for session in sessions.iter().take(limit) {
        if let Ok(json) = serde_json::to_string(session) {
            println!("{}", json);
        }
    }
}

fn collect_opencode_list_entries(search_paths: &[String], limit: usize) -> Vec<ListResult> {
    use crate::recent::opencode_databases_for_search_paths;
    use crate::session::opencode::list_sessions_for_recent;
    let dbs = opencode_databases_for_search_paths(search_paths);
    if dbs.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for db in &dbs {
        for summary in list_sessions_for_recent(db, limit) {
            let project = summary
                .project_label
                .clone()
                .unwrap_or_else(|| summary.project_id.clone());
            out.push(ListResult {
                session_id: summary.id,
                project,
                provider: "Opencode".to_string(),
                source: SessionSource::CLI.display_name().to_string(),
                file_path: summary.session_file.to_string_lossy().to_string(),
                last_active: summary.updated_at.to_rfc3339(),
                message_count: summary.message_count,
            });
        }
    }
    out
}

/// Extract metadata from a single .jsonl file by reading first and last messages
fn extract_session_metadata(path: &Path) -> Option<ListResult> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let path_str = path.to_str()?;
    let project = extract_project_from_path(path_str);
    let provider = SessionProvider::from_path(path_str);
    let source = SessionSource::from_path(path_str);

    let mut session_id: Option<String> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session_id.is_none() {
            session_id = crate::session::extract_session_id(&json)
                .or_else(|| crate::session::extract_codex_session_id_from_path(path_str));
        }
        if let Some(ts) = crate::session::extract_timestamp(&json) {
            if last_timestamp.is_none_or(|t| ts > t) {
                last_timestamp = Some(ts);
            }
        }
    }

    let message_count = crate::search::count_session_messages(path_str).0;

    let session_id = session_id?;
    let last_active = last_timestamp?.to_rfc3339();

    Some(ListResult {
        session_id,
        project,
        provider: provider.display_name().to_string(),
        source: source.display_name().to_string(),
        file_path: path_str.to_string(),
        last_active,
        message_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::record::{MessageRole, SessionRecord};

    #[test]
    fn classify_record_kind_names_every_variant() {
        let cases: Vec<(SessionRecord, &str)> = vec![
            (
                SessionRecord::Message {
                    role: MessageRole::User,
                    content_blocks: Vec::new(),
                    uuid: None,
                    parent_uuid: None,
                    is_sidechain: false,
                },
                "message with empty content",
            ),
            (
                SessionRecord::Summary {
                    text: "s".to_string(),
                    is_compaction: false,
                    uuid: None,
                    parent_uuid: None,
                    leaf_uuid: None,
                    is_sidechain: false,
                },
                "type=summary",
            ),
            (
                SessionRecord::CustomTitle("t".to_string()),
                "type=custom_title",
            ),
            (SessionRecord::AiTitle("t".to_string()), "type=ai_title"),
            (SessionRecord::AgentName("t".to_string()), "type=agent_name"),
            (
                SessionRecord::LastPrompt("t".to_string()),
                "type=last_prompt",
            ),
            (
                SessionRecord::CompactBoundary {
                    uuid: None,
                    parent_uuid: None,
                    logical_parent_uuid: None,
                    is_sidechain: false,
                },
                "type=compact_boundary",
            ),
            (
                SessionRecord::Metadata {
                    uuid: None,
                    parent_uuid: None,
                    is_sidechain: false,
                },
                "metadata record",
            ),
            (
                SessionRecord::Other {
                    uuid: None,
                    parent_uuid: None,
                    is_sidechain: false,
                },
                "unrecognized record",
            ),
        ];
        for (record, expected) in &cases {
            assert_eq!(classify_record_kind(record), *expected);
        }
    }

    #[test]
    fn classify_non_message_line_rejects_invalid_json() {
        assert_eq!(classify_non_message_line("{not json"), "invalid JSON");
    }

    #[test]
    fn classify_non_message_line_without_type_is_unparseable() {
        assert_eq!(
            classify_non_message_line(r#"{"foo": 1}"#),
            "unparseable record"
        );
    }

    #[test]
    fn classify_non_message_line_names_parsed_record() {
        assert_eq!(
            classify_non_message_line(r#"{"type":"summary","summary":"Discussed"}"#),
            "type=summary"
        );
    }

    #[test]
    fn snippet_regex_built_only_in_regex_snippet_mode() {
        let re = build_snippet_regex("Foo.*Bar", true, false).unwrap();
        assert!(
            re.is_match("prefix foo middle bar suffix"),
            "case-insensitive"
        );
        assert!(build_snippet_regex("foo", false, false).is_none());
        assert!(build_snippet_regex("foo", true, true).is_none());
        // Invalid pattern degrades to None instead of failing
        assert!(build_snippet_regex("foo(", true, false).is_none());
    }

    #[test]
    fn render_match_content_full_returns_whole_message() {
        let content = "x".repeat(600);
        assert_eq!(render_match_content(&content, "x", true, None), content);
    }

    #[test]
    fn render_match_content_anchors_on_regex_match() {
        let re = regex::Regex::new("needle").unwrap();
        let content = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let rendered = render_match_content(&content, "needle", false, Some(&re));
        assert!(rendered.contains("needle"));
        assert!(rendered.len() < content.len());
    }

    #[test]
    fn render_match_content_falls_back_to_literal_query() {
        let content = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let rendered = render_match_content(&content, "needle", false, None);
        assert!(rendered.contains("needle"));
        assert!(rendered.len() < content.len());
    }

    #[test]
    fn truncate_chars_respects_char_boundaries() {
        let (s, truncated) = truncate_chars("héllo", 2);
        assert_eq!(s, "hé");
        assert!(truncated);
        let (s, truncated) = truncate_chars("hi", 10);
        assert_eq!(s, "hi");
        assert!(!truncated);
    }
}
