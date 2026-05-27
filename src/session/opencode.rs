//! Opencode session storage layer (SQLite).
//!
//! Opencode 1.x stores all session state in a single SQLite database at
//! `<opencode_home>/opencode.db`. Older releases used a file tree under
//! `<opencode_home>/storage/{session,message,part}/...`; ccs no longer reads
//! that legacy layout.
//!
//! Database schema (relevant subset):
//!
//! ```sql
//! CREATE TABLE session (
//!     id, project_id, slug, directory, title,
//!     time_created INTEGER, time_updated INTEGER,
//!     ...
//! );
//! CREATE TABLE message (
//!     id, session_id, time_created INTEGER, time_updated INTEGER,
//!     data TEXT  -- JSON: {role, parentID?, time:{created,...}, ...}
//! );
//! CREATE TABLE part (
//!     id, message_id, session_id, time_created INTEGER, time_updated INTEGER,
//!     data TEXT  -- JSON: {type, text?, state?, ...}
//! );
//! CREATE TABLE project ( id, worktree, vcs, name, ... );
//! ```
//!
//! Synthetic file paths: outside this module, sessions are identified by
//! `<opencode_home>/opencode.db#<session_id>`. The `#`-fragment encoding lets
//! the rest of ccs keep a single `file_path: String` field on every record
//! while routing back to the SQLite loader via `is_opencode_session_path`.

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags, Row};
use serde_json::Value;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::session::record::{ContentBlock, MessageRole, SessionRecord};

const DB_FILENAME: &str = "opencode.db";
const PATH_FRAGMENT_SEP: &str = "#";

/// Resolve the Opencode "home" directory (the parent of `opencode.db`).
///
/// Precedence:
/// 1. `$OPENCODE_DATA` if set and existing
/// 2. `$XDG_DATA_HOME/opencode` if set and existing (XDG Base Directory spec)
/// 3. `~/.local/share/opencode` (Linux/XDG default)
/// 4. `~/Library/Application Support/opencode` (macOS)
pub fn opencode_storage_root() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("OPENCODE_DATA") {
        let path = PathBuf::from(custom);
        if path.exists() {
            return Some(path);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            let path = PathBuf::from(xdg).join("opencode");
            if path.exists() {
                return Some(path);
            }
        }
    }
    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".local/share/opencode"),
        home.join("Library/Application Support/opencode"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Path to the Opencode database, if a home directory was found and the file
/// exists on disk.
pub fn opencode_database_path() -> Option<PathBuf> {
    let db = opencode_storage_root()?.join(DB_FILENAME);
    db.exists().then_some(db)
}

/// True when `path` looks like an Opencode synthetic path
/// (`<root>/opencode.db#<session_id>` or just `<root>/opencode.db`).
pub fn is_opencode_session_path(path: &str) -> bool {
    let normalized = if path.contains('\\') {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    normalized.contains("/opencode.db")
}

/// Build the synthetic ccs path used to identify an Opencode session.
pub fn synthetic_session_path(db_path: &Path, session_id: &str) -> String {
    format!(
        "{}{}{}",
        db_path.to_string_lossy(),
        PATH_FRAGMENT_SEP,
        session_id
    )
}

/// Extract `(db_path, session_id)` from a synthetic path produced by
/// `synthetic_session_path`. Returns `None` if the path is not Opencode-shaped.
pub fn parse_session_path(path: &str) -> Option<(PathBuf, String)> {
    let (db, sid) = path.rsplit_once(PATH_FRAGMENT_SEP)?;
    if !db.ends_with(DB_FILENAME) {
        return None;
    }
    Some((PathBuf::from(db), sid.to_string()))
}

/// Open a read-only connection to the Opencode database.
///
/// We open `READ_ONLY | URI` and append `?mode=ro` so we never accidentally
/// write to the user's database, even when they have a hot Opencode running.
pub(crate) fn open_db(db_path: &Path) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?mode=ro", db_path.to_string_lossy());
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

/// Metadata for a single Opencode session as stored in the `session` table.
#[derive(Debug, Clone)]
pub struct OpencodeSession {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub directory: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Synthetic path identifying this session for the rest of ccs.
    pub session_file: PathBuf,
}

impl OpencodeSession {
    fn from_row(row: &Row<'_>, db_path: &Path) -> rusqlite::Result<Self> {
        let id: String = row.get("id")?;
        let project_id: String = row.get("project_id")?;
        let title: String = row.get("title")?;
        let directory: Option<String> = row.get("directory")?;
        let time_created: i64 = row.get("time_created")?;
        let time_updated: i64 = row.get("time_updated")?;
        let synthetic = synthetic_session_path(db_path, &id);
        Ok(OpencodeSession {
            id,
            project_id,
            title,
            directory: directory.filter(|s| !s.is_empty()),
            created_at: millis_to_datetime(time_created).unwrap_or_else(Utc::now),
            updated_at: millis_to_datetime(time_updated)
                .unwrap_or_else(|| millis_to_datetime(time_created).unwrap_or_else(Utc::now)),
            session_file: PathBuf::from(&synthetic),
        })
    }

    /// Load a single session by id. Returns `None` if the session does not
    /// exist or the database can't be opened.
    pub fn from_db(db_path: &Path, session_id: &str) -> Option<Self> {
        let conn = open_db(db_path).ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, project_id, title, directory, time_created, time_updated \
                 FROM session WHERE id = ?1",
            )
            .ok()?;
        let mut rows = stmt.query([session_id]).ok()?;
        let row = rows.next().ok()??;
        Self::from_row(row, db_path).ok()
    }

    /// Resolve a session from the synthetic `file_path` representation.
    pub fn from_file(path: &Path) -> Option<Self> {
        let path_str = path.to_str()?;
        let (db, sid) = parse_session_path(path_str)?;
        Self::from_db(&db, &sid)
    }
}

/// Look up an Opencode project's display label.
///
/// Returns the project's `name` when set, otherwise the basename of the
/// `worktree` path, otherwise the raw worktree.
pub fn read_project_label(db_path: &Path, project_id: &str) -> Option<String> {
    let conn = open_db(db_path).ok()?;
    let mut stmt = conn
        .prepare("SELECT name, worktree FROM project WHERE id = ?1")
        .ok()?;
    let mut rows = stmt.query([project_id]).ok()?;
    let row = rows.next().ok()??;
    let name: Option<String> = row.get("name").ok();
    let worktree: Option<String> = row.get("worktree").ok();
    derive_project_label(name, worktree)
}

/// Pick a display label from a `(name, worktree)` pair using the same rules
/// as [`read_project_label`]. Extracted so the JOIN-based session-summary
/// query can reuse the precedence without re-querying the `project` table.
fn derive_project_label(name: Option<String>, worktree: Option<String>) -> Option<String> {
    if let Some(n) = name.as_ref().filter(|s| !s.is_empty()) {
        return Some(n.clone());
    }
    let worktree = worktree?;
    if let Some(base) = Path::new(&worktree)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
    {
        return Some(base.to_string());
    }
    Some(worktree)
}

/// Read the worktree path for a project (used as the resume cwd).
pub fn read_project_worktree(db_path: &Path, project_id: &str) -> Option<String> {
    let conn = open_db(db_path).ok()?;
    let mut stmt = conn
        .prepare("SELECT worktree FROM project WHERE id = ?1")
        .ok()?;
    let mut rows = stmt.query([project_id]).ok()?;
    let row = rows.next().ok()??;
    row.get::<_, String>("worktree").ok()
}

/// Compact session row used by the "recent sessions" and `ccs list` paths.
///
/// One JOIN against `session`/`project` populates every field the callers
/// need, so they avoid the per-session DB round-trips that
/// `OpencodeSession::from_db` + `read_project_label` + `load_messages` would
/// otherwise force.
#[derive(Debug, Clone)]
pub struct OpencodeSessionSummary {
    pub id: String,
    pub project_id: String,
    pub project_label: Option<String>,
    pub title: String,
    pub directory: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    pub session_file: PathBuf,
}

/// Return the most-recently-updated Opencode sessions (with directory set),
/// enriched with project label and message count in a single query.
///
/// Skips sessions whose `directory` is empty (Opencode-internal global state,
/// equivalent to the old `session/global/` bucket).
pub fn list_sessions_for_recent(db_path: &Path, limit: usize) -> Vec<OpencodeSessionSummary> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(conn) = open_db(db_path) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(SQL_LIST_RECENT) else {
        return Vec::new();
    };
    let limit_i64: i64 = limit.try_into().unwrap_or(i64::MAX);
    let rows = stmt.query_map([limit_i64], |row| {
        let id: String = row.get("id")?;
        let project_id: String = row.get("project_id")?;
        let project_name: Option<String> = row.get("project_name")?;
        let worktree: Option<String> = row.get("worktree")?;
        let title: String = row.get("title")?;
        let directory: Option<String> = row.get("directory")?;
        let time_updated: i64 = row.get("time_updated")?;
        let msg_count: i64 = row.get("msg_count")?;
        let session_file = PathBuf::from(synthetic_session_path(db_path, &id));
        Ok(OpencodeSessionSummary {
            id,
            project_id,
            project_label: derive_project_label(project_name, worktree),
            title,
            directory: directory.filter(|s| !s.is_empty()),
            updated_at: millis_to_datetime(time_updated).unwrap_or_else(Utc::now),
            message_count: msg_count.max(0) as usize,
            session_file,
        })
    });
    match rows {
        Ok(iter) => iter.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

const SQL_LIST_RECENT: &str = r#"
    SELECT s.id            AS id,
           s.project_id    AS project_id,
           p.name          AS project_name,
           p.worktree      AS worktree,
           s.title         AS title,
           s.directory     AS directory,
           s.time_updated  AS time_updated,
           (SELECT COUNT(*) FROM message m WHERE m.session_id = s.id) AS msg_count
    FROM   session s
    LEFT JOIN project p ON p.id = s.project_id
    WHERE  s.directory IS NOT NULL AND s.directory != ''
    ORDER  BY s.time_updated DESC
    LIMIT  ?1
"#;

/// A single rendered message from an Opencode session.
#[derive(Debug, Clone)]
pub struct OpencodeMessage {
    pub id: String,
    pub session_id: String,
    pub role: MessageRole,
    pub parent_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub content_blocks: Vec<ContentBlock>,
}

/// Load every message + part for a given session, ordered chronologically.
pub fn load_messages(db_path: &Path, session_id: &str) -> Vec<OpencodeMessage> {
    let Ok(conn) = open_db(db_path) else {
        return Vec::new();
    };

    // Fetch messages first, then fetch parts in a single query and bucket
    // them by message_id. Doing this in two passes (instead of N+1 queries)
    // keeps tree loads fast even for sessions with hundreds of messages.
    let mut msg_stmt = match conn.prepare(
        "SELECT id, session_id, data, time_created \
         FROM message WHERE session_id = ?1 \
         ORDER BY time_created ASC, id ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let msg_rows: Vec<(String, String, i64, String)> = msg_stmt
        .query_map([session_id], |row| {
            Ok((
                row.get::<_, String>("id")?,
                row.get::<_, String>("session_id")?,
                row.get::<_, i64>("time_created")?,
                row.get::<_, String>("data")?,
            ))
        })
        .map(|iter| iter.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();

    if msg_rows.is_empty() {
        return Vec::new();
    }

    // Parts: fetch all rows for this session in one query, sorted by
    // (message_id, time_created, id) so blocks within a message stay in
    // creation order.
    let mut part_stmt = match conn.prepare(
        "SELECT message_id, data \
         FROM part WHERE session_id = ?1 \
         ORDER BY message_id ASC, time_created ASC, id ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let part_rows: Vec<(String, String)> = part_stmt
        .query_map([session_id], |row| {
            Ok((
                row.get::<_, String>("message_id")?,
                row.get::<_, String>("data")?,
            ))
        })
        .map(|iter| iter.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();

    let mut parts_by_msg: std::collections::HashMap<String, Vec<ContentBlock>> =
        std::collections::HashMap::new();
    for (msg_id, data) in part_rows {
        let Ok(json) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        push_blocks_from_part(&json, parts_by_msg.entry(msg_id).or_default());
    }

    let mut out = Vec::with_capacity(msg_rows.len());
    for (id, session_id, time_created, data) in msg_rows {
        let Ok(meta) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let role = match meta.get("role").and_then(|v| v.as_str()) {
            Some("user") => MessageRole::User,
            Some("assistant") => MessageRole::Assistant,
            _ => continue,
        };
        let parent_id = meta
            .get("parentID")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let created_at = meta
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(|v| v.as_i64())
            .and_then(millis_to_datetime)
            .unwrap_or_else(|| millis_to_datetime(time_created).unwrap_or_else(Utc::now));
        let content_blocks = parts_by_msg.remove(&id).unwrap_or_default();
        out.push(OpencodeMessage {
            id,
            session_id,
            role,
            parent_id,
            created_at,
            content_blocks,
        });
    }
    out
}

fn push_blocks_from_part(json: &Value, blocks: &mut Vec<ContentBlock>) {
    let Some(part_type) = json.get("type").and_then(|v| v.as_str()) else {
        return;
    };
    match part_type {
        "text" => {
            if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                if !text.trim().is_empty() {
                    blocks.push(ContentBlock::Text(text.to_string()));
                }
            }
        }
        "reasoning" => {
            if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                if !text.trim().is_empty() {
                    blocks.push(ContentBlock::Thinking(text.to_string()));
                }
            }
        }
        "tool" => push_tool_blocks(json, blocks),
        // step-start / step-finish / patch / snapshot are structural,
        // not user-visible content.
        _ => {}
    }
}

fn push_tool_blocks(json: &Value, blocks: &mut Vec<ContentBlock>) {
    let name = json
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("tool")
        .to_string();
    let state = json.get("state");
    let input = state
        .and_then(|s| s.get("input"))
        .map(|v| match v.as_str() {
            Some(s) => s.to_string(),
            None => serde_json::to_string(v).unwrap_or_default(),
        })
        .unwrap_or_default();
    blocks.push(ContentBlock::ToolUse { name, input });

    if let Some(output) = state.and_then(|s| s.get("output")) {
        let text = match output.as_str() {
            Some(s) => s.to_string(),
            None => serde_json::to_string(output).unwrap_or_default(),
        };
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            blocks.push(ContentBlock::ToolResult(text));
        }
    }
}

/// A record paired with its position and timestamp, in the shape expected by
/// `SessionDag::from_records`.
pub type MaterializedRecord = (SessionRecord, usize, Option<DateTime<Utc>>);

/// Materialize an Opencode session as an ordered list of `SessionRecord`s
/// compatible with the existing DAG engine and tree renderer.
///
/// Synthetic chain: Opencode does not branch the way Claude Code does — each
/// assistant turn references the same prior user message via `parentID`. To
/// produce a linear chain that the existing DAG flattener can render as a
/// tree, we link every message to its immediate predecessor in `time.created`
/// order rather than using the recorded `parentID`.
pub fn materialize_session(session: &OpencodeSession) -> Option<(String, Vec<MaterializedRecord>)> {
    let (db, _) = parse_session_path(session.session_file.to_str()?)?;
    let messages = load_messages(&db, &session.id);
    let mut records = Vec::with_capacity(messages.len());
    let mut prev_uuid: Option<String> = None;
    for (idx, msg) in messages.into_iter().enumerate() {
        let parent_uuid = prev_uuid.take();
        let record = SessionRecord::Message {
            role: msg.role,
            content_blocks: msg.content_blocks,
            uuid: Some(msg.id.clone()),
            parent_uuid,
            is_sidechain: false,
        };
        records.push((record, idx, Some(msg.created_at)));
        prev_uuid = Some(msg.id);
    }
    Some((session.id.clone(), records))
}

/// What kind of search the caller wants to run.
///
/// Fixed-string queries use a SQL `LIKE` prefilter so SQLite can narrow the
/// candidate set with its `data` index — orders of magnitude faster than a
/// table scan. Regex queries can't be expressed as a single `LIKE`, so we
/// fall back to a full part scan and let the Rust regex engine do the
/// matching. SQLite has no portable regex operator across builds, and the
/// bundled SQLite we link doesn't load the optional `regex` extension.
#[derive(Debug, Clone, Copy)]
pub enum SearchMode<'a> {
    /// Case-insensitive substring match.
    Fixed,
    /// Full part scan; the caller supplies the compiled regex so we don't
    /// recompile it per row.
    Regex(&'a regex::Regex),
}

/// A single streaming match row from the part-search JOIN. Carries every
/// field [`search_parts_streaming`] callers need to build a `RipgrepMatch`
/// without any further DB round-trips.
#[derive(Debug, Clone)]
pub struct OpencodeMatchRow {
    pub session_id: String,
    pub message_id: String,
    pub role: MessageRole,
    pub timestamp: DateTime<Utc>,
    pub text: String,
    pub session_file: PathBuf,
}

/// How often to poll the cancel token while iterating SQL rows. Checked once
/// per `CANCEL_POLL_INTERVAL` rows — frequent enough to abort within a few
/// hundred microseconds, rare enough that the atomic load is amortised.
const CANCEL_POLL_INTERVAL: usize = 64;

/// Stream matching `part` rows via a single JOIN against `message`/`session`.
///
/// One row per matching part is yielded to `on_match`, deduped so the
/// callback only sees the first hit per `(session_id, message_id)` pair —
/// matches the per-session/per-message granularity the rest of ccs expects.
/// Structural part types (`step-start`, `snapshot`, …) are filtered via
/// [`render_part_text`] before the callback fires.
///
/// Cancellation: the supplied flag is polled every `CANCEL_POLL_INTERVAL`
/// rows so mid-stream cancels are honoured without per-row atomic traffic.
/// The callback returns `ControlFlow::Break(())` to stop iteration early —
/// e.g. when the caller has hit its own result cap.
///
/// Errors:
/// * `Err("cancelled")` if the cancel token flips during iteration.
/// * `Err(...)` for SQL/connection failures.
pub fn search_parts_streaming<F>(
    db_path: &Path,
    query: &str,
    mode: SearchMode<'_>,
    cancel: &Arc<AtomicBool>,
    mut on_match: F,
) -> Result<(), String>
where
    F: FnMut(OpencodeMatchRow) -> ControlFlow<()>,
{
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    let conn =
        open_db(db_path).map_err(|e| format!("opencode db open failed ({db_path:?}): {e}"))?;

    // Lowercase the query only when the Fixed-mode post-filter needs it;
    // Regex mode matches case-sensitively against the original text.
    let lower_query = match mode {
        SearchMode::Fixed => Some(query.to_lowercase()),
        SearchMode::Regex(_) => None,
    };
    let (sql, params): (&str, Vec<String>) = match mode {
        SearchMode::Fixed => (
            SQL_PART_SEARCH_FIXED,
            vec![format!(
                "%{}%",
                escape_like(lower_query.as_deref().unwrap_or(""))
            )],
        ),
        SearchMode::Regex(_) => (SQL_PART_SEARCH_REGEX, vec![]),
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("opencode search prepare failed: {e}"))?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(params.iter()))
        .map_err(|e| format!("opencode search query failed: {e}"))?;

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut row_count: usize = 0;

    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(e) => return Err(format!("opencode search row failed: {e}")),
        };

        row_count += 1;
        if row_count.is_multiple_of(CANCEL_POLL_INTERVAL) && cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }

        let session_id: String = row.get(0).map_err(|e| format!("row get session_id: {e}"))?;
        let message_id: String = row.get(1).map_err(|e| format!("row get message_id: {e}"))?;
        let part_data: String = row.get(2).map_err(|e| format!("row get part_data: {e}"))?;
        let message_data: String = row
            .get(3)
            .map_err(|e| format!("row get message_data: {e}"))?;
        let message_time: i64 = row
            .get(4)
            .map_err(|e| format!("row get message_time: {e}"))?;

        let Ok(part_json) = serde_json::from_str::<Value>(&part_data) else {
            continue;
        };
        let Some(text) = render_part_text(&part_json) else {
            continue;
        };
        let matched = match mode {
            SearchMode::Fixed => text
                .to_lowercase()
                .contains(lower_query.as_deref().unwrap_or("")),
            SearchMode::Regex(re) => re.is_match(&text),
        };
        if !matched {
            continue;
        }

        // Collapse multiple parts of the same message into one hit to mirror
        // ripgrep's behaviour (one match per session/message pair surfaces in
        // the UI; users expand the session to see all hits).
        if !seen.insert(format!("{}:{}", session_id, message_id)) {
            continue;
        }

        let Ok(msg_json) = serde_json::from_str::<Value>(&message_data) else {
            continue;
        };
        let role = match msg_json.get("role").and_then(|v| v.as_str()) {
            Some("user") => MessageRole::User,
            Some("assistant") => MessageRole::Assistant,
            _ => continue,
        };
        let timestamp = msg_json
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(|v| v.as_i64())
            .and_then(millis_to_datetime)
            .unwrap_or_else(|| millis_to_datetime(message_time).unwrap_or_else(Utc::now));

        let session_file = PathBuf::from(synthetic_session_path(db_path, &session_id));

        let row_out = OpencodeMatchRow {
            session_id,
            message_id,
            role,
            timestamp,
            text,
            session_file,
        };

        if let ControlFlow::Break(()) = on_match(row_out) {
            return Ok(());
        }
    }

    Ok(())
}

// Both queries inner-join `session` so orphaned `message`/`part` rows whose
// `session_id` has no corresponding `session` row are filtered out at the SQL
// layer. The previous code path looked up `OpencodeSession::from_db` per hit
// and `continue`d on None; the streaming path no longer does that lookup, so
// without this join we'd emit `<db>#<missing_session>` results that fail to
// open in tree mode. The `m.session_id = p.session_id` predicate also guards
// against the rare case where a part's `session_id` disagrees with its
// message's `session_id`.
const SQL_PART_SEARCH_FIXED: &str = r#"
    SELECT p.session_id   AS session_id,
           p.message_id   AS message_id,
           p.data         AS part_data,
           m.data         AS message_data,
           m.time_created AS message_time
    FROM   part p
    JOIN   message m ON m.id = p.message_id AND m.session_id = p.session_id
    JOIN   session s ON s.id = p.session_id
    WHERE  LOWER(p.data) LIKE ?1 ESCAPE '\'
    ORDER  BY p.time_created DESC
"#;

const SQL_PART_SEARCH_REGEX: &str = r#"
    SELECT p.session_id   AS session_id,
           p.message_id   AS message_id,
           p.data         AS part_data,
           m.data         AS message_data,
           m.time_created AS message_time
    FROM   part p
    JOIN   message m ON m.id = p.message_id AND m.session_id = p.session_id
    JOIN   session s ON s.id = p.session_id
    ORDER  BY p.time_created DESC
"#;

/// Render the user-visible text of a part for the search post-filter.
/// Returns `None` for structural parts (`step-start`, `step-finish`,
/// `snapshot`, `patch`) so they're skipped as match candidates.
pub fn render_part_text(json: &Value) -> Option<String> {
    let part_type = json.get("type").and_then(|v| v.as_str())?;
    match part_type {
        "text" | "reasoning" => json
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "tool" => {
            let state = json.get("state")?;
            let mut out = String::new();
            if let Some(input) = state.get("input") {
                let rendered = match input.as_str() {
                    Some(s) => s.to_string(),
                    None => serde_json::to_string(input).unwrap_or_default(),
                };
                out.push_str(&rendered);
            }
            if let Some(output) = state.get("output") {
                let rendered = match output.as_str() {
                    Some(s) => s.to_string(),
                    None => serde_json::to_string(output).unwrap_or_default(),
                };
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&rendered);
            }
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

fn millis_to_datetime(ms: i64) -> Option<DateTime<Utc>> {
    if ms < 0 {
        return None;
    }
    let secs = ms / 1000;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    Utc.timestamp_opt(secs, nanos).single()
}

fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    /// Build a minimal Opencode SQLite database on disk and return
    /// `(tempdir, db_path)`.
    pub(crate) fn build_db_fixture() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (
                id TEXT PRIMARY KEY,
                worktree TEXT NOT NULL,
                vcs TEXT,
                name TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL
            );
            CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                slug TEXT NOT NULL DEFAULT '',
                directory TEXT,
                title TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL
            );
            CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            "#,
        )
        .unwrap();

        // One project with two sessions; one of those is a no-directory
        // "global" session that list_sessions_for_recent must skip.
        conn.execute(
            "INSERT INTO project VALUES ('projAAA', '/tmp/myproj', 'git', 'myproj', 1, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session VALUES ('ses_TEST', 'projAAA', 'curious-star', '/tmp/myproj', \
             'Test conversation', 1769762431585, 1769762509666)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session VALUES ('ses_GLOBAL', 'projAAA', 'g', '', 'global noise', 1, 2)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO message VALUES ('msg_user1', 'ses_TEST', 1769762431591, 1769762431591, ?1)",
            [r#"{"role":"user","time":{"created":1769762431591}}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message VALUES ('msg_asst1', 'ses_TEST', 1769762431596, 1769762431596, ?1)",
            [r#"{"role":"assistant","parentID":"msg_user1","time":{"created":1769762431596}}"#],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO part VALUES ('prt_001', 'msg_user1', 'ses_TEST', 1, 1, ?1)",
            [r#"{"type":"text","text":"Hello opencode"}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('prt_002', 'msg_asst1', 'ses_TEST', 2, 2, ?1)",
            [r#"{"type":"step-start","snapshot":"abc"}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('prt_003', 'msg_asst1', 'ses_TEST', 3, 3, ?1)",
            [r#"{"type":"text","text":"Sure thing"}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('prt_004', 'msg_asst1', 'ses_TEST', 4, 4, ?1)",
            [r#"{"type":"tool","tool":"grep","state":{"input":{"pattern":"foo"},"output":"matched 3 lines"}}"#],
        )
        .unwrap();

        (dir, db_path)
    }

    #[test]
    fn test_is_opencode_session_path_detects_db() {
        assert!(is_opencode_session_path(
            "/home/u/.local/share/opencode/opencode.db#ses_x"
        ));
        assert!(is_opencode_session_path(
            "/home/u/.local/share/opencode/opencode.db"
        ));
        assert!(!is_opencode_session_path(
            "/home/u/.claude/projects/foo/abc.jsonl"
        ));
    }

    #[test]
    fn test_parse_session_path_round_trip() {
        let db = PathBuf::from("/x/opencode.db");
        let synth = synthetic_session_path(&db, "ses_42");
        let (parsed_db, sid) = parse_session_path(&synth).unwrap();
        assert_eq!(parsed_db, db);
        assert_eq!(sid, "ses_42");
        assert!(parse_session_path("/not/even/a/db.jsonl#abc").is_none());
    }

    #[test]
    fn test_storage_root_honours_xdg_data_home() {
        let _lock = crate::TEST_ENV_MUTEX.lock().unwrap();
        let _env_guard = crate::EnvGuard::new(&["OPENCODE_DATA", "XDG_DATA_HOME"]);

        // SAFETY: tests run single-threaded behind TEST_ENV_MUTEX.
        unsafe { std::env::remove_var("OPENCODE_DATA") };

        let tmp = TempDir::new().unwrap();
        let opencode_dir = tmp.path().join("opencode");
        std::fs::create_dir_all(&opencode_dir).unwrap();
        std::fs::write(opencode_dir.join("opencode.db"), b"").unwrap();

        unsafe { std::env::set_var("XDG_DATA_HOME", tmp.path()) };
        let resolved = opencode_storage_root();
        assert_eq!(
            resolved.as_deref(),
            Some(opencode_dir.as_path()),
            "XDG_DATA_HOME should resolve to <xdg>/opencode when that directory exists"
        );
    }

    #[test]
    fn test_storage_root_opencode_data_wins_over_xdg() {
        let _lock = crate::TEST_ENV_MUTEX.lock().unwrap();
        let _env_guard = crate::EnvGuard::new(&["OPENCODE_DATA", "XDG_DATA_HOME"]);

        let xdg_tmp = TempDir::new().unwrap();
        let xdg_opencode = xdg_tmp.path().join("opencode");
        std::fs::create_dir_all(&xdg_opencode).unwrap();
        std::fs::write(xdg_opencode.join("opencode.db"), b"").unwrap();

        let oc_tmp = TempDir::new().unwrap();
        std::fs::write(oc_tmp.path().join("opencode.db"), b"").unwrap();

        // SAFETY: tests run single-threaded behind TEST_ENV_MUTEX.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", xdg_tmp.path());
            std::env::set_var("OPENCODE_DATA", oc_tmp.path());
        }

        let resolved = opencode_storage_root();
        assert_eq!(
            resolved.as_deref(),
            Some(oc_tmp.path()),
            "OPENCODE_DATA must take precedence over XDG_DATA_HOME"
        );
    }

    #[test]
    fn test_list_sessions_for_recent_skips_empty_directory() {
        let (_dir, db) = build_db_fixture();
        let sessions = list_sessions_for_recent(&db, 10);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "ses_TEST");
        assert_eq!(sessions[0].title, "Test conversation");
        assert_eq!(sessions[0].directory.as_deref(), Some("/tmp/myproj"));
    }

    #[test]
    fn test_session_from_file_uses_synthetic_path() {
        let (_dir, db) = build_db_fixture();
        let synth = synthetic_session_path(&db, "ses_TEST");
        let session = OpencodeSession::from_file(Path::new(&synth)).unwrap();
        assert_eq!(session.id, "ses_TEST");
        assert_eq!(session.project_id, "projAAA");
        assert_eq!(session.title, "Test conversation");
    }

    #[test]
    fn test_read_project_label_prefers_name() {
        let (_dir, db) = build_db_fixture();
        // name was inserted as 'myproj' so it should win over the worktree basename
        assert_eq!(
            read_project_label(&db, "projAAA").as_deref(),
            Some("myproj")
        );
    }

    #[test]
    fn test_load_messages_orders_by_time_and_renders_parts() {
        let (_dir, db) = build_db_fixture();
        let messages = load_messages(&db, "ses_TEST");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, "msg_user1");
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(
            messages[0].content_blocks,
            vec![ContentBlock::Text("Hello opencode".to_string())]
        );
        assert_eq!(messages[1].id, "msg_asst1");
        assert!(matches!(
            messages[1].content_blocks.first(),
            Some(ContentBlock::Text(_))
        ));
        // tool + tool_result emitted from a single tool part
        assert!(messages[1]
            .content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name == "grep")));
        assert!(messages[1]
            .content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult(_))));
    }

    #[test]
    fn test_materialize_session_builds_linear_chain() {
        let (_dir, db) = build_db_fixture();
        let session =
            OpencodeSession::from_file(Path::new(&synthetic_session_path(&db, "ses_TEST")))
                .unwrap();
        let (sid, records) = materialize_session(&session).unwrap();
        assert_eq!(sid, "ses_TEST");
        assert_eq!(records.len(), 2);

        match &records[1].0 {
            SessionRecord::Message {
                uuid, parent_uuid, ..
            } => {
                assert_eq!(uuid.as_deref(), Some("msg_asst1"));
                assert_eq!(
                    parent_uuid.as_deref(),
                    Some("msg_user1"),
                    "second message must chain to the first"
                );
            }
            other => panic!("expected Message, got {:?}", other),
        }
    }

    /// Drain `search_parts_streaming` into a `Vec` for test assertions.
    fn collect_search(db: &Path, query: &str, mode: SearchMode<'_>) -> Vec<OpencodeMatchRow> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut out = Vec::new();
        search_parts_streaming(db, query, mode, &cancel, |row| {
            out.push(row);
            ControlFlow::Continue(())
        })
        .expect("search_parts_streaming failed");
        out
    }

    #[test]
    fn test_search_parts_returns_text_hits() {
        let (_dir, db) = build_db_fixture();
        let hits = collect_search(&db, "Hello opencode", SearchMode::Fixed);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "ses_TEST");
        assert_eq!(hits[0].message_id, "msg_user1");
        assert!(hits[0].text.contains("Hello opencode"));
        assert_eq!(hits[0].role, MessageRole::User);
        // Synthetic path is built from the DB path + session id.
        assert!(hits[0]
            .session_file
            .to_string_lossy()
            .ends_with("#ses_TEST"));
    }

    #[test]
    fn test_search_parts_finds_tool_output() {
        let (_dir, db) = build_db_fixture();
        let hits = collect_search(&db, "matched 3 lines", SearchMode::Fixed);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, "msg_asst1");
        assert_eq!(hits[0].role, MessageRole::Assistant);
    }

    #[test]
    fn test_search_parts_ignores_structural_match() {
        // The step-start part has snapshot=abc; a query for "abc" hits the
        // JSON envelope via LIKE but render_part_text returns None for
        // step-start, so the post-filter drops it.
        let (_dir, db) = build_db_fixture();
        let hits = collect_search(&db, "abc", SearchMode::Fixed);
        assert!(hits.is_empty());
    }

    #[test]
    fn test_search_parts_case_insensitive() {
        let (_dir, db) = build_db_fixture();
        let hits = collect_search(&db, "HELLO OPENCODE", SearchMode::Fixed);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn test_search_parts_regex_matches_whitespace_pattern() {
        // The regression the LIKE-only prefilter caused: `hello\s+opencode`
        // would be turned into a literal LIKE pattern and never match the
        // text `Hello opencode` (which has a real space, not `\s+`).
        let (_dir, db) = build_db_fixture();
        let re = regex::RegexBuilder::new(r"hello\s+opencode")
            .case_insensitive(true)
            .build()
            .unwrap();
        let hits = collect_search(&db, r"hello\s+opencode", SearchMode::Regex(&re));
        assert_eq!(hits.len(), 1, "regex must match across the whitespace");
        assert_eq!(hits[0].session_id, "ses_TEST");
    }

    #[test]
    fn test_search_parts_regex_does_not_short_circuit_on_envelope() {
        // The LIKE prefilter happens to match JSON keys like `"text"` for the
        // query `text`, but a regex search should rely on render_part_text
        // (user-visible content) — not on envelope substrings. Pattern
        // `^Hello` should match only the user message, not the assistant's
        // `Sure thing` or the tool output.
        let (_dir, db) = build_db_fixture();
        let re = regex::Regex::new("^Hello").unwrap();
        let hits = collect_search(&db, "^Hello", SearchMode::Regex(&re));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_id, "msg_user1");
    }

    #[test]
    fn test_search_parts_streaming_honours_break() {
        // Build a DB with multiple matching messages and assert the callback
        // can stop iteration early via `ControlFlow::Break`.
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT,
                name TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                slug TEXT NOT NULL DEFAULT '', directory TEXT, title TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL);
            INSERT INTO project VALUES ('p', '/tmp/p', 'git', 'p', 1, 2);
            INSERT INTO session VALUES ('s', 'p', '', '/tmp/p', 't', 1, 2);
            "#,
        )
        .unwrap();
        for i in 0..5 {
            let mid = format!("m{i}");
            conn.execute(
                "INSERT INTO message VALUES (?1, 's', ?2, ?2, ?3)",
                rusqlite::params![mid, 100 + i, r#"{"role":"user","time":{"created":100}}"#],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part VALUES (?1, ?2, 's', ?3, ?3, ?4)",
                rusqlite::params![
                    format!("p{i}"),
                    mid,
                    100 + i,
                    r#"{"type":"text","text":"findme"}"#
                ],
            )
            .unwrap();
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let mut seen = 0usize;
        search_parts_streaming(&db_path, "findme", SearchMode::Fixed, &cancel, |_row| {
            seen += 1;
            if seen >= 2 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .expect("streaming should succeed");
        assert_eq!(seen, 2, "callback must stop after Break");
    }

    #[test]
    fn test_list_sessions_for_recent_msg_count_matches_load_messages() {
        let (_dir, db) = build_db_fixture();
        let summaries = list_sessions_for_recent(&db, 10);
        assert_eq!(
            summaries.len(),
            1,
            "global-directory session must be skipped"
        );
        let summary = &summaries[0];
        assert_eq!(summary.id, "ses_TEST");
        assert_eq!(summary.project_label.as_deref(), Some("myproj"));
        assert_eq!(summary.directory.as_deref(), Some("/tmp/myproj"));

        let loaded = load_messages(&db, "ses_TEST");
        assert_eq!(
            summary.message_count,
            loaded.len(),
            "msg_count from list_sessions_for_recent must match load_messages.len()"
        );
    }

    #[test]
    fn test_list_sessions_for_recent_honours_limit() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT,
                name TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                slug TEXT NOT NULL DEFAULT '', directory TEXT, title TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL);
            INSERT INTO project VALUES ('p', '/tmp/p', 'git', 'p', 1, 2);
            "#,
        )
        .unwrap();
        for i in 0..5 {
            conn.execute(
                "INSERT INTO session VALUES (?1, 'p', '', '/tmp/p', ?2, ?3, ?3)",
                rusqlite::params![format!("ses_{i}"), format!("title-{i}"), 1_000 + i as i64],
            )
            .unwrap();
        }

        let summaries = list_sessions_for_recent(&db_path, 2);
        assert_eq!(summaries.len(), 2, "LIMIT must cap the returned rows");
        // ORDER BY time_updated DESC — the newest two are ses_4 and ses_3.
        assert_eq!(summaries[0].id, "ses_4");
        assert_eq!(summaries[1].id, "ses_3");
    }

    #[test]
    fn test_search_parts_streaming_skips_orphaned_session_rows() {
        // Build a DB with two messages whose `session_id` references a
        // non-existent session row, plus one message tied to a real session.
        // Only the real-session hit must surface — the orphans would later
        // fail to open in tree mode (OpencodeSession::from_file would return
        // None) and produce a "Failed to load Opencode session" error.
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT,
                name TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                slug TEXT NOT NULL DEFAULT '', directory TEXT, title TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL);
            INSERT INTO project VALUES ('p', '/tmp/p', 'git', 'p', 1, 2);
            INSERT INTO session VALUES ('ses_REAL', 'p', '', '/tmp/p', 't', 1, 2);
            -- Orphan: references ses_GHOST which has no row in `session`.
            INSERT INTO message VALUES ('m_orphan', 'ses_GHOST', 100, 100,
                '{"role":"user","time":{"created":100}}');
            INSERT INTO part VALUES ('p_orphan', 'm_orphan', 'ses_GHOST', 100, 100,
                '{"type":"text","text":"findme"}');
            -- Real hit: session row exists, so it must surface.
            INSERT INTO message VALUES ('m_real', 'ses_REAL', 200, 200,
                '{"role":"user","time":{"created":200}}');
            INSERT INTO part VALUES ('p_real', 'm_real', 'ses_REAL', 200, 200,
                '{"type":"text","text":"findme"}');
            "#,
        )
        .unwrap();

        let hits = collect_search(&db_path, "findme", SearchMode::Fixed);
        assert_eq!(
            hits.len(),
            1,
            "orphaned session_id must be filtered out by the session JOIN"
        );
        assert_eq!(hits[0].session_id, "ses_REAL");
    }

    #[test]
    fn test_search_parts_streaming_respects_cancel() {
        let (_dir, db) = build_db_fixture();
        let cancel = Arc::new(AtomicBool::new(true));
        let err = search_parts_streaming(&db, "Hello", SearchMode::Fixed, &cancel, |_row| {
            ControlFlow::Continue(())
        });
        assert_eq!(err.err().as_deref(), Some("cancelled"));
    }
}
