use crate::remote::client::{Reply, Request};
use crate::remote::oneshot::{query_hosts, HostOutcome};
use crate::remote::wire::{RecentParams, SearchParams};
use crate::remote::{Host, HostSelection};
use crate::search::{
    extract_context, extract_context_around_span, extract_project_from_path, group_by_session,
    search_multiple_paths, Message, SessionGroup,
};
use crate::session::{collect_session_jsonl_files, SessionProvider, SessionSource};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Serialize)]
struct CliSearchResult {
    #[serde(rename = "type")]
    record_type: &'static str,
    /// `"local"` or the remote host the session lives on.
    host: String,
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
    /// One entry per host asked, `local` first.
    hosts: Vec<CliHostSummary>,
}

/// How one host answered a CLI command.
#[derive(Serialize)]
struct CliHostSummary {
    name: String,
    status: &'static str,
    shown: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// How long CLI commands wait for remote hosts, next to the local work.
pub const CLI_SEARCH_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
pub const CLI_LIST_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Default `--host` for `search`, `list` and `pick`.
pub const CLI_DEFAULT_HOSTS: &str = "all";

#[derive(Serialize)]
struct ListResult {
    host: String,
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
    selection: &HostSelection,
) {
    let remote = spawn_remote_query(
        selection,
        Request::Search(SearchParams {
            query: query.to_string(),
            regex: use_regex,
            project: None,
            full_content,
            limit: Some(limit),
        }),
        CLI_SEARCH_WAIT,
    );

    // CLI search is one-shot and runs to completion; no cancellation is needed,
    // but the lower-level API requires a token, so we pass a permanently-false one.
    let cancel = Arc::new(AtomicBool::new(false));
    let (local_matches, local_truncated) = if selection.local {
        match search_multiple_paths(query, search_paths, use_regex, &cancel) {
            Ok(r) => (r.matches, r.truncated),
            Err(e) => {
                eprintln!("Search error: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        (Vec::new(), false)
    };

    if local_truncated {
        eprintln!("Warning: results may be incomplete (per-file match limit reached)");
    }

    let snippet_regex = build_snippet_regex(query, use_regex, full_content);

    let mut groups = group_by_session(local_matches);
    let mut truncated = local_truncated;
    let outcomes = join_remote(remote);
    for (name, outcome) in &outcomes {
        if let HostOutcome::Reply(Reply::Search(found)) = outcome {
            truncated |= found.truncated;
            let host = Host::Remote(name.clone());
            groups.extend(
                found
                    .groups
                    .iter()
                    .cloned()
                    .map(|g| g.into_group(host.clone()).0),
            );
        }
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.latest_timestamp()));

    // A host sends at most a few matches per session and says how many it
    // found; count those so `total_matches` and `truncated` stay honest.
    let total_matches: usize = groups
        .iter()
        .map(|g| match g.matches_total {
            Some(total) => total,
            None => g.matches.iter().filter(|m| m.message.is_some()).count(),
        })
        .sum();

    let printed = print_group_matches(&groups, query, full_content, limit, snippet_regex.as_ref());

    // The summary is always the last line, even with zero matches, so machine
    // consumers can tell "nothing found" from "search did not run".
    let summary = CliSearchSummary {
        record_type: "summary",
        shown: printed.shown,
        total_matches,
        sessions: printed.sessions.len(),
        truncated: truncated || total_matches > printed.shown,
        hosts: host_summaries(selection, &outcomes, &printed.per_host),
    };
    print_json_line(&summary);
}

type RemoteQuery = Option<std::thread::JoinHandle<Vec<(String, HostOutcome)>>>;

/// Start asking the selected remote hosts; local work runs meanwhile.
fn spawn_remote_query(
    selection: &HostSelection,
    request: Request,
    wait: std::time::Duration,
) -> RemoteQuery {
    if selection.remote.is_empty() {
        return None;
    }
    let hosts = selection.remote.clone();
    Some(std::thread::spawn(move || {
        query_hosts(&hosts, &request, wait)
    }))
}

fn join_remote(remote: RemoteQuery) -> Vec<(String, HostOutcome)> {
    remote.and_then(|h| h.join().ok()).unwrap_or_default()
}

fn host_summaries(
    selection: &HostSelection,
    outcomes: &[(String, HostOutcome)],
    shown: &std::collections::HashMap<String, usize>,
) -> Vec<CliHostSummary> {
    let count = |name: &str| shown.get(name).copied().unwrap_or(0);
    let mut out = Vec::new();
    if selection.local {
        out.push(CliHostSummary {
            name: "local".to_string(),
            status: "ok",
            shown: count("local"),
            error: None,
        });
    }
    out.extend(outcomes.iter().map(|(name, outcome)| {
        let (status, error) = outcome.status();
        CliHostSummary {
            name: name.clone(),
            status,
            shown: count(name),
            error,
        }
    }));
    out
}

/// In regex mode the query pattern rarely appears literally in the content,
/// so the snippet is anchored on the first actual regex match instead.
pub(crate) fn build_snippet_regex(
    query: &str,
    use_regex: bool,
    full_content: bool,
) -> Option<regex::Regex> {
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
pub(crate) fn render_match_content(
    content: &str,
    query: &str,
    full_content: bool,
    snippet_regex: Option<&regex::Regex>,
) -> String {
    render_match_content_with(
        content,
        query,
        full_content,
        snippet_regex,
        SNIPPET_CONTEXT_CHARS,
    )
}

/// [`render_match_content`] with `context_chars` on each side of the match.
pub(crate) fn render_match_content_with(
    content: &str,
    query: &str,
    full_content: bool,
    snippet_regex: Option<&regex::Regex>,
    context_chars: usize,
) -> String {
    if full_content {
        content.to_string()
    } else if let Some(found) = snippet_regex.and_then(|re| re.find(content)) {
        extract_context_around_span(content, found.start(), found.end(), context_chars)
    } else {
        extract_context(content, query, context_chars)
    }
}

/// What `print_group_matches` printed.
struct Printed {
    shown: usize,
    sessions: std::collections::HashSet<String>,
    /// Rows printed per host label.
    per_host: std::collections::HashMap<String, usize>,
}

/// Print one JSON line per match, stopping at `limit`.
fn print_group_matches(
    groups: &[SessionGroup],
    query: &str,
    full_content: bool,
    limit: usize,
    snippet_regex: Option<&regex::Regex>,
) -> Printed {
    let mut printed = Printed {
        shown: 0,
        sessions: std::collections::HashSet::new(),
        per_host: std::collections::HashMap::new(),
    };

    'groups: for group in groups {
        // A remote host sent the label; its files are not here to read.
        let project = group
            .project_label
            .clone()
            .unwrap_or_else(|| extract_project_from_path(&group.file_path));
        let provider = SessionProvider::from_path(&group.file_path);
        let source = SessionSource::from_path(&group.file_path);
        let is_opencode = crate::session::opencode::parse_session_path(&group.file_path).is_some();
        let host = group.host.label().to_string();

        for m in &group.matches {
            if printed.shown >= limit {
                break 'groups;
            }

            if let Some(ref msg) = m.message {
                let result = CliSearchResult {
                    record_type: "match",
                    host: host.clone(),
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
                    printed.shown += 1;
                    printed.sessions.insert(msg.session_id.clone());
                    *printed.per_host.entry(host.clone()).or_default() += 1;
                }
            }
        }
    }

    printed
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
    /// The file the message is in: for a Codex rollout, inherited history
    /// comes from its base files.
    file_path: String,
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
    /// `"tail"` when the window is the end of the session instead of a
    /// window around a target.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
    /// Messages on the session's current line, in tail mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    total_messages: Option<usize>,
}

/// Messages `ccs show` prints when neither `--line` nor `--uuid` is given.
pub const DEFAULT_TAIL: usize = 10;

fn is_false(b: &bool) -> bool {
    !*b
}

/// Run CLI show command — print a window of messages around a search hit,
/// or the end of the session.
///
/// `file_path`, `--line` and `--uuid` come straight from `ccs search` output
/// (`file_path`, `line_number`, `message_uuid`). The window is `context`
/// messages before and after the target, in file order. Without `--line` and
/// `--uuid` it is the last `tail` messages (default [`DEFAULT_TAIL`]) of the
/// session's current line.
#[allow(clippy::too_many_arguments)]
pub fn cli_show(
    target: &str,
    line: Option<usize>,
    uuid: Option<&str>,
    tail: Option<usize>,
    context: usize,
    max_chars: usize,
    host: Option<&crate::remote::config::HostConfig>,
    search_paths: &[String],
) {
    let tail = (line.is_none() && uuid.is_none()).then(|| tail.unwrap_or(DEFAULT_TAIL));
    if let Some(host) = host {
        // The host resolves a session id against its own files.
        std::process::exit(show_remote(
            host, target, line, uuid, tail, context, max_chars,
        ));
    }
    let file_path = match resolve_show_target(target, search_paths) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };
    let file_path = file_path.as_str();
    let opencode = crate::session::opencode::parse_session_path(file_path);
    let result = match (opencode, tail) {
        (Some((db, session_id)), Some(n)) => {
            show_opencode_tail(&db, &session_id, file_path, n, max_chars)
        }
        (Some((db, session_id)), None) => {
            show_opencode(&db, &session_id, file_path, uuid, context, max_chars)
        }
        (None, Some(n)) => show_jsonl_tail(file_path, n, max_chars),
        (None, None) => show_jsonl(file_path, line, uuid, context, max_chars),
    };
    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// A session target is a file path, the same rule as `--tree`, or a bare
/// session id.
pub fn is_path_like(target: &str) -> bool {
    target.contains('/')
        || target.ends_with(".jsonl")
        // `opencode.db#ses_…`: the synthetic path a relative Opencode db gives
        || crate::session::opencode::parse_session_path(target).is_some()
}

/// Resolve a `ccs show` target to a session file: paths (including Opencode's
/// `<db>#<session_id>`) are used as given, a session id is looked up in
/// `search_paths` (for a Codex thread, its current rollout).
pub fn resolve_show_target(target: &str, search_paths: &[String]) -> Result<String, String> {
    if is_path_like(target) {
        return Ok(target.to_string());
    }
    crate::session::find_session_file_in_paths(target, search_paths)
        .ok_or_else(|| format!("no session {} in search paths", target))
}

/// `ccs show` on the host that has the file: the window of messages is
/// exactly what that ccs prints, so its output passes straight through.
/// Tail mode sends `--tail` explicitly and no `--context`: the remote ccs
/// rejects the two together.
fn show_remote(
    host: &crate::remote::config::HostConfig,
    target: &str,
    line: Option<usize>,
    uuid: Option<&str>,
    tail: Option<usize>,
    context: usize,
    max_chars: usize,
) -> i32 {
    let mut args = vec!["show".to_string(), target.to_string()];
    if let Some(line) = line {
        args.extend(["--line".to_string(), line.to_string()]);
    }
    if let Some(uuid) = uuid {
        args.extend(["--uuid".to_string(), uuid.to_string()]);
    }
    match tail {
        Some(n) => args.extend(["--tail".to_string(), n.to_string()]),
        None => args.extend(["--context".to_string(), context.to_string()]),
    }
    args.extend(["--max-chars".to_string(), max_chars.to_string()]);
    let mut command =
        crate::remote::ssh::ssh_command(host, &crate::remote::ssh::SshMode::Command(&args));
    match command.status() {
        Ok(status) if status.code() == Some(255) => {
            eprintln!("Error: {}: не удалось подключиться", host.name);
            1
        }
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("Error: cannot start ssh: {e}");
            1
        }
    }
}

fn show_jsonl(
    file_path: &str,
    line: Option<usize>,
    uuid: Option<&str>,
    context: usize,
    max_chars: usize,
) -> Result<(), String> {
    let (messages, target_idx) = locate_jsonl_message(file_path, line, uuid)?;

    let start = target_idx.saturating_sub(context);
    let end = target_idx
        .saturating_add(context)
        .saturating_add(1)
        .min(messages.len());

    for (i, msg) in messages[start..end].iter().enumerate() {
        print_json_line(&jsonl_row(
            msg,
            file_path,
            start + i == target_idx,
            max_chars,
        ));
    }

    let target = &messages[target_idx];
    let summary = ShowSummary {
        session_id: target.session_id.clone(),
        shown: end - start,
        target_line: Some(target.line_number),
        ..show_summary(file_path)
    };
    print_json_line(&summary);
    Ok(())
}

/// The last `n` messages of a JSONL session's current line: the latest
/// branch of a Claude session, the whole stitched history of a Codex one.
fn show_jsonl_tail(file_path: &str, n: usize, max_chars: usize) -> Result<(), String> {
    fs::File::open(file_path).map_err(|e| format!("cannot open {}: {}", file_path, e))?;
    let messages = jsonl_history_messages(file_path, |_| {});
    let latest_chain = crate::dag::SessionDag::from_file(
        Path::new(file_path),
        crate::dag::DisplayFilter::Standard,
    )
    .ok()
    .and_then(|dag| {
        let tip = dag.tip(crate::dag::TipStrategy::LastAppended)?;
        Some(dag.chain_from(tip))
    });
    // Records without a DAG uuid (Codex) are a linear history: all on the line.
    let current: Vec<&Message> = messages
        .iter()
        .filter(|m| match (&latest_chain, &m.uuid) {
            (Some(chain), Some(uuid)) => chain.contains(uuid),
            _ => true,
        })
        .collect();

    let start = current.len().saturating_sub(n);
    for msg in &current[start..] {
        print_json_line(&jsonl_row(msg, file_path, false, max_chars));
    }

    let summary = ShowSummary {
        session_id: own_session_id(file_path).unwrap_or_default(),
        shown: current.len() - start,
        mode: Some("tail"),
        total_messages: Some(current.len()),
        ..show_summary(file_path)
    };
    print_json_line(&summary);
    Ok(())
}

/// The session id from the file's own records. Not from its messages: a
/// Codex fork's history, and so its tail, may lie wholly in its parent's file.
fn own_session_id(file_path: &str) -> Option<String> {
    crate::session::codex_history::file_lines(Path::new(file_path))
        .take(50)
        .find_map(|line| {
            let json: serde_json::Value = serde_json::from_str(&line.raw).ok()?;
            crate::session::extract_session_id(&json)
        })
        .or_else(|| crate::session::extract_codex_session_id_from_path(file_path))
}

/// Summary fields that depend only on the file; callers fill in the window.
fn show_summary(file_path: &str) -> ShowSummary {
    ShowSummary {
        record_type: "summary",
        session_id: String::new(),
        file_path: file_path.to_string(),
        provider: SessionProvider::from_path(file_path)
            .display_name()
            .to_string(),
        source: SessionSource::from_path(file_path)
            .display_name()
            .to_string(),
        project: extract_project_from_path(file_path),
        shown: 0,
        target_line: None,
        target_uuid: None,
        mode: None,
        total_messages: None,
    }
}

fn jsonl_row(msg: &Message, file_path: &str, is_target: bool, max_chars: usize) -> ShowMessageRow {
    let (content, content_truncated) = truncate_chars(&msg.content, max_chars);
    ShowMessageRow {
        record_type: "message",
        file_path: msg
            .file_path
            .clone()
            .unwrap_or_else(|| file_path.to_string()),
        line_number: Some(msg.line_number),
        message_uuid: msg.uuid.clone(),
        parent_uuid: msg.parent_uuid.clone(),
        role: msg.role.clone(),
        timestamp: msg.timestamp.to_rfc3339(),
        content,
        is_target,
        content_truncated,
    }
}

/// Messages of a JSONL session's history in order: for a Codex rollout,
/// what it inherits through `history_base` first. Each message keeps its own
/// file and line number; `on_other_line` sees every line that is not one.
fn jsonl_history_messages(
    file_path: &str,
    mut on_other_line: impl FnMut(&crate::session::codex_history::HistoryLine),
) -> Vec<Message> {
    let mut messages = Vec::new();
    for history_line in crate::session::codex_history::history_lines(Path::new(file_path)) {
        let line_file = history_line.path.to_string_lossy();
        match Message::from_jsonl_with_path(
            &history_line.raw,
            history_line.line_number,
            Some(&line_file),
        ) {
            Some(msg) => messages.push(msg),
            None => on_other_line(&history_line),
        }
    }
    messages
}

/// Parse every message of a JSONL session and find the one `line` or `uuid`
/// names. Shared by `ccs show` and `ccs serve`'s `show`.
pub(crate) fn locate_jsonl_message(
    file_path: &str,
    line: Option<usize>,
    uuid: Option<&str>,
) -> Result<(Vec<Message>, usize), String> {
    enum Anchor<'a> {
        Line(usize),
        Uuid(&'a str),
    }
    let anchor = match (line, uuid) {
        (Some(n), _) => Anchor::Line(n),
        (None, Some(u)) => Anchor::Uuid(u),
        (None, None) => {
            return Err(
                "pass --line or --uuid to anchor the window (both come from search output)"
                    .to_string(),
            )
        }
    };

    fs::File::open(file_path).map_err(|e| format!("cannot open {}: {}", file_path, e))?;
    let own_path = Path::new(file_path);

    // If --line points at a record that isn't a message, remember what it is
    // so the error names the record type instead of a bare "not found".
    let mut anchor_line_kind: Option<String> = None;
    let mut last_other_line = 0usize;
    let messages = jsonl_history_messages(file_path, |other| {
        if &*other.path == own_path {
            last_other_line = other.line_number;
            if line == Some(other.line_number) {
                anchor_line_kind = Some(classify_non_message_line(&other.raw));
            }
        }
    });
    let total_lines = messages
        .iter()
        .filter(|m| m.file_path.as_deref() == Some(file_path))
        .map(|m| m.line_number)
        .chain([last_other_line])
        .max()
        .unwrap_or(0);

    let target_idx = match anchor {
        Anchor::Line(n) => messages
            .iter()
            .position(|m| m.line_number == n && m.file_path.as_deref() == Some(file_path))
            .ok_or_else(|| match &anchor_line_kind {
                Some(kind) => format!("line {} is not a message record ({})", n, kind),
                None => format!("line {} is beyond end of file ({} lines)", n, total_lines),
            })?,
        Anchor::Uuid(u) => messages
            .iter()
            .position(|m| m.uuid.as_deref() == Some(u))
            .ok_or_else(|| format!("no message with uuid {} in {}", u, file_path))?,
    };
    Ok((messages, target_idx))
}

/// Load an Opencode session's messages and find the one `uuid` names.
/// Shared by `ccs show` and `ccs serve`'s `show`.
pub(crate) fn locate_opencode_message(
    db_path: &Path,
    session_id: &str,
    uuid: Option<&str>,
) -> Result<(Vec<crate::session::opencode::OpencodeMessage>, usize), String> {
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
    Ok((messages, target_idx))
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
    let (messages, target_idx) = locate_opencode_message(db_path, session_id, uuid)?;

    let start = target_idx.saturating_sub(context);
    let end = target_idx
        .saturating_add(context)
        .saturating_add(1)
        .min(messages.len());

    for (i, msg) in messages[start..end].iter().enumerate() {
        print_json_line(&opencode_row(
            msg,
            file_path,
            start + i == target_idx,
            max_chars,
        ));
    }

    let summary = ShowSummary {
        session_id: session_id.to_string(),
        shown: end - start,
        target_uuid: Some(messages[target_idx].id.clone()),
        ..show_summary(file_path)
    };
    print_json_line(&summary);
    Ok(())
}

/// The last `n` messages of an Opencode session.
fn show_opencode_tail(
    db_path: &Path,
    session_id: &str,
    file_path: &str,
    n: usize,
    max_chars: usize,
) -> Result<(), String> {
    let messages = crate::session::opencode::load_messages(db_path, session_id);
    if messages.is_empty() {
        return Err(format!(
            "no messages found for session {} in {}",
            session_id,
            db_path.display()
        ));
    }
    let start = messages.len().saturating_sub(n);
    for msg in &messages[start..] {
        print_json_line(&opencode_row(msg, file_path, false, max_chars));
    }

    let summary = ShowSummary {
        session_id: session_id.to_string(),
        shown: messages.len() - start,
        mode: Some("tail"),
        total_messages: Some(messages.len()),
        ..show_summary(file_path)
    };
    print_json_line(&summary);
    Ok(())
}

fn opencode_row(
    msg: &crate::session::opencode::OpencodeMessage,
    file_path: &str,
    is_target: bool,
    max_chars: usize,
) -> ShowMessageRow {
    use crate::session::record::{ContentMode, MessageRole, SessionRecord};

    let rendered = SessionRecord::render_content(&msg.content_blocks, &ContentMode::Full);
    let (content, content_truncated) = truncate_chars(&rendered, max_chars);
    let role = match msg.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    };
    ShowMessageRow {
        record_type: "message",
        file_path: file_path.to_string(),
        line_number: None,
        message_uuid: Some(msg.id.clone()),
        parent_uuid: msg.parent_id.clone(),
        role: role.to_string(),
        timestamp: msg.created_at.to_rfc3339(),
        content,
        is_target,
        content_truncated,
    }
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
pub fn cli_list(search_paths: &[String], limit: usize, selection: &HostSelection) {
    let remote = spawn_remote_query(
        selection,
        Request::Recent(RecentParams {
            limit,
            project: None,
        }),
        CLI_LIST_WAIT,
    );
    let mut sessions = if selection.local {
        local_list(search_paths, limit)
    } else {
        Vec::new()
    };
    for (name, outcome) in join_remote(remote) {
        match outcome {
            HostOutcome::Reply(Reply::Recent(rows)) => {
                sessions.extend(rows.iter().map(|r| remote_list_row(&name, r)))
            }
            other => {
                let (status, error) = other.status();
                eprintln!("{name}: {status} {}", error.unwrap_or_default());
            }
        }
    }

    // Sort by last_active descending
    sessions.sort_by(|a, b| b.last_active.cmp(&a.last_active));

    // Deduplicate by session_id on each host (keep newest)
    let mut seen = std::collections::HashSet::new();
    sessions.retain(|s| seen.insert((s.host.clone(), s.session_id.clone())));

    for session in sessions.iter().take(limit) {
        if let Ok(json) = serde_json::to_string(session) {
            println!("{}", json);
        }
    }
}

fn remote_list_row(host: &str, row: &crate::remote::wire::WireRecent) -> ListResult {
    ListResult {
        host: host.to_string(),
        session_id: row.session_id.clone(),
        project: row.project.clone(),
        provider: SessionProvider::from_path(&row.file_path)
            .display_name()
            .to_string(),
        source: SessionSource::from_path(&row.file_path)
            .display_name()
            .to_string(),
        file_path: row.file_path.clone(),
        last_active: row.timestamp.to_rfc3339(),
        message_count: row.message_count.unwrap_or(0),
    }
}

fn local_list(search_paths: &[String], limit: usize) -> Vec<ListResult> {
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

    sessions
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
                host: "local".to_string(),
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

/// Extract metadata from a session's history. The session id comes from the
/// file's own records: a Codex fork's history starts with its parent's.
fn extract_session_metadata(path: &Path) -> Option<ListResult> {
    if !path.is_file() {
        return None;
    }

    let path_str = path.to_str()?;
    let project = extract_project_from_path(path_str);
    let provider = SessionProvider::from_path(path_str);
    let source = SessionSource::from_path(path_str);

    let mut session_id: Option<String> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;

    for line in crate::session::codex_history::history_lines(path) {
        let json: serde_json::Value = match serde_json::from_str(line.raw.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session_id.is_none() && &*line.path == path {
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
        host: "local".to_string(),
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

    #[test]
    fn resolve_show_target_when_relative_opencode_path_then_used_as_given() {
        assert_eq!(
            resolve_show_target("opencode.db#ses_abc", &[]),
            Ok("opencode.db#ses_abc".to_string())
        );
    }
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

    fn linear_fixture() -> String {
        format!(
            "{}/tests/fixtures/linear_session.jsonl",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn locate_jsonl_message_when_line_and_uuid_given_then_line_wins() {
        let (messages, idx) = locate_jsonl_message(&linear_fixture(), Some(3), Some("u1")).unwrap();
        assert_eq!(messages[idx].uuid.as_deref(), Some("u3"));
    }

    #[test]
    fn locate_jsonl_message_when_no_anchor_then_error() {
        let err = locate_jsonl_message(&linear_fixture(), None, None).unwrap_err();
        assert!(
            err.contains("pass --line or --uuid"),
            "unexpected error: {err}"
        );
    }
}
