use crate::search::{extract_project_from_path, group_by_session, search_multiple_paths};
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

/// Run CLI search command
pub fn cli_search(query: &str, search_paths: &[String], use_regex: bool, limit: usize) {
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

    let groups = group_by_session(search_result.matches);
    let mut count = 0;

    for group in &groups {
        let project = extract_project_from_path(&group.file_path);
        let provider = SessionProvider::from_path(&group.file_path);
        let source = SessionSource::from_path(&group.file_path);

        for m in &group.matches {
            if count >= limit {
                return;
            }

            if let Some(ref msg) = m.message {
                let result = CliSearchResult {
                    session_id: msg.session_id.clone(),
                    project: project.clone(),
                    provider: provider.display_name().to_string(),
                    source: source.display_name().to_string(),
                    file_path: m.file_path.clone(),
                    timestamp: msg.timestamp.to_rfc3339(),
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                };

                if let Ok(json) = serde_json::to_string(&result) {
                    println!("{}", json);
                    count += 1;
                }
            }
        }
    }
}

/// Run CLI list command — enumerate all sessions with metadata
pub fn cli_list(search_paths: &[String], limit: usize) {
    let mut sessions: Vec<ListResult> = collect_session_jsonl_files(search_paths)
        .into_iter()
        .filter_map(|path| extract_session_metadata(&path))
        .collect();

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
