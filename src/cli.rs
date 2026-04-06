use crate::search::{
    extract_project_from_path, group_by_session, search_multiple_paths, Message, SessionSource,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Serialize)]
struct SearchResult {
    session_id: String,
    project: String,
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
    source: String,
    file_path: String,
    last_active: String,
    message_count: usize,
}

/// Run CLI search command
pub fn cli_search(query: &str, search_paths: &[String], use_regex: bool, limit: usize) {
    let results = match search_multiple_paths(query, search_paths, use_regex) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Search error: {}", e);
            std::process::exit(1);
        }
    };

    let groups = group_by_session(results);
    let mut count = 0;

    for group in &groups {
        let project = extract_project_from_path(&group.file_path);
        let source = SessionSource::from_path(&group.file_path);

        for m in &group.matches {
            if count >= limit {
                return;
            }

            if let Some(ref msg) = m.message {
                let result = SearchResult {
                    session_id: msg.session_id.clone(),
                    project: project.clone(),
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
    let mut sessions: Vec<ListResult> = Vec::new();

    for search_path in search_paths {
        if !Path::new(search_path).exists() {
            continue;
        }
        collect_sessions(search_path, &mut sessions);
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

/// Recursively find .jsonl files and extract session metadata.
/// Skips `agent-*.jsonl` files and `subagents/` directories (duplicates of parent sessions).
fn collect_sessions(dir: &str, results: &mut Vec<ListResult>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if path.is_dir() {
            if name == "subagents" {
                continue;
            }
            collect_sessions(path.to_str().unwrap_or(""), results);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            if name.starts_with("agent-") {
                continue;
            }
            if let Some(session) = extract_session_metadata(&path) {
                results.push(session);
            }
        }
    }
}

/// Extract metadata from a single .jsonl file by reading first and last messages
fn extract_session_metadata(path: &Path) -> Option<ListResult> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let path_str = path.to_str()?;
    let project = extract_project_from_path(path_str);
    let source = SessionSource::from_path(path_str);

    let mut session_id: Option<String> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;
    let mut message_count: usize = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if let Some(msg) = Message::from_jsonl(line.trim(), 0) {
            if session_id.is_none() {
                session_id = Some(msg.session_id.clone());
            }
            if last_timestamp.is_none_or(|t| msg.timestamp > t) {
                last_timestamp = Some(msg.timestamp);
            }
            message_count += 1;
        }
    }

    let session_id = session_id?;
    let last_active = last_timestamp?.to_rfc3339();

    Some(ListResult {
        session_id,
        project,
        source: source.display_name().to_string(),
        file_path: path_str.to_string(),
        last_active,
        message_count,
    })
}
