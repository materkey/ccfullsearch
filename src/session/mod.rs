pub mod record;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Codex stores rollout transcripts under one of these subdirectories of `CODEX_HOME`.
pub(crate) const CODEX_SESSION_SUBDIRS: [&str; 2] = ["sessions", "archived_sessions"];

/// Source of the Claude session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSource {
    /// Claude Code CLI sessions stored in ~/.claude/projects/
    ClaudeCodeCLI,
    /// Claude Desktop app sessions stored in ~/Library/Application Support/Claude/
    ClaudeDesktop,
}

impl SessionSource {
    /// Detect session source from file path
    pub fn from_path(path: &str) -> Self {
        if path.contains("local-agent-mode-sessions") {
            SessionSource::ClaudeDesktop
        } else {
            SessionSource::ClaudeCodeCLI
        }
    }

    /// Returns display name for the source
    pub fn display_name(&self) -> &'static str {
        match self {
            SessionSource::ClaudeCodeCLI => "CLI",
            SessionSource::ClaudeDesktop => "Desktop",
        }
    }
}

/// Product/tool that owns a session transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionProvider {
    /// Claude Code or Claude Desktop session.
    Claude,
    /// Codex CLI session.
    Codex,
}

impl SessionProvider {
    /// Detect the session provider from the transcript path.
    pub fn from_path(path: &str) -> Self {
        let normalized = normalize_session_path(path);
        let is_codex = normalized.contains("/.codex/sessions/")
            || normalized.contains("/.codex/archived_sessions/")
            || std::env::var("CODEX_HOME")
                .ok()
                .as_deref()
                .map(|codex_home| path_is_under_codex_home(&normalized, codex_home))
                .unwrap_or(false);
        if is_codex {
            SessionProvider::Codex
        } else {
            SessionProvider::Claude
        }
    }

    /// Returns display name for the provider.
    pub fn display_name(self) -> &'static str {
        match self {
            SessionProvider::Claude => "Claude",
            SessionProvider::Codex => "Codex",
        }
    }
}

fn normalize_session_path(path: &str) -> Cow<'_, str> {
    if path.contains('\\') {
        Cow::Owned(path.replace('\\', "/"))
    } else {
        Cow::Borrowed(path)
    }
}

fn path_is_under_codex_home(path: &str, codex_home: &str) -> bool {
    let normalized_home = normalize_session_path(codex_home);
    let home = normalized_home.trim_end_matches('/');
    // Reject empty CODEX_HOME (or `"/"`): otherwise any path starting with `/sessions/` would match.
    if home.is_empty() {
        return false;
    }
    let Some(rest) = path.strip_prefix(home).and_then(|r| r.strip_prefix('/')) else {
        return false;
    };
    CODEX_SESSION_SUBDIRS.iter().any(|subdir| {
        rest.strip_prefix(subdir)
            .and_then(|r| r.strip_prefix('/'))
            .is_some()
    })
}

/// Return true when a transcript path belongs to Codex rollout storage.
pub fn is_codex_session_path(path: &str) -> bool {
    SessionProvider::from_path(path) == SessionProvider::Codex
}

/// Extract a Codex thread/session id from a rollout filename.
///
/// Codex response-item lines do not repeat the session id; it is stored in the
/// initial `session_meta` record and in filenames shaped as
/// `rollout-YYYY-MM-DDTHH-MM-SS-<thread-id>.jsonl`.
pub fn extract_codex_session_id_from_path(path: &str) -> Option<String> {
    let file_stem = Path::new(path).file_stem()?.to_str()?;
    let rest = file_stem.strip_prefix("rollout-")?;
    if rest.len() <= 20 || rest.as_bytes().get(19) != Some(&b'-') {
        return None;
    }
    let id = &rest[20..];
    (!id.is_empty()).then(|| id.to_string())
}

/// Extract session ID from a JSON record.
/// Supports Claude CLI (`sessionId`), Claude Desktop (`session_id`), and Codex
/// rollout `session_meta` (`payload.id`) records.
pub fn extract_session_id(json: &serde_json::Value) -> Option<String> {
    json.get("sessionId")
        .or_else(|| json.get("session_id"))
        .or_else(|| {
            (json.get("type").and_then(|v| v.as_str()) == Some("session_meta"))
                .then(|| json.get("payload")?.get("id"))
                .flatten()
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract the parent thread id from a Codex subagent `session_meta` record.
///
/// Codex stores spawned subagents as normal rollout files. The only reliable
/// parent link is in `payload.source.subagent.thread_spawn.parent_thread_id`.
pub fn extract_codex_parent_thread_id(json: &serde_json::Value) -> Option<String> {
    (json.get("type").and_then(|v| v.as_str()) == Some("session_meta"))
        .then(|| {
            json.get("payload")?
                .get("source")?
                .get("subagent")?
                .get("thread_spawn")?
                .get("parent_thread_id")
        })
        .flatten()
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Read a Codex rollout's first few records and return the parent thread id
/// when the file represents a spawned subagent.
pub fn codex_parent_thread_id_from_file(path: &Path) -> Option<String> {
    let path_str = path.to_str()?;
    if !is_codex_session_path(path_str) {
        return None;
    }

    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().take(10).flatten() {
        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(parent_id) = extract_codex_parent_thread_id(&json) {
            return Some(parent_id);
        }
        if extract_record_type(&json) == Some("session_meta") {
            break;
        }
    }

    None
}

fn codex_rollout_matches_session_id(path: &Path, session_id: &str) -> bool {
    path.to_str()
        .and_then(extract_codex_session_id_from_path)
        .as_deref()
        == Some(session_id)
}

fn codex_session_root(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        let name = ancestor.file_name()?.to_str()?;
        matches!(name, "sessions" | "archived_sessions").then(|| ancestor.to_path_buf())
    })
}

fn find_codex_rollout_in_tree(root: &Path, session_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if let Some(found) = find_codex_rollout_in_tree(&path, session_id) {
                return Some(found);
            }
        } else if file_type.is_file() && codex_rollout_matches_session_id(&path, session_id) {
            return Some(path);
        }
    }
    None
}

fn find_codex_rollout_near(path: &Path, session_id: &str) -> Option<PathBuf> {
    if let Some(parent) = path.parent() {
        if let Some(found) = find_codex_rollout_in_tree(parent, session_id) {
            return Some(found);
        }
    }

    let root = codex_session_root(path)?;
    if let Some(found) = find_codex_rollout_in_tree(&root, session_id) {
        return Some(found);
    }

    let sibling_root = root.parent().and_then(|codex_home| {
        let root_name = root.file_name()?.to_str()?;
        match root_name {
            "sessions" => Some(codex_home.join("archived_sessions")),
            "archived_sessions" => Some(codex_home.join("sessions")),
            _ => None,
        }
    });
    sibling_root
        .filter(|p| p.is_dir())
        .and_then(|p| find_codex_rollout_in_tree(&p, session_id))
}

/// Resolve a Codex spawned-subagent rollout to its parent rollout.
pub fn resolve_codex_subagent_session(path: &Path) -> Option<(String, String)> {
    let parent_id = codex_parent_thread_id_from_file(path)?;
    let parent_path =
        find_codex_rollout_near(path, &parent_id).unwrap_or_else(|| path.to_path_buf());
    Some((parent_id, parent_path.to_string_lossy().to_string()))
}

/// Extract timestamp from a JSON record.
/// Supports Claude CLI/Codex rollout (`timestamp`), Claude Desktop
/// (`_audit_timestamp`), and Codex `session_meta.payload.timestamp`.
pub fn extract_timestamp(json: &serde_json::Value) -> Option<DateTime<Utc>> {
    json.get("timestamp")
        .or_else(|| json.get("_audit_timestamp"))
        .or_else(|| json.get("payload").and_then(|p| p.get("timestamp")))
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Extract uuid from a JSON record.
pub fn extract_uuid(json: &serde_json::Value) -> Option<String> {
    json.get("uuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract parentUuid from a JSON record.
pub fn extract_parent_uuid(json: &serde_json::Value) -> Option<String> {
    json.get("parentUuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract parentUuid, falling back to logicalParentUuid.
/// At compact_boundary points, parentUuid is null but logicalParentUuid preserves
/// the logical link to the pre-boundary chain.
pub fn extract_parent_uuid_or_logical(json: &serde_json::Value) -> Option<String> {
    extract_parent_uuid(json).or_else(|| {
        json.get("logicalParentUuid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Extract leafUuid from a JSON record.
pub fn extract_leaf_uuid(json: &serde_json::Value) -> Option<String> {
    json.get("leafUuid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract record type from a JSON record.
pub fn extract_record_type(json: &serde_json::Value) -> Option<&str> {
    json.get("type").and_then(|v| v.as_str())
}

/// Extract the git branch from a JSON record. CLI sessions use `branch`; some
/// older recordings also use `gitBranch`. Desktop records carry neither.
pub fn extract_branch(json: &serde_json::Value) -> Option<String> {
    json.get("branch")
        .or_else(|| json.get("gitBranch"))
        .or_else(|| {
            json.get("payload")
                .and_then(|p| p.get("git")?.get("branch"))
        })
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Extract the working directory from formats that persist it as metadata.
pub fn extract_cwd(json: &serde_json::Value) -> Option<String> {
    json.get("cwd")
        .or_else(|| json.get("payload").and_then(|p| p.get("cwd")))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Read a Codex rollout's first records and return the recorded `cwd`.
///
/// Codex stores it in the initial `session_meta` record; scanning the prefix
/// is enough and avoids loading large rollouts.
pub fn read_codex_session_cwd(path: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().take(50).flatten() {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(cwd) = extract_cwd(&json) {
            return Some(cwd);
        }
    }

    None
}

/// Returns true if the JSON record has `isSidechain: true`.
/// Sidechain messages are subagent messages that should not participate in the main chain.
pub fn is_sidechain(json: &serde_json::Value) -> bool {
    json.get("isSidechain")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

const RALPHEX_MARKER: &str = "<<<RALPHEX:";
const SCHEDULED_TASK_MARKER: &str = "<scheduled-task";
const CLAUDE_MEM_OBSERVER_PATH_SEGMENT: &str = "-claude-mem-observer-sessions";
const CLAUDE_MEM_OBSERVER_PATH_RAW: &str = "/.claude-mem/observer-sessions/";
const CLAUDE_MEM_CONTENT_MARKER: &str = "<observed_from_primary_session>";
const CLAUDE_MEM_TAG: &str = "claude-mem";
pub(crate) const RECOG_AUTOMATION_MARKER_PREFIX: &str = "<!-- ccs-automation:recog";
const RECOG_TAG: &str = "recog";

fn matches_scheduled_task_marker(content: &str) -> bool {
    content.trim_start().starts_with(SCHEDULED_TASK_MARKER)
}

fn matches_ralphex_marker(content: &str) -> bool {
    content.contains(RALPHEX_MARKER)
}

fn matches_claude_mem_content_marker(content: &str) -> bool {
    content.trim_start().starts_with(CLAUDE_MEM_CONTENT_MARKER)
}

fn matches_recog_automation_marker(content: &str) -> bool {
    content
        .trim_start()
        .starts_with(RECOG_AUTOMATION_MARKER_PREFIX)
}

/// Recursively collect session JSONL files from the given search roots.
///
/// Skips Claude `subagents/` directories / `agent-*.jsonl` files and Codex
/// subagent rollout files because they are auxiliary child sessions that
/// should not appear in recent-session and session-list views.
pub fn collect_session_jsonl_files(search_paths: &[String]) -> Vec<PathBuf> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let is_symlink = entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);
            let path = entry.path();

            if path.is_dir() {
                if is_symlink {
                    continue;
                }
                if path.file_name().and_then(|n| n.to_str()) == Some("subagents") {
                    continue;
                }
                walk(&path, files);
                continue;
            }

            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".jsonl") && !name.starts_with("agent-") {
                if codex_parent_thread_id_from_file(&path).is_some() {
                    continue;
                }
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    for search_path in search_paths {
        let root = Path::new(search_path);
        if root.is_dir() {
            walk(root, &mut files);
        }
    }
    files
}

/// Search for a JSONL file by session ID across the given search paths.
///
/// Prefers an exact `<session_id>.jsonl` match, but also supports Claude Desktop
/// `audit.jsonl` files whose content carries the session ID.
pub fn find_session_file_in_paths(session_id: &str, search_paths: &[String]) -> Option<String> {
    use std::io::{BufRead, BufReader};

    let target_filename = format!("{}.jsonl", session_id);
    let mut audit_match: Option<String> = None;

    for path in collect_session_jsonl_files(search_paths) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        if name == target_filename
            || (is_codex_session_path(&path.to_string_lossy())
                && codex_rollout_matches_session_id(&path, session_id))
        {
            return Some(path.to_string_lossy().to_string());
        }

        if name != "audit.jsonl" {
            continue;
        }

        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let reader = BufReader::new(file);
        let found = reader
            .lines()
            .take(50)
            .flatten()
            .any(|line| line.contains(session_id));
        if found {
            audit_match = Some(path.to_string_lossy().to_string());
        }
    }

    audit_match
}

/// Resolve the correct session ID and file path for `claude --resume`.
///
/// Claude CLI matches sessions by filename: it looks for `<session-id>.jsonl`.
/// But search results may come from auxiliary files (agent files, metadata files)
/// whose filename doesn't match the `sessionId` in their content.
///
/// This function handles four cases:
/// 1. **Codex spawned-subagent rollout**:
///    reads `session_meta.payload.source.subagent.thread_spawn.parent_thread_id`
///    and resolves to the parent rollout file when available
/// 2. **Claude subagent file** (`../session-id/subagents/agent-xxx.jsonl`):
///    resolves to the parent `session-id.jsonl`
/// 3. **Mismatched filename** (file's stem != session_id):
///    looks for `<session_id>.jsonl` in the same directory
/// 4. **Normal file**: returns as-is
pub fn resolve_parent_session(session_id: &str, file_path: &str) -> (String, String) {
    let path = std::path::Path::new(file_path);

    // Case 1: Codex spawned-subagent rollout. Codex stores child sessions as
    // normal rollout files and records the parent in session_meta.payload.source.
    if let Some((parent_id, parent_path)) = resolve_codex_subagent_session(path) {
        return (parent_id, parent_path);
    }

    // Case 2: subagent file under .../session-id/subagents/
    if let Some(parent) = path.parent() {
        if parent.file_name().and_then(|n| n.to_str()) == Some("subagents") {
            if let Some(session_dir) = parent.parent() {
                let parent_jsonl = session_dir.with_extension("jsonl");
                if parent_jsonl.exists() {
                    let dir_name = session_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(session_id);
                    return (
                        dir_name.to_string(),
                        parent_jsonl.to_string_lossy().to_string(),
                    );
                }
            }
        }
    }

    // Case 3: filename stem doesn't match session_id — find the correct file
    let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if file_stem != session_id {
        if let Some(parent_dir) = path.parent() {
            let correct_file = parent_dir.join(format!("{}.jsonl", session_id));
            if correct_file.exists() {
                return (
                    session_id.to_string(),
                    correct_file.to_string_lossy().to_string(),
                );
            }
        }
    }

    // Case 4: normal file
    (session_id.to_string(), file_path.to_string())
}

/// Detect whether message content was produced by a known automation tool.
/// Returns the tool name if a marker is found, `None` otherwise.
pub fn detect_automation(content: &str) -> Option<&'static str> {
    if matches_recog_automation_marker(content) {
        return Some(RECOG_TAG);
    }

    if matches_scheduled_task_marker(content) {
        return Some("scheduled");
    }

    if matches_ralphex_marker(content) {
        return Some("ralphex");
    }

    if matches_claude_mem_content_marker(content) {
        return Some(CLAUDE_MEM_TAG);
    }

    None
}

/// Detect automation by session file path. Reliable for tools that store
/// their sessions in a well-known directory (e.g. claude-mem's observer sessions
/// land under `~/.claude/projects/-Users-<u>--claude-mem-observer-sessions/`
/// because its worker cwd is `~/.claude-mem/observer-sessions`).
pub fn detect_automation_by_path(path: &std::path::Path) -> Option<&'static str> {
    let s = path.to_string_lossy();
    if s.contains(CLAUDE_MEM_OBSERVER_PATH_SEGMENT) || s.contains(CLAUDE_MEM_OBSERVER_PATH_RAW) {
        return Some(CLAUDE_MEM_TAG);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_source_from_cli_path() {
        let path = "/Users/user/.claude/projects/-Users-user-myproject/abc123.jsonl";
        assert_eq!(SessionSource::from_path(path), SessionSource::ClaudeCodeCLI);
    }

    #[test]
    fn test_session_source_from_desktop_path() {
        let path = "/Users/user/Library/Application Support/Claude/local-agent-mode-sessions/uuid1/uuid2/local_session/audit.jsonl";
        assert_eq!(SessionSource::from_path(path), SessionSource::ClaudeDesktop);
    }

    #[test]
    fn test_session_source_display_name() {
        assert_eq!(SessionSource::ClaudeCodeCLI.display_name(), "CLI");
        assert_eq!(SessionSource::ClaudeDesktop.display_name(), "Desktop");
    }

    #[test]
    fn test_session_provider_from_claude_path() {
        let path = "/Users/user/.claude/projects/-Users-user-myproject/abc123.jsonl";
        assert_eq!(SessionProvider::from_path(path), SessionProvider::Claude);
    }

    #[test]
    fn test_session_provider_from_codex_sessions_path() {
        let path =
            "/Users/user/.codex/sessions/2026/05/01/rollout-2026-05-01T12-00-00-session.jsonl";
        assert_eq!(SessionProvider::from_path(path), SessionProvider::Codex);
    }

    #[test]
    fn test_session_provider_from_codex_archived_sessions_path() {
        let path = "/Users/user/.codex/archived_sessions/rollout-2026-05-01T12-00-00-session.jsonl";
        assert_eq!(SessionProvider::from_path(path), SessionProvider::Codex);
    }

    #[test]
    fn test_session_provider_from_custom_codex_home_path() {
        let _lock = crate::TEST_ENV_MUTEX.lock().unwrap();
        let prev_codex = std::env::var("CODEX_HOME").ok();
        unsafe { std::env::set_var("CODEX_HOME", "/Users/user/custom-codex") };

        let session_path = "/Users/user/custom-codex/sessions/2026/05/01/rollout-2026-05-01T12-00-00-session.jsonl";
        let archived_path =
            "/Users/user/custom-codex/archived_sessions/rollout-2026-05-01T12-00-00-session.jsonl";
        let sibling_path = "/Users/user/custom-codexish/sessions/2026/05/01/rollout-2026-05-01T12-00-00-session.jsonl";

        assert_eq!(
            SessionProvider::from_path(session_path),
            SessionProvider::Codex
        );
        assert_eq!(
            SessionProvider::from_path(archived_path),
            SessionProvider::Codex
        );
        assert_eq!(
            SessionProvider::from_path(sibling_path),
            SessionProvider::Claude
        );

        unsafe { std::env::remove_var("CODEX_HOME") };
        if let Some(v) = prev_codex {
            unsafe { std::env::set_var("CODEX_HOME", v) };
        }
    }

    #[test]
    fn test_session_provider_display_name() {
        assert_eq!(SessionProvider::Claude.display_name(), "Claude");
        assert_eq!(SessionProvider::Codex.display_name(), "Codex");
    }

    #[test]
    fn test_extract_codex_session_id_from_rollout_path() {
        let path =
            "/Users/user/.codex/sessions/2026/05/01/rollout-2026-05-01T12-00-00-019f0000-0000-7000-8000-000000000001.jsonl";
        assert_eq!(
            extract_codex_session_id_from_path(path),
            Some("019f0000-0000-7000-8000-000000000001".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_codex_session_meta() {
        let json: serde_json::Value = serde_json::json!({
            "type": "session_meta",
            "payload": {"id": "codex-thread-id"}
        });
        assert_eq!(
            extract_session_id(&json),
            Some("codex-thread-id".to_string())
        );
    }

    #[test]
    fn test_extract_codex_parent_thread_id() {
        let json: serde_json::Value = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "codex-child-thread",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": "codex-parent-thread",
                            "depth": 1,
                            "agent_nickname": "Sagan",
                            "agent_role": "default"
                        }
                    }
                }
            }
        });
        assert_eq!(
            extract_codex_parent_thread_id(&json),
            Some("codex-parent-thread".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_cli_format() {
        let json: serde_json::Value = serde_json::json!({"sessionId": "abc123", "type": "user"});
        assert_eq!(extract_session_id(&json), Some("abc123".to_string()));
    }

    #[test]
    fn test_extract_session_id_desktop_format() {
        let json: serde_json::Value =
            serde_json::json!({"session_id": "desktop-456", "type": "user"});
        assert_eq!(extract_session_id(&json), Some("desktop-456".to_string()));
    }

    #[test]
    fn test_extract_session_id_cli_takes_precedence() {
        let json: serde_json::Value =
            serde_json::json!({"sessionId": "cli", "session_id": "desktop"});
        assert_eq!(extract_session_id(&json), Some("cli".to_string()));
    }

    #[test]
    fn test_extract_timestamp_cli_format() {
        let json: serde_json::Value = serde_json::json!({"timestamp": "2025-01-09T10:00:00Z"});
        let ts = extract_timestamp(&json).unwrap();
        assert_eq!(ts.to_rfc3339(), "2025-01-09T10:00:00+00:00");
    }

    #[test]
    fn test_extract_timestamp_desktop_format() {
        let json: serde_json::Value =
            serde_json::json!({"_audit_timestamp": "2025-01-09T10:00:00.000Z"});
        assert!(extract_timestamp(&json).is_some());
    }

    #[test]
    fn test_extract_branch_codex_session_meta() {
        let json: serde_json::Value = serde_json::json!({
            "type": "session_meta",
            "payload": {"git": {"branch": "main"}}
        });
        assert_eq!(extract_branch(&json), Some("main".to_string()));
    }

    #[test]
    fn test_extract_uuid() {
        let json: serde_json::Value = serde_json::json!({"uuid": "u1"});
        assert_eq!(extract_uuid(&json), Some("u1".to_string()));
    }

    #[test]
    fn test_extract_uuid_missing() {
        let json: serde_json::Value = serde_json::json!({"type": "user"});
        assert_eq!(extract_uuid(&json), None);
    }

    #[test]
    fn test_extract_parent_uuid() {
        let json: serde_json::Value = serde_json::json!({"parentUuid": "p1"});
        assert_eq!(extract_parent_uuid(&json), Some("p1".to_string()));
    }

    #[test]
    fn test_extract_leaf_uuid() {
        let json: serde_json::Value = serde_json::json!({"leafUuid": "l1"});
        assert_eq!(extract_leaf_uuid(&json), Some("l1".to_string()));
    }

    #[test]
    fn test_extract_record_type() {
        let json: serde_json::Value = serde_json::json!({"type": "user"});
        assert_eq!(extract_record_type(&json), Some("user"));
    }

    #[test]
    fn test_detect_automation_ralphex_marker() {
        let content = "Path A - No issues: Output <<<RALPHEX:REVIEW_DONE>>>";
        assert_eq!(detect_automation(content), Some("ralphex"));
    }

    #[test]
    fn test_detect_automation_ralphex_all_tasks_done() {
        let content = "Follow the plan and emit <<<RALPHEX:ALL_TASKS_DONE>>> when complete";
        assert_eq!(detect_automation(content), Some("ralphex"));
    }

    #[test]
    fn test_detect_automation_scheduled_task() {
        let content = r#"<scheduled-task name="chezmoi-sync" file="/Users/user/.claude/scheduled-tasks/chezmoi-sync/SCHEDULE.md">"#;
        assert_eq!(detect_automation(content), Some("scheduled"));
    }

    #[test]
    fn test_detect_automation_recog_marker() {
        let content = "<!-- ccs-automation:recog task=title -->\nПридумай короткое название";
        assert_eq!(detect_automation(content), Some("recog"));
    }

    #[test]
    fn test_detect_automation_recog_marker_with_leading_whitespace() {
        let content = "\n  <!-- ccs-automation:recog task=increment -->\nПроанализируй транскрипт";
        assert_eq!(detect_automation(content), Some("recog"));
    }

    #[test]
    fn test_detect_automation_no_marker() {
        let content = "How do I sort a list in Python?";
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_detect_automation_matches_ralphex_marker_anywhere() {
        let content = "Ralphex uses <<<RALPHEX:ALL_TASKS_DONE>>> signals.";
        assert_eq!(detect_automation(content), Some("ralphex"));
    }

    #[test]
    fn test_detect_automation_scheduled_task_not_at_start() {
        // scheduled-task must start the message (trim_start), not appear mid-text
        let content = r#"такие тоже надо детектить <scheduled-task name="chezmoi-sync">"#;
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_detect_automation_recog_marker_not_at_start() {
        let content = "Обсуждаем маркер <!-- ccs-automation:recog task=title --> в переписке";
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_detect_automation_priority_recog_over_embedded_markers() {
        let content =
            "<!-- ccs-automation:recog task=title -->\nTranscript mentions <<<RALPHEX:ALL_TASKS_DONE>>>";
        assert_eq!(detect_automation(content), Some("recog"));
    }

    #[test]
    fn test_detect_automation_empty_content() {
        assert_eq!(detect_automation(""), None);
    }

    #[test]
    fn test_detect_automation_partial_marker_no_match() {
        // Just "RALPHEX" without the <<< prefix should not match
        let content = "discussing RALPHEX in a conversation";
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_detect_automation_claude_mem_content_marker() {
        let content = r#"<observed_from_primary_session><user_request>hello</user_request></observed_from_primary_session>"#;
        assert_eq!(detect_automation(content), Some("claude-mem"));
    }

    #[test]
    fn test_detect_automation_claude_mem_marker_with_leading_whitespace() {
        // Observer prompts may have leading whitespace/newlines before the tag.
        let content = "\n  <observed_from_primary_session>hello</observed_from_primary_session>";
        assert_eq!(detect_automation(content), Some("claude-mem"));
    }

    #[test]
    fn test_detect_automation_claude_mem_marker_mid_text_no_match() {
        // Regression: casual mention of the tag inside a normal conversation
        // (e.g. discussing claude-mem in chat, pasting examples, writing tests)
        // must NOT classify the session as automation.
        let content = "Обсуждаем маркер <observed_from_primary_session> в переписке";
        assert_eq!(detect_automation(content), None);
    }

    #[test]
    fn test_detect_automation_priority_scheduled_over_claude_mem() {
        // If both markers present, cheaper/earlier detector wins.
        let content = r#"<scheduled-task name="x"> and later <observed_from_primary_session>..."#;
        assert_eq!(detect_automation(content), Some("scheduled"));
    }

    #[test]
    fn test_detect_automation_by_path_encoded_projects_dir() {
        let path = std::path::Path::new(
            "/Users/u/.claude/projects/-Users-u--claude-mem-observer-sessions/abc.jsonl",
        );
        assert_eq!(detect_automation_by_path(path), Some("claude-mem"));
    }

    #[test]
    fn test_detect_automation_by_path_raw_data_dir() {
        let path = std::path::Path::new("/home/u/.claude-mem/observer-sessions/abc.jsonl");
        assert_eq!(detect_automation_by_path(path), Some("claude-mem"));
    }

    #[test]
    fn test_detect_automation_by_path_ignores_unrelated() {
        // Plain project path
        let p1 =
            std::path::Path::new("/Users/u/.claude/projects/-Users-u-projects-myapp/session.jsonl");
        assert_eq!(detect_automation_by_path(p1), None);

        // Path that mentions "claude-mem" but NOT the observer-sessions subdir —
        // e.g. a clone of the claude-mem repo itself must not false-positive.
        let p2 = std::path::Path::new("/Users/u/projects/claude-mem/src/foo.ts");
        assert_eq!(detect_automation_by_path(p2), None);
    }

    #[test]
    fn test_resolve_parent_for_subagent_uses_parent_session_id() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join("aaa-bbb-ccc");
        let subagents_dir = session_dir.join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();

        // Create parent JSONL
        let parent_jsonl = dir.path().join("aaa-bbb-ccc.jsonl");
        fs::write(&parent_jsonl, "{}").unwrap();

        // Create subagent JSONL
        let agent_file = subagents_dir.join("agent-xyz.jsonl");
        fs::write(&agent_file, "{}").unwrap();

        let (sid, fpath) = resolve_parent_session("wrong-id", agent_file.to_str().unwrap());
        assert_eq!(sid, "aaa-bbb-ccc");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_codex_subagent_rollout() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let session_dir = dir.path().join(".codex/sessions/2026/05/03");
        fs::create_dir_all(&session_dir).unwrap();

        let parent_id = "019f0000-0000-7000-8000-000000000001";
        let child_id = "019f0000-0000-7000-8000-000000000002";
        let parent_file =
            session_dir.join(format!("rollout-2026-05-03T10-00-00-{parent_id}.jsonl"));
        let child_file = session_dir.join(format!("rollout-2026-05-03T10-01-00-{child_id}.jsonl"));

        fs::write(
            &parent_file,
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{parent_id}","source":"cli","cwd":"/tmp/project"}}}}"#
            ),
        )
        .unwrap();
        fs::write(
            &child_file,
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{child_id}","cwd":"/tmp/project","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent_id}","depth":1,"agent_nickname":"Sagan","agent_role":"default"}}}}}}}}}}"#
            ),
        )
        .unwrap();

        let (sid, fpath) = resolve_parent_session(child_id, child_file.to_str().unwrap());
        assert_eq!(sid, parent_id);
        assert_eq!(fpath, parent_file.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_top_level_agent_uses_filename_session_id() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-Users-proj");
        fs::create_dir_all(&project_dir).unwrap();

        // Parent session file
        let parent_jsonl = project_dir.join("64cd6570-parent.jsonl");
        fs::write(&parent_jsonl, r#"{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        // Top-level agent file with sessionId pointing to parent
        let agent_file = project_dir.join("agent-abc123.jsonl");
        fs::write(&agent_file, r#"{"type":"user","message":{"role":"user","content":"sub"},"sessionId":"64cd6570-parent","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        let (sid, fpath) = resolve_parent_session("64cd6570-parent", agent_file.to_str().unwrap());
        assert_eq!(sid, "64cd6570-parent");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_exact_user_scenario() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("-Users-Shared-projects-avito-android");
        fs::create_dir_all(&project_dir).unwrap();

        // Main session file
        let main_file = project_dir.join("64cd6570-3475-47fd-a2cc-2da718d0dcb3.jsonl");
        fs::write(&main_file, r#"{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"64cd6570-3475-47fd-a2cc-2da718d0dcb3","timestamp":"2025-01-01T00:00:00Z"}"#).unwrap();

        // Auxiliary metadata file (different UUID filename, same sessionId inside)
        let aux_file = project_dir.join("68483698-e6fc-4ea8-a85e-989e6dfa5c2f.jsonl");
        fs::write(&aux_file, r#"{"type":"last-prompt","lastPrompt":"test","sessionId":"64cd6570-3475-47fd-a2cc-2da718d0dcb3"}"#).unwrap();

        let (sid, fpath) = resolve_parent_session(
            "64cd6570-3475-47fd-a2cc-2da718d0dcb3",
            aux_file.to_str().unwrap(),
        );
        assert_eq!(sid, "64cd6570-3475-47fd-a2cc-2da718d0dcb3");
        assert_eq!(fpath, main_file.to_string_lossy());
    }

    #[test]
    fn test_resolve_parent_for_auxiliary_file_finds_main_session() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-Users-proj");
        fs::create_dir_all(&project_dir).unwrap();

        // Parent session file
        let parent_jsonl = project_dir.join("64cd6570-parent.jsonl");
        fs::write(&parent_jsonl, "{}").unwrap();

        // Auxiliary file with different filename but sessionId pointing to parent
        let aux_file = project_dir.join("1630cd72-auxiliary.jsonl");
        fs::write(&aux_file, "{}").unwrap();

        let (sid, fpath) = resolve_parent_session("64cd6570-parent", aux_file.to_str().unwrap());
        assert_eq!(sid, "64cd6570-parent");
        assert_eq!(fpath, parent_jsonl.to_string_lossy());
    }

    #[test]
    fn test_collect_session_jsonl_files_skips_subagents_and_agents() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let root = dir.path().join("projects");
        let project = root.join("-Users-user-demo");
        let subagents = project.join("subagents");
        fs::create_dir_all(&subagents).unwrap();

        let main = project.join("session.jsonl");
        let agent = project.join("agent-task.jsonl");
        let subagent = subagents.join("agent-child.jsonl");
        let audit = project.join("audit.jsonl");

        fs::write(&main, "{}").unwrap();
        fs::write(&agent, "{}").unwrap();
        fs::write(&subagent, "{}").unwrap();
        fs::write(&audit, "{}").unwrap();

        let files = collect_session_jsonl_files(&[root.to_string_lossy().to_string()]);
        let names: std::collections::HashSet<_> = files
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();

        assert!(names.contains("session.jsonl"));
        assert!(names.contains("audit.jsonl"));
        assert!(!names.contains("agent-task.jsonl"));
        assert!(!names.contains("agent-child.jsonl"));
    }

    #[test]
    fn test_collect_session_jsonl_files_skips_codex_subagent_rollouts() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let root = dir.path().join(".codex/sessions");
        let day = root.join("2026/05/03");
        fs::create_dir_all(&day).unwrap();

        let parent_id = "019f0000-0000-7000-8000-000000000001";
        let child_id = "019f0000-0000-7000-8000-000000000002";
        let parent_file = day.join(format!("rollout-2026-05-03T10-00-00-{parent_id}.jsonl"));
        let child_file = day.join(format!("rollout-2026-05-03T10-01-00-{child_id}.jsonl"));

        fs::write(
            &parent_file,
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{parent_id}","source":"cli","cwd":"/tmp/project"}}}}"#
            ),
        )
        .unwrap();
        fs::write(
            &child_file,
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{child_id}","cwd":"/tmp/project","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent_id}","depth":1}}}}}}}}}}"#
            ),
        )
        .unwrap();

        let files = collect_session_jsonl_files(&[root.to_string_lossy().to_string()]);
        assert_eq!(files, vec![parent_file]);
    }

    #[test]
    fn test_find_session_file_in_paths_finds_exact_match_recursively() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let nested = dir
            .path()
            .join("desktop")
            .join("uuid-1")
            .join("uuid-2")
            .join("local_123");
        fs::create_dir_all(&nested).unwrap();

        let session = nested.join("sess-123.jsonl");
        fs::write(&session, "{}").unwrap();

        let found =
            find_session_file_in_paths("sess-123", &[dir.path().to_string_lossy().to_string()]);

        assert_eq!(found, Some(session.to_string_lossy().to_string()));
    }

    #[test]
    fn test_find_session_file_in_paths_finds_desktop_audit_by_content() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let nested = dir
            .path()
            .join("local-agent-mode-sessions")
            .join("uuid-1")
            .join("uuid-2")
            .join("local_123");
        fs::create_dir_all(&nested).unwrap();

        let audit = nested.join("audit.jsonl");
        fs::write(
            &audit,
            r#"{"type":"user","session_id":"desktop-123","_audit_timestamp":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        let found =
            find_session_file_in_paths("desktop-123", &[dir.path().to_string_lossy().to_string()]);

        assert_eq!(found, Some(audit.to_string_lossy().to_string()));
    }
}
