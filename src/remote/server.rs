//! What `ccs serve` computes for each request: the same pipeline the picker
//! runs locally, finished on the machine that has the files, so the reply
//! carries everything the client would otherwise read from disk.

use super::wire::{
    RecentParams, SearchParams, ShowParams, TreeParams, WireGroup, WireMatch, WireRecent,
    WireSearch, WireShow, WireTree,
};
use crate::cli::{
    build_snippet_regex, locate_jsonl_message, locate_opencode_message, render_match_content_with,
};
use crate::project_match::remote_project_matches;
use crate::recent::{collect_recent_sessions, detect_session_automation};
use crate::search::{
    count_session_messages, extract_project_from_path, group_by_session, search_multiple_paths,
    Message, SessionGroup, CANCELLED_ERR,
};
use crate::session::record::{ContentMode, SessionRecord};
use crate::session::{opencode, read_session_cwd, SessionProvider};
use crate::tree::SessionTree;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Characters kept on each side of the match in a snippet. Shorter than
/// `ccs search`: the preview fetches the full text with `show`.
pub const SNIPPET_CONTEXT_CHARS: usize = 100;
/// Matches sent per session; the group says how many there were.
pub const MAX_MATCHES_PER_SESSION: usize = 20;
/// Sessions sent per search, newest first.
pub const MAX_SESSIONS: usize = 200;

/// Run a search here and finish each group: project scope, automation tag,
/// message count, latest-chain flags and snippets.
pub fn search(
    params: &SearchParams,
    search_paths: &[String],
    home: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<WireSearch, String> {
    let result = search_multiple_paths(&params.query, search_paths, params.regex, cancel)?;
    let snippet_regex = build_snippet_regex(&params.query, params.regex, false);
    let snippets = Snippets {
        query: &params.query,
        regex: snippet_regex.as_ref(),
    };
    let all = group_by_session(result.matches);
    // Reading files for scope, counts and chains is the slow part; spread
    // it over cores, keeping the newest-first order.
    let in_scope: Vec<SessionGroup> = all
        .into_par_iter()
        .filter(|g| in_project(&g.file_path, params.project.as_deref(), home))
        .collect();
    let truncated = result.truncated || in_scope.len() > MAX_SESSIONS;
    let kept: Vec<(SessionGroup, usize)> = with_full_quota(in_scope, params);
    let groups: Vec<WireGroup> = kept
        .into_par_iter()
        .map(|(group, full)| {
            (!cancel.load(Ordering::Relaxed)).then(|| finish_group(group, &snippets, full))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| CANCELLED_ERR.to_string())?;
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED_ERR.into());
    }
    Ok(WireSearch { groups, truncated })
}

/// The newest `MAX_SESSIONS` groups, each with how many of its first
/// matches go out untrimmed: with `full_content`, the first `limit`
/// matches newest-first, as `ccs search --full-content` does locally.
fn with_full_quota(groups: Vec<SessionGroup>, params: &SearchParams) -> Vec<(SessionGroup, usize)> {
    let mut left = if params.full_content {
        params.limit.unwrap_or(usize::MAX)
    } else {
        0
    };
    groups
        .into_iter()
        .take(MAX_SESSIONS)
        .map(|g| {
            let sent = g.matches.len().min(MAX_MATCHES_PER_SESSION);
            let full = left.min(sent);
            left -= full;
            (g, full)
        })
        .collect()
}

struct Snippets<'a> {
    query: &'a str,
    regex: Option<&'a regex::Regex>,
}

impl Snippets<'_> {
    /// The snippet and whether it is shorter than the message.
    fn cut(&self, content: &str, full: bool) -> (String, bool) {
        let shown =
            render_match_content_with(content, self.query, full, self.regex, SNIPPET_CONTEXT_CHARS);
        let cut = shown.len() < content.len();
        (shown, cut)
    }
}

fn in_project(file_path: &str, project: Option<&str>, home: &str) -> bool {
    let Some(project) = project else {
        return true;
    };
    read_session_cwd(file_path).is_some_and(|cwd| {
        remote_project_matches(SessionProvider::from_path(file_path), &cwd, project, home)
    })
}

fn finish_group(group: SessionGroup, snippets: &Snippets, full: usize) -> WireGroup {
    let file_path = group.file_path;
    let (message_count, message_count_compacted) = count_session_messages(&file_path);
    let chain = crate::resume::build_chain_from_tip(&file_path);
    let messages: Vec<Message> = group
        .matches
        .into_iter()
        .filter_map(|m| m.message)
        .collect();
    let matches_total = messages.len();
    let matches = messages
        .into_iter()
        .take(MAX_MATCHES_PER_SESSION)
        .enumerate()
        .map(|(i, msg)| wire_match(msg, snippets, i < full, chain.as_ref()))
        .collect();
    WireGroup {
        session_id: group.session_id,
        project: extract_project_from_path(&file_path),
        automation: detect_session_automation(Path::new(&file_path)),
        message_count: Some(message_count),
        message_count_compacted,
        file_path,
        matches,
        matches_total,
    }
}

/// A message without a known chain (Codex, Opencode) counts as on it, so
/// no fork marker is shown, as locally.
fn wire_match(
    msg: Message,
    snippets: &Snippets,
    full: bool,
    chain: Option<&HashSet<String>>,
) -> WireMatch {
    let on_latest_chain = match (chain, msg.uuid.as_deref()) {
        (Some(chain), Some(uuid)) => chain.contains(uuid),
        _ => true,
    };
    let (content, content_cut) = snippets.cut(&msg.content, full);
    let (text_content, text_cut) = snippets.cut(&msg.text_content, full);
    WireMatch {
        session_id: msg.session_id,
        role: msg.role,
        content,
        text_content,
        content_truncated: content_cut || text_cut,
        timestamp: msg.timestamp,
        branch: msg.branch,
        file_path: msg.file_path,
        line_number: msg.line_number,
        uuid: msg.uuid,
        parent_uuid: msg.parent_uuid,
        on_latest_chain,
    }
}

/// Recent sessions here, scoped to `project` when given.
pub fn recent(params: &RecentParams, search_paths: &[String], home: &str) -> Vec<WireRecent> {
    collect_recent_sessions(search_paths, params.limit)
        .iter()
        .filter(|s| in_project(&s.file_path, params.project.as_deref(), home))
        .map(WireRecent::from_recent)
        .collect()
}

/// Full text of one message.
pub fn show(params: &ShowParams) -> Result<WireShow, String> {
    if let Some((db, session_id)) = opencode::parse_session_path(&params.file_path) {
        let (messages, idx) = locate_opencode_message(&db, &session_id, params.uuid.as_deref())?;
        let blocks = &messages[idx].content_blocks;
        return Ok(WireShow {
            content: SessionRecord::render_content(blocks, &ContentMode::Full),
            text_content: SessionRecord::render_content(blocks, &ContentMode::TextOnly),
        });
    }
    let (messages, idx) =
        locate_jsonl_message(&params.file_path, params.line, params.uuid.as_deref())?;
    let msg = &messages[idx];
    Ok(WireShow {
        content: msg.content.clone(),
        text_content: msg.text_content.clone(),
    })
}

pub fn tree(params: &TreeParams) -> Result<WireTree, String> {
    SessionTree::from_file(&params.file_path).map(|tree| WireTree::from_tree(&tree))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
    }

    fn fixtures_dir() -> Vec<String> {
        vec![format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))]
    }

    fn params(query: &str) -> SearchParams {
        SearchParams {
            query: query.into(),
            regex: false,
            project: None,
            full_content: false,
            limit: None,
        }
    }

    fn not_cancelled() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn group_for<'a>(result: &'a WireSearch, file: &str) -> &'a WireGroup {
        result
            .groups
            .iter()
            .find(|g| g.file_path.ends_with(file))
            .unwrap_or_else(|| panic!("no group for {file}"))
    }

    #[test]
    fn server_search_when_query_matches_fixture_then_group_has_counts_and_project() {
        let result = search(
            &params("sort a list"),
            &fixtures_dir(),
            "/home/m",
            &not_cancelled(),
        )
        .unwrap();
        let group = group_for(&result, "linear_session.jsonl");
        assert!(group.message_count.is_some());
        assert!(!group.project.is_empty());
        assert_eq!(group.matches_total, group.matches.len());
        for m in &group.matches {
            assert!(m.content.chars().count() <= 2 * SNIPPET_CONTEXT_CHARS + 11 + 6);
        }
    }

    #[test]
    fn server_search_when_session_has_many_matches_then_capped_with_total() {
        let dir = tempfile::TempDir::new().unwrap();
        let lines: String = (0..MAX_MATCHES_PER_SESSION + 5)
            .map(|i| {
                format!(
                    r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"needle {i}"}}]}},"uuid":"u{i}","sessionId":"s","timestamp":"2026-01-01T00:00:{:02}Z"}}"#,
                    i % 60
                ) + "\n"
            })
            .collect();
        std::fs::write(dir.path().join("s.jsonl"), lines).unwrap();
        let paths = vec![dir.path().to_string_lossy().to_string()];
        let result = search(&params("needle"), &paths, "/home/m", &not_cancelled()).unwrap();
        let group = &result.groups[0];
        assert_eq!(group.matches.len(), MAX_MATCHES_PER_SESSION);
        assert_eq!(group.matches_total, MAX_MATCHES_PER_SESSION + 5);
    }

    #[test]
    fn server_search_when_long_message_then_snippet_uses_server_context() {
        let dir = tempfile::TempDir::new().unwrap();
        let text = format!("{} needle {}", "a".repeat(500), "b".repeat(500));
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}},"uuid":"u1","sessionId":"s","timestamp":"2026-01-01T00:00:00Z"}}"#
        );
        std::fs::write(dir.path().join("s.jsonl"), line + "\n").unwrap();
        let paths = vec![dir.path().to_string_lossy().to_string()];
        let result = search(&params("needle"), &paths, "/home/m", &not_cancelled()).unwrap();
        let m = &result.groups[0].matches[0];
        assert!(
            m.content.chars().count() <= 2 * SNIPPET_CONTEXT_CHARS + 6 + 6,
            "{}",
            m.content.len()
        );
        assert!(m.content_truncated);
    }

    #[test]
    fn server_search_when_branched_then_only_latest_chain_matches_flagged() {
        let result = search(
            &params("function"),
            &fixtures_dir(),
            "/home/m",
            &not_cancelled(),
        )
        .unwrap();
        let group = group_for(&result, "branched_session.jsonl");
        assert!(group.matches.iter().any(|m| m.on_latest_chain));
        assert!(group.matches.iter().any(|m| !m.on_latest_chain));
    }

    #[test]
    fn server_search_when_cancelled_then_err_cancelled() {
        let cancel = Arc::new(AtomicBool::new(true));
        let err = search(&params("function"), &fixtures_dir(), "/home/m", &cancel).unwrap_err();
        assert_eq!(err, "cancelled");
    }

    #[test]
    fn server_search_when_project_matches_session_cwd_then_only_that_session() {
        let dir = tempfile::TempDir::new().unwrap();
        for (name, cwd) in [
            ("in.jsonl", "/home/m/projects/x"),
            ("out.jsonl", "/home/m/projects/y"),
        ] {
            let line = format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"needle"}}]}},"uuid":"u1","sessionId":"{name}","cwd":"{cwd}","timestamp":"2026-01-01T00:00:00Z"}}"#
            );
            std::fs::write(dir.path().join(name), line + "\n").unwrap();
        }
        let mut p = params("needle");
        p.project = Some("projects/x".into());
        let paths = vec![dir.path().to_string_lossy().to_string()];
        let result = search(&p, &paths, "/home/m", &not_cancelled()).unwrap();
        assert_eq!(result.groups.len(), 1);
        assert!(result.groups[0].file_path.ends_with("in.jsonl"));
    }

    #[test]
    fn server_search_when_project_mismatch_then_no_groups() {
        let mut p = params("function");
        p.project = Some("projects/none".into());
        let result = search(&p, &fixtures_dir(), "/home/m", &not_cancelled()).unwrap();
        assert!(result.groups.is_empty());
    }

    #[test]
    fn server_recent_when_fixtures_then_sessions_with_projects() {
        let recent = recent(
            &RecentParams {
                limit: 100,
                project: None,
            },
            &fixtures_dir(),
            "/home/m",
        );
        assert!(recent
            .iter()
            .any(|r| r.file_path.ends_with("linear_session.jsonl")));
    }

    #[test]
    fn server_tree_when_branched_then_tip_and_branch_count() {
        let tree = tree(&TreeParams {
            file_path: fixture("branched_session.jsonl"),
        })
        .unwrap();
        assert!(tree.tip_uuid.is_some());
        assert!(tree.branch_count >= 1);
        assert!(!tree.rows.is_empty());
    }

    #[test]
    fn server_show_when_uuid_given_then_full_text() {
        let shown = show(&ShowParams {
            file_path: fixture("linear_session.jsonl"),
            line: None,
            uuid: Some("u2".into()),
        })
        .unwrap();
        assert!(shown.content.contains("sorted() function"));
    }

    #[test]
    fn server_show_when_line_missing_then_error() {
        let err = show(&ShowParams {
            file_path: fixture("linear_session.jsonl"),
            line: Some(10_000),
            uuid: None,
        })
        .unwrap_err();
        assert!(err.contains("beyond end of file"), "{err}");
    }
}
