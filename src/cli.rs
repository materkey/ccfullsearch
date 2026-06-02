use crate::search::{
    extract_context, extract_context_around_span, extract_project_from_path, group_by_session,
    search_multiple_paths,
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
    session_id: String,
    project: String,
    provider: String,
    source: String,
    file_path: String,
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

    // In regex mode the query pattern rarely appears literally in the content,
    // so the snippet is anchored on the first actual regex match instead.
    let snippet_regex = if use_regex && !full_content {
        regex::RegexBuilder::new(query)
            .case_insensitive(true)
            .build()
            .ok()
    } else {
        None
    };

    let groups = group_by_session(search_result.matches);

    let total_matches: usize = groups
        .iter()
        .map(|g| g.matches.iter().filter(|m| m.message.is_some()).count())
        .sum();

    let mut shown = 0;
    let mut sessions_shown = std::collections::HashSet::new();

    'groups: for group in &groups {
        let project = extract_project_from_path(&group.file_path);
        let provider = SessionProvider::from_path(&group.file_path);
        let source = SessionSource::from_path(&group.file_path);

        for m in &group.matches {
            if shown >= limit {
                break 'groups;
            }

            if let Some(ref msg) = m.message {
                let content = if full_content {
                    msg.content.clone()
                } else if let Some(found) =
                    snippet_regex.as_ref().and_then(|re| re.find(&msg.content))
                {
                    extract_context_around_span(
                        &msg.content,
                        found.start(),
                        found.end(),
                        SNIPPET_CONTEXT_CHARS,
                    )
                } else {
                    extract_context(&msg.content, query, SNIPPET_CONTEXT_CHARS)
                };
                let result = CliSearchResult {
                    session_id: msg.session_id.clone(),
                    project: project.clone(),
                    provider: provider.display_name().to_string(),
                    source: source.display_name().to_string(),
                    file_path: m.file_path.clone(),
                    timestamp: msg.timestamp.to_rfc3339(),
                    role: msg.role.clone(),
                    content,
                };

                if let Ok(json) = serde_json::to_string(&result) {
                    println!("{}", json);
                    shown += 1;
                    sessions_shown.insert(msg.session_id.clone());
                }
            }
        }
    }

    // Keep the "no matches -> empty stdout" contract: the summary record is
    // emitted only when at least one result was shown.
    if shown > 0 {
        let summary = CliSearchSummary {
            record_type: "summary",
            shown,
            total_matches,
            sessions: sessions_shown.len(),
            truncated: search_result.truncated || total_matches > shown,
        };
        if let Ok(json) = serde_json::to_string(&summary) {
            println!("{}", json);
        }
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
