use crate::agent_search::{AgentEvent, AgentSearchHandle, FoundSession};
use crate::config::AgentSearchConfig;
use crate::project_match::{normalize_path_for_prefix_check, path_is_within_project};
use crate::recent::{collect_recent_sessions, detect_session_automation, RecentSession};
use crate::resume::encode_path_for_claude;
use crate::search::{group_by_session, search_multiple_paths, SessionGroup};
use crate::session::{SessionProvider, SessionSource};
use crate::tree::SessionTree;
use crate::tui::dispatch::{KeyAction, KeyContext};
use crate::tui::search_mode::session_outcome;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const DEBOUNCE_MS: u64 = 300;
const RECENT_SESSIONS_LIMIT: usize = 100;

/// Encapsulates text input and cursor position, enforcing the invariant
/// that `cursor_pos` is always a valid byte offset within `text` (on a char boundary).
#[derive(Debug, Clone)]
pub struct InputState {
    text: String,
    cursor_pos: usize,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
        }
    }

    fn clamp_cursor_to_boundary(text: &str, cursor: usize) -> usize {
        let mut cursor = cursor.min(text.len());
        while cursor > 0 && !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        cursor
    }

    // -- Getters --

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Set text content and place cursor at end.
    pub fn set_text(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor_pos = self.text.len();
    }

    /// Set text and cursor position.
    ///
    /// The cursor is clamped to the nearest valid UTF-8 character boundary at or
    /// before the requested byte offset so subsequent cursor movement and delete
    /// operations never panic on mid-codepoint offsets.
    pub fn set_text_and_cursor(&mut self, s: &str, cursor: usize) {
        self.text = s.to_string();
        self.cursor_pos = Self::clamp_cursor_to_boundary(&self.text, cursor);
    }

    /// Consume and return the inner text.
    pub fn into_text(self) -> String {
        self.text
    }

    // -- Mutation methods --

    /// Insert a character at the current cursor position.
    pub fn push_char(&mut self, c: char) {
        self.text.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Delete the character before the cursor. Returns true if something was deleted.
    pub fn backspace(&mut self) -> bool {
        if self.cursor_pos > 0 {
            let prev = self.text[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.remove(prev);
            self.cursor_pos = prev;
            true
        } else {
            false
        }
    }

    /// Delete the character at the cursor. Returns true if something was deleted.
    pub fn delete_forward(&mut self) -> bool {
        if self.cursor_pos < self.text.len() {
            self.text.remove(self.cursor_pos);
            true
        } else {
            false
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.text[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_pos < self.text.len() {
            self.cursor_pos += self.text[self.cursor_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
        }
    }

    pub fn move_word_left(&mut self) {
        let before = &self.text[..self.cursor_pos];
        let mut chars = before.char_indices().rev();
        // Skip non-alphanumeric
        while let Some((i, c)) = chars.next() {
            if c.is_alphanumeric() {
                // Found alphanumeric — now skip the rest of the word
                self.cursor_pos = i;
                for (j, c2) in chars {
                    if !c2.is_alphanumeric() {
                        self.cursor_pos = j + c2.len_utf8();
                        return;
                    }
                    self.cursor_pos = j;
                }
                self.cursor_pos = 0;
                return;
            }
        }
        self.cursor_pos = 0;
    }

    pub fn move_word_right(&mut self) {
        let after = &self.text[self.cursor_pos..];
        let mut chars = after.char_indices();
        // Skip alphanumeric
        while let Some((_i, c)) = chars.next() {
            if !c.is_alphanumeric() {
                // Found non-alphanumeric — now skip to next word
                for (j, c2) in chars {
                    if c2.is_alphanumeric() {
                        self.cursor_pos += j;
                        return;
                    }
                }
                self.cursor_pos = self.text.len();
                return;
            }
        }
        self.cursor_pos = self.text.len();
    }

    /// Delete from cursor to previous word boundary. Returns true if something was deleted.
    pub fn delete_word_left(&mut self) -> bool {
        if self.cursor_pos == 0 {
            return false;
        }
        let old_pos = self.cursor_pos;
        self.move_word_left();
        self.text.drain(self.cursor_pos..old_pos);
        true
    }

    /// Delete from cursor to next word boundary. Returns true if something was deleted.
    pub fn delete_word_right(&mut self) -> bool {
        if self.cursor_pos >= self.text.len() {
            return false;
        }
        let old_pos = self.cursor_pos;
        self.move_word_right();
        let new_pos = self.cursor_pos;
        self.cursor_pos = old_pos;
        self.text.drain(old_pos..new_pos);
        true
    }

    pub fn move_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_pos = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_pos = 0;
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

fn search_path_is_within_claude_project(search_path: &str, encoded_cwd: &str) -> bool {
    let search_path = normalize_path_for_prefix_check(search_path);
    let parts: Vec<&str> = search_path.split('/').filter(|p| !p.is_empty()).collect();

    parts
        .windows(2)
        .any(|window| window[0] == "projects" && window[1] == encoded_cwd)
}

fn derive_current_project_paths(
    search_paths: &[String],
    current_dir: Option<&Path>,
) -> Vec<String> {
    let Some(encoded_cwd) = current_dir
        .and_then(|cwd| cwd.to_str())
        .map(encode_path_for_claude)
    else {
        return Vec::new();
    };

    search_paths
        .iter()
        .filter_map(|base| {
            if Path::new(base).is_dir() && search_path_is_within_claude_project(base, &encoded_cwd)
            {
                return Some(normalize_path_for_prefix_check(base));
            }

            let candidate = Path::new(base).join(&encoded_cwd);
            if candidate.is_dir() {
                candidate.to_str().map(normalize_path_for_prefix_check)
            } else {
                None
            }
        })
        .collect()
}

fn search_path_is_standard_codex_scope(search_path: &str) -> bool {
    let parts: Vec<&str> = search_path.split('/').filter(|p| !p.is_empty()).collect();

    parts.iter().enumerate().any(|(idx, part)| {
        *part == ".codex"
            && parts
                .get(idx + 1)
                .is_some_and(|next| crate::session::CODEX_SESSION_SUBDIRS.contains(next))
    })
}

fn search_path_supports_codex_cwd_scope(search_path: &str) -> bool {
    let search_path = normalize_path_for_prefix_check(search_path);
    if search_path_is_standard_codex_scope(&search_path) {
        return true;
    }
    if let Some(default_codex_home) = dirs::home_dir()
        .and_then(|home| home.to_str().map(|s| format!("{s}/.codex")))
        .map(|home| normalize_path_for_prefix_check(&home))
    {
        if search_path == default_codex_home {
            return true;
        }
    }

    let Ok(codex_home) = std::env::var("CODEX_HOME") else {
        return false;
    };
    let codex_home = normalize_path_for_prefix_check(&codex_home);
    if codex_home.is_empty() || codex_home == "/" {
        return false;
    }
    if search_path == codex_home {
        return true;
    }

    crate::session::CODEX_SESSION_SUBDIRS.iter().any(|subdir| {
        let codex_session_root = format!("{codex_home}/{subdir}");
        path_is_within_project(&search_path, &codex_session_root)
    })
}

/// Opencode DB paths always carry per-session `directory` metadata, so they
/// support project filtering by cwd the same way Codex roots do.
fn search_path_supports_opencode_cwd_scope(search_path: &str) -> bool {
    crate::session::opencode::is_opencode_session_path(search_path)
}

/// Either provider's cwd scope support — for places that allow project
/// filtering when *any* per-session cwd metadata is reachable.
fn search_path_supports_cwd_scope(search_path: &str) -> bool {
    search_path_supports_codex_cwd_scope(search_path)
        || search_path_supports_opencode_cwd_scope(search_path)
}

fn canonicalize_path_to_string(path: &Path) -> Option<String> {
    std::fs::canonicalize(path)
        .ok()
        .unwrap_or_else(|| path.to_path_buf())
        .to_str()
        .map(String::from)
}

/// Decide whether `session` belongs to the project the user is currently
/// in. Combines the Claude-style file_path check (session file lives
/// inside one of `project_paths`) with an asymmetric Codex cwd check:
/// Codex `session.cwd` must be inside (or equal to) `current_cwd`. The
/// asymmetry is intentional — symmetric matching would let a Codex
/// session recorded at `$HOME` falsely match every project under
/// `$HOME`, which was observed in the wild.
fn session_matches_project(
    session: &RecentSession,
    project_paths: &[String],
    current_cwd: Option<&str>,
) -> bool {
    if project_paths
        .iter()
        .any(|p| path_is_within_project(&session.file_path, p))
    {
        return true;
    }
    let provider = SessionProvider::from_path(&session.file_path);
    // Both Codex and Opencode store the session's working directory in
    // metadata; for either we can match by cwd when the Claude-style
    // file_path prefix check failed. The match is asymmetric: session_cwd
    // must lie inside current_cwd, never the reverse.
    if !matches!(provider, SessionProvider::Codex | SessionProvider::Opencode) {
        return false;
    }
    match (session.cwd.as_deref(), current_cwd) {
        (Some(session_cwd), Some(cwd)) => path_is_within_project(session_cwd, cwd),
        _ => false,
    }
}

fn group_matches_project(
    group: &SessionGroup,
    project_paths: &[String],
    current_cwd: Option<&str>,
    opencode_cwd_cache: &mut HashMap<String, Option<String>>,
) -> bool {
    if project_paths
        .iter()
        .any(|p| path_is_within_project(&group.file_path, p))
    {
        return true;
    }

    let provider = SessionProvider::from_path(&group.file_path);
    let Some(cwd) = current_cwd else {
        return false;
    };

    let session_cwd = match provider {
        SessionProvider::Codex => crate::session::read_codex_session_cwd(&group.file_path),
        SessionProvider::Opencode => {
            read_opencode_session_cwd_cached(&group.file_path, opencode_cwd_cache)
        }
        SessionProvider::Claude => return false,
    };

    session_cwd
        .map(|cwd_str| canonicalize_path_to_string(Path::new(&cwd_str)).unwrap_or(cwd_str))
        .is_some_and(|session_cwd| path_is_within_project(&session_cwd, cwd))
}

/// Look up an Opencode session's `directory` field via the synthetic path
/// (`<db>#<session_id>`) so the project-filter cwd check works on search
/// results whose RecentSession.cwd isn't populated.
fn read_opencode_session_cwd(file_path: &str) -> Option<String> {
    let (db, sid) = crate::session::opencode::parse_session_path(file_path)?;
    crate::session::opencode::OpencodeSession::from_db(&db, &sid)?.directory
}

/// Cache-aware wrapper for [`read_opencode_session_cwd`]. Opencode lookups
/// open the SQLite DB, so the project-filter rebuild can run hundreds of
/// these per toggle. Cache both `Some` and `None` outcomes so repeated
/// misses are also O(1).
fn read_opencode_session_cwd_cached(
    file_path: &str,
    cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if let Some(cached) = cache.get(file_path) {
        return cached.clone();
    }
    let result = read_opencode_session_cwd(file_path);
    cache.insert(file_path.to_string(), result.clone());
    result
}

/// Recent sessions sub-state: encapsulates global/project data sources,
/// filtered view, background loaders, and navigation cursor.
pub struct RecentState {
    /// Unfiltered global sessions (loaded once at startup).
    /// Invariant: sorted newest-first by `sort_recent_sessions_desc`.
    pub(crate) all: Vec<RecentSession>,
    /// Project-specific sessions (loaded when project filter is activated).
    /// Invariant: sorted newest-first by `sort_recent_sessions_desc`.
    pub(crate) project: Option<Vec<RecentSession>>,
    pub(crate) filtered: Vec<RecentSession>,
    pub(crate) cursor: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) loading: bool,
    /// Channel for global recent session background load
    pub(crate) load_rx: Option<Receiver<Vec<RecentSession>>>,
    pub(crate) project_loading: bool,
    /// Channel for project-specific recent session background load
    pub(crate) project_load_rx: Option<Receiver<Vec<RecentSession>>>,
}

impl RecentState {
    pub(crate) fn new(search_paths: Vec<String>) -> Self {
        let load_rx = Self::spawn_load(search_paths);
        Self {
            all: Vec::new(),
            project: None,
            filtered: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
            loading: true,
            load_rx: Some(load_rx),
            project_loading: false,
            project_load_rx: None,
        }
    }

    fn spawn_load(paths: Vec<String>) -> Receiver<Vec<RecentSession>> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let sessions = collect_recent_sessions(&paths, RECENT_SESSIONS_LIMIT);
            let _ = tx.send(sessions);
        });
        rx
    }

    /// Trigger a fresh background load of project-specific sessions.
    pub(crate) fn start_project_load(&mut self, project_paths: Vec<String>) {
        self.project = None;
        self.project_loading = true;
        self.project_load_rx = Some(Self::spawn_load(project_paths));
    }

    /// Poll background channels. Returns `(global_loaded, project_loaded)`.
    pub(crate) fn poll(&mut self) -> (bool, bool) {
        let mut global_loaded = false;
        let mut project_loaded = false;

        if let Some(ref rx) = self.load_rx {
            if let Ok(sessions) = rx.try_recv() {
                self.all = sessions;
                self.loading = false;
                self.load_rx = None;
                global_loaded = true;
            }
        }

        if let Some(ref rx) = self.project_load_rx {
            if let Ok(sessions) = rx.try_recv() {
                self.project = Some(sessions);
                self.project_loading = false;
                self.project_load_rx = None;
                project_loaded = true;
            }
        }

        (global_loaded, project_loaded)
    }

    /// Rebuild `filtered` from source sessions based on current filters.
    ///
    /// When `project_filter` is on, the source is a union of the
    /// Claude-specific project load (`self.project`) and any Codex sessions
    /// already discovered in `self.all`. Codex sessions never appear in
    /// `self.project` because `start_project_load` only scans Claude paths,
    /// so without the union they would silently drop out the moment the
    /// project load resolves. Dedup is by `session_id` to handle the
    /// (rare) case where the same Codex session shows up in both lists.
    pub(crate) fn apply_filter(
        &mut self,
        project_filter: bool,
        project_paths: &[String],
        current_cwd: Option<&str>,
        automation_filter: &AutomationFilter,
    ) {
        let project_filtered: Vec<_> = if project_filter
            && (!project_paths.is_empty() || current_cwd.is_some())
        {
            // When the project-specific load has resolved, take its
            // Claude sessions verbatim and merge in any Codex sessions
            // from the global pool (start_project_load scans only
            // Claude paths). When the load hasn't resolved yet, fall
            // back to the global pool so Ctrl+A doesn't blank the
            // list while the background scan is in flight.
            let (mut source, seen): (Vec<RecentSession>, HashSet<String>) =
                match self.project.as_ref() {
                    Some(project) => {
                        let seen = project.iter().map(|s| s.session_id.clone()).collect();
                        (project.clone(), seen)
                    }
                    None => (self.all.clone(), HashSet::new()),
                };
            if self.project.is_some() {
                // `start_project_load` only scans Claude paths, so Codex
                // and Opencode sessions are missing from `self.project`.
                // Union them in from `self.all` (which has all providers)
                // before the per-session project-match filter runs.
                source.extend(
                    self.all
                        .iter()
                        .filter(|s| {
                            let provider = SessionProvider::from_path(&s.file_path);
                            matches!(provider, SessionProvider::Codex | SessionProvider::Opencode)
                                && !seen.contains(&s.session_id)
                        })
                        .cloned(),
                );
            }
            let mut filtered: Vec<RecentSession> = source
                .into_iter()
                .filter(|s| session_matches_project(s, project_paths, current_cwd))
                .collect();
            // Concatenating two already-sorted slices does not preserve order; re-sort
            // the filtered union with the shared helper so the view matches the
            // unfiltered ordering.
            crate::recent::sort_recent_sessions_desc(&mut filtered);
            filtered
        } else {
            self.all.clone()
        };

        self.filtered = match automation_filter {
            AutomationFilter::All => project_filtered,
            AutomationFilter::Manual => project_filtered
                .into_iter()
                .filter(|s| s.automation.is_none())
                .collect(),
            AutomationFilter::Auto => project_filtered
                .into_iter()
                .filter(|s| s.automation.is_some())
                .collect(),
        };

        if self.filtered.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = self.cursor.min(self.filtered.len().saturating_sub(1));
        }
    }

    /// Adjust scroll offset to keep cursor visible.
    pub(crate) fn adjust_scroll(&mut self, visible_height: usize) {
        if self.cursor >= self.scroll_offset + visible_height {
            self.scroll_offset = self.cursor.saturating_sub(visible_height.saturating_sub(1));
        } else if self.cursor < self.scroll_offset {
            self.scroll_offset = self.cursor;
        }
    }

    /// Total count of sessions in the active source for the status bar
    /// (pre `session_matches_project` / pre `automation_filter`). After
    /// the union change in `apply_filter`, when `project_filter` is on the
    /// source is a union of `self.project` (Claude) and Codex sessions in
    /// `self.all`; otherwise `self.all`. This keeps the
    /// "X recent (Y hidden by filter)" message in
    /// `recent_sessions_status_text` coherent.
    pub fn total_count(&self, project_filter: bool) -> usize {
        if project_filter {
            let mut total = self
                .project
                .as_ref()
                .map(|p| p.len())
                .unwrap_or(self.all.len());
            // When `self.project` is loaded, count Codex + Opencode sessions
            // from `self.all` that are not already in `self.project` (matches
            // the union built in apply_filter). Skip when project hasn't
            // loaded yet — `self.all` already contains those sessions then.
            if let Some(project) = self.project.as_ref() {
                let seen: HashSet<&str> = project.iter().map(|s| s.session_id.as_str()).collect();
                total += self
                    .all
                    .iter()
                    .filter(|s| {
                        let provider = SessionProvider::from_path(&s.file_path);
                        matches!(provider, SessionProvider::Codex | SessionProvider::Opencode)
                            && !seen.contains(s.session_id.as_str())
                    })
                    .count();
            }
            total
        } else {
            self.all.len()
        }
    }

    /// Whether any background load is in progress.
    pub fn is_loading(&self, project_filter: bool) -> bool {
        self.loading || (project_filter && self.project_loading)
    }
}

fn apply_recent_automation_to_groups(
    groups: &mut [SessionGroup],
    recent_sessions: &[RecentSession],
    automation_cache: &mut HashMap<String, Option<String>>,
) {
    let mut automation_by_session_id: HashMap<&str, String> = HashMap::new();
    for session in recent_sessions {
        if let Some(automation) = &session.automation {
            automation_by_session_id
                .entry(session.session_id.as_str())
                .or_insert_with(|| automation.clone());
        }
        automation_cache
            .entry(session.file_path.clone())
            .or_insert_with(|| session.automation.clone());
    }

    for group in groups {
        if group.automation.is_some() {
            automation_cache.insert(group.file_path.clone(), group.automation.clone());
            continue;
        }

        if let Some(automation) = automation_by_session_id
            .get(group.session_id.as_str())
            .cloned()
        {
            automation_cache.insert(group.file_path.clone(), Some(automation.clone()));
            group.automation = Some(automation);
            continue;
        }

        if let Some(cached) = automation_cache.get(&group.file_path) {
            group.automation = cached.clone();
            continue;
        }

        let detected = detect_session_automation(Path::new(&group.file_path));
        automation_cache.insert(group.file_path.clone(), detected.clone());
        group.automation = detected;
    }
}

/// Update the message count in place for every group backed by `file_path`.
fn set_group_message_count(
    groups: &mut [SessionGroup],
    file_path: &str,
    count: usize,
    compacted: bool,
) {
    for group in groups {
        if group.file_path == file_path {
            group.message_count = Some(count);
            group.message_count_compacted = compacted;
        }
    }
}

/// A session selected in picker mode, ready for output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedSession {
    pub session_id: String,
    pub file_path: String,
    pub source: SessionSource,
    pub project: String,
    pub message_uuid: Option<String>,
}

impl PickedSession {
    pub fn to_key_value(&self) -> String {
        let mut out = format!(
            "session_id: {}\nfile_path: {}\nsource: {}\nproject: {}",
            self.session_id,
            self.file_path,
            self.source.display_name(),
            self.project,
        );
        if let Some(ref uuid) = self.message_uuid {
            out.push_str(&format!("\nmessage_uuid: {}", uuid));
        }
        out
    }

    pub fn write_output(&self, output_path: Option<&str>) -> Result<(), String> {
        let content = self.to_key_value();
        match output_path {
            Some(path) => {
                // Add trailing newline for file output so parsers can reliably read the last line
                std::fs::write(path, format!("{}\n", content))
                    .map_err(|e| format!("Failed to write output to {}: {}", path, e))
            }
            None => {
                use std::io::Write;
                // Use println + flush to ensure output is written before process::exit()
                println!("{}", content);
                std::io::stdout()
                    .flush()
                    .map_err(|e| format!("Failed to flush stdout: {}", e))
            }
        }
    }
}

/// Result of TUI interaction — what the user chose to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiOutcome {
    /// User quit without selecting anything (Esc, Ctrl-C)
    Quit,
    /// User selected a session to resume in normal mode
    Resume {
        session_id: String,
        file_path: String,
        source: SessionSource,
        uuid: Option<String>,
    },
    /// User picked a session in picker mode
    Pick(PickedSession),
}

/// Filter mode for automated vs manual sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationFilter {
    /// Show all sessions
    All,
    /// Show only manual (non-automated) sessions (default)
    Manual,
    /// Show only automated sessions
    Auto,
}

/// Result from a per-request search thread.
///
/// With `SearchHandle` as the source of truth, secondary validation by
/// query/paths/use_regex was redundant — newer requests cancel older ones
/// before they can deliver, and any straggler with a stale `seq` is
/// dropped on arrival. Only `seq` (for routing) and `query` (preserved on
/// `results_query` so renderers can show what produced the current
/// matches) flow through to `handle_search_result`.
pub(crate) struct BackgroundSearchResult {
    pub seq: u64,
    pub query: String,
    pub result: Result<crate::search::SearchResult, String>,
}

/// Handle on a single in-flight search request. Carries the request's
/// monotonic sequence number and a cooperative cancellation flag that the
/// background thread checks while iterating ripgrep output. `App.search.current
/// .is_some()` is the single source of truth for "is a search in flight";
/// the previous `searching: bool` shadow flag has been removed because any
/// early return from `handle_search_result` could leave it stuck at `true`.
pub(crate) struct SearchHandle {
    pub seq: u64,
    pub cancel: Arc<AtomicBool>,
}

/// Search-related state: results, cursors, per-request channel.
pub struct SearchState {
    /// All search result groups (unfiltered)
    pub(crate) all_groups: Vec<SessionGroup>,
    /// Search result groups filtered by automation filter
    pub groups: Vec<SessionGroup>,
    pub results_count: usize,
    pub results_query: String,
    pub group_cursor: usize,
    pub sub_cursor: usize,
    pub expanded: bool,
    /// Handle on the in-flight search request, if any. Replaces the prior
    /// `searching: bool`. `current.is_some()` is the source of truth for the
    /// "Searching…" status.
    pub(crate) current: Option<SearchHandle>,
    pub error: Option<String>,
    /// Cache: file_path → set of uuids on the latest chain (for fork indicator)
    pub latest_chains: HashMap<String, HashSet<String>>,
    /// Channel to receive search results from per-request search threads
    pub(crate) search_rx: Receiver<BackgroundSearchResult>,
    /// Sender end of the result channel — cloned into each spawned per-request
    /// thread so it can deliver its result back to the UI.
    pub(crate) result_tx: Sender<BackgroundSearchResult>,
    /// Monotonic request sequence to ignore stale async results
    pub(crate) search_seq: u64,
    /// Whether the last search hit the per-file match limit (results may be incomplete)
    pub search_truncated: bool,
    /// Channel to receive (file_path, count, compacted) from background message-counting thread
    pub(crate) message_count_rx: Option<Receiver<(String, usize, bool)>>,
    /// Cancellation flag for the background counting thread
    pub(crate) message_count_cancel: Option<Arc<AtomicBool>>,
}

/// Tree-view state: loaded tree, cursor, scroll, background loader.
pub struct TreeState {
    /// The loaded session tree
    pub session_tree: Option<SessionTree>,
    /// Cursor position in tree rows
    pub tree_cursor: usize,
    /// Vertical scroll offset for tree view
    pub tree_scroll_offset: usize,
    /// Whether tree is currently loading
    pub tree_loading: bool,
    /// Channel to receive loaded tree from background thread
    pub(crate) tree_load_rx: Option<Receiver<Result<SessionTree, String>>>,
    /// Whether tree mode was the initial mode (launched with --tree)
    pub tree_mode_standalone: bool,
}

/// Ctrl+G agent search state. Three phases: input (no agent, no hits),
/// searching (`search` is `Some`), results (`hits` is `Some`).
pub struct AiState {
    pub active: bool,
    pub query: InputState,
    /// The running agent; dropping it kills the agent.
    pub(crate) search: Option<AgentSearchHandle>,
    /// Search queries the running agent has tried so far.
    pub progress: Vec<String>,
    pub hits: Option<Vec<FoundSession>>,
    pub hit_cursor: usize,
    pub error: Option<String>,
}

impl AiState {
    pub fn searching(&self) -> bool {
        self.search.is_some()
    }

    fn selected_hit(&self) -> Option<&FoundSession> {
        self.hits.as_ref()?.get(self.hit_cursor)
    }

    /// Back to the input phase: stop the agent and forget its results.
    fn reset_search(&mut self) {
        self.search = None;
        self.progress.clear();
        self.hits = None;
        self.hit_cursor = 0;
        self.error = None;
    }
}

/// All fields needed to resume a session, bundled as one value.
/// Replaces the former 4 separate `Option<String>` fields on `App`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTarget {
    pub session_id: String,
    pub file_path: String,
    pub source: SessionSource,
    /// UUID of the selected message (for branch-aware resume from tree view)
    pub uuid: Option<String>,
}

/// Internal outcome set by `on_enter` / `on_enter_tree`.
/// Consumed by `take_outcome()` to produce the public `TuiOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppOutcome {
    Resume(ResumeTarget),
    Pick(PickedSession),
}

pub struct App {
    pub input: InputState,
    pub search: SearchState,
    pub tree: TreeState,
    pub typing: bool,
    pub search_paths: Vec<String>,
    pub last_query: String,
    pub last_keystroke: Option<Instant>,
    pub preview_mode: bool,
    pub should_quit: bool,
    /// Outcome set by `on_enter` / `on_enter_tree` — consumed by `take_outcome()`.
    /// Replaces the former 4 separate resume `Option` fields + `picked_session`.
    pub outcome: Option<AppOutcome>,
    /// Flag to force a full terminal redraw (clears diff optimization artifacts)
    pub needs_full_redraw: bool,
    /// Regex search mode (Ctrl+R to toggle)
    pub regex_mode: bool,
    /// Track last regex mode used for search
    pub(crate) last_regex_mode: bool,
    /// Track last search path scope used for search
    pub(crate) last_search_paths: Vec<String>,
    /// Track last project filter state used for search. The path list may
    /// stay the same when cwd-based Codex scoping is active, while the
    /// post-search predicate still changes.
    pub(crate) last_project_filter: bool,
    /// Tree explorer mode
    pub tree_mode: bool,
    /// Whether search is scoped to current project only (Ctrl+A toggle)
    pub project_filter: bool,
    /// Filter for automated vs manual sessions (Ctrl+H toggle)
    pub automation_filter: AutomationFilter,
    /// Cache: file_path -> resolved automation marker (including negative lookups)
    automation_cache: HashMap<String, Option<String>>,
    /// Cache: Opencode synthetic file_path (`<db>#<sid>`) -> session
    /// directory. Opens of the Opencode SQLite DB are amortised across
    /// repeated project-filter rebuilds; caches both `Some` and `None`.
    opencode_cwd_cache: HashMap<String, Option<String>>,
    /// All search paths (for "all sessions" mode)
    pub(crate) all_search_paths: Vec<String>,
    /// Search path(s) for current project only
    pub current_project_paths: Vec<String>,
    /// Canonicalized cwd of the ccs process. Used to match Codex sessions
    /// (their cwd is stored inside the session file, not in the file path).
    pub current_cwd: Option<String>,
    /// Recent sessions sub-state
    pub recent: RecentState,
    /// Ctrl+G agent search sub-state
    pub ai: AiState,
    /// ccs binary the Ctrl+G agent calls, resolved once at startup so an
    /// in-place upgrade does not change it under a running TUI.
    pub(crate) ccs_path: Result<PathBuf, String>,
    /// Picker mode: on_enter sets outcome to Pick instead of Resume
    pub picker_mode: bool,
    /// Last known tree area visible height (set from frame after draw, used for scroll calculations)
    pub last_tree_visible_height: usize,
    /// Sessions resumed from this picker that ran, marked in the result lists
    pub opened_sessions: HashSet<String>,
}

impl App {
    pub fn new(search_paths: Vec<String>) -> Self {
        let current_dir = std::env::current_dir().ok();
        Self::new_with_current_dir(search_paths, current_dir.as_deref())
    }

    fn new_with_current_dir(search_paths: Vec<String>, current_dir: Option<&Path>) -> Self {
        // Channel for results from per-request search threads. `result_tx` is
        // promoted to a `SearchState` field (was a local in the legacy
        // worker model) so each spawn-per-request thread in `start_search`
        // can `.clone()` it.
        let (result_tx, result_rx) = mpsc::channel::<BackgroundSearchResult>();

        // Detect current project paths for Ctrl+A filtering. This includes
        // both default roots (`.../projects/<encoded-cwd>`) and already-scoped
        // custom roots (`.../projects/<encoded-cwd>/...`).
        let current_project_paths = derive_current_project_paths(&search_paths, current_dir);

        let all_search_paths = search_paths.clone();

        // Canonicalize once: macOS resolves `/var` → `/private/var`, and dev
        // boxes routinely symlink `~/projects`. Codex stores its `cwd`
        // verbatim, so the side that we control (ccs's own cwd) must match
        // the OS-canonical form. Fall back to the raw path if canonicalize
        // fails (rare; e.g. cwd removed).
        let current_cwd = current_dir.and_then(canonicalize_path_to_string);

        let recent = RecentState::new(search_paths.clone());

        Self {
            input: InputState::new(),
            search: SearchState {
                all_groups: vec![],
                groups: vec![],
                results_count: 0,
                results_query: String::new(),
                group_cursor: 0,
                sub_cursor: 0,
                expanded: false,
                current: None,
                error: None,
                latest_chains: HashMap::new(),
                search_rx: result_rx,
                result_tx,
                search_seq: 0,
                search_truncated: false,
                message_count_rx: None,
                message_count_cancel: None,
            },
            tree: TreeState {
                session_tree: None,
                tree_cursor: 0,
                tree_scroll_offset: 0,
                tree_loading: false,
                tree_load_rx: None,
                tree_mode_standalone: false,
            },
            typing: false,
            search_paths,
            last_query: String::new(),
            last_keystroke: None,
            preview_mode: false,
            should_quit: false,
            outcome: None,
            needs_full_redraw: false,
            regex_mode: false,
            last_regex_mode: false,
            last_search_paths: all_search_paths.clone(),
            last_project_filter: false,
            tree_mode: false,
            project_filter: false,
            automation_filter: AutomationFilter::Manual,
            automation_cache: HashMap::new(),
            opencode_cwd_cache: HashMap::new(),
            all_search_paths,
            current_project_paths,
            current_cwd,
            recent,
            ai: AiState {
                active: false,
                query: InputState::new(),
                search: None,
                progress: Vec::new(),
                hits: None,
                hit_cursor: 0,
                error: None,
            },
            ccs_path: crate::agent_search::resolve_ccs_path(),
            picker_mode: false,
            last_tree_visible_height: 20,
            opened_sessions: HashSet::new(),
        }
    }

    pub(crate) fn can_filter_by_codex_cwd(&self) -> bool {
        self.current_cwd.is_some()
            && self
                .all_search_paths
                .iter()
                .any(|path| search_path_supports_cwd_scope(path))
    }

    /// Build ripgrep roots for Ctrl+A project search. Claude sessions can be
    /// narrowed by encoded project path; Codex and Opencode sessions need
    /// their session-source roots in the path list so the post-search cwd
    /// filter can decide project membership.
    pub(crate) fn project_scoped_search_paths(&self) -> Vec<String> {
        if self.current_project_paths.is_empty() {
            return self.all_search_paths.clone();
        }

        let mut paths = self.current_project_paths.clone();
        if self.can_filter_by_codex_cwd() {
            let extra_paths: Vec<String> = self
                .all_search_paths
                .iter()
                .filter(|path| search_path_supports_cwd_scope(path))
                .filter(|path| !paths.contains(path))
                .cloned()
                .collect();
            paths.extend(extra_paths);
        }
        paths
    }

    /// Create a read-only view of the app state for rendering.
    pub fn view(&self) -> crate::tui::view::AppView<'_> {
        crate::tui::view::AppView(self)
    }

    pub fn on_key(&mut self, c: char) {
        self.input.push_char(c);
        self.typing = true;
        self.last_keystroke = Some(Instant::now());
    }

    fn edit_input(&mut self, edit: fn(&mut InputState) -> bool) {
        if edit(&mut self.input) {
            self.typing = true;
            self.last_keystroke = Some(Instant::now());
        }
    }

    pub fn on_backspace(&mut self) {
        self.edit_input(InputState::backspace);
    }

    pub fn on_delete(&mut self) {
        self.edit_input(InputState::delete_forward);
    }

    /// Determine the outcome of the TUI session based on app state after the loop exits.
    ///
    /// Borrows rather than consumes: the app outlives a resumed session so the
    /// picker can come back in the same state, see `return_from_resume`.
    pub fn take_outcome(&mut self) -> TuiOutcome {
        let outcome = self.outcome.take();
        match outcome {
            Some(AppOutcome::Pick(picked)) => TuiOutcome::Pick(picked),
            Some(AppOutcome::Resume(target)) => {
                ccs_debug!(
                    "[ccs:take_outcome] session_id={}, file_path={}, source={:?}, uuid={:?}",
                    target.session_id,
                    target.file_path,
                    target.source,
                    target.uuid
                );
                self.suspend_for_resume();
                TuiOutcome::Resume {
                    session_id: target.session_id,
                    file_path: target.file_path,
                    source: target.source,
                    uuid: target.uuid,
                }
            }
            None => TuiOutcome::Quit,
        }
    }

    /// Settle pending search work before a resumed session takes the
    /// terminal, as `Drop` did when the app ended there. A debounced query or
    /// an `rg` result landing after the return would otherwise reset the
    /// results and the cursor the user comes back to. Message counting is
    /// left running: it fills counts in place and never moves the cursor.
    fn suspend_for_resume(&mut self) {
        if let Some(h) = self.search.current.take() {
            h.cancel.store(true, Ordering::Relaxed);
        }
        // A running agent would land its hits after the return; its hits
        // from an earlier search stay for the user to come back to.
        self.ai.search = None;
        self.ai.progress.clear();
        self.last_keystroke = None;
        self.typing = false;
        self.last_query = self.input.text().to_string();
    }

    /// Reopen the picker after a resumed session handed the terminal back.
    /// `opened` names the session that actually ran; a failed launch passes
    /// `None` so the session is not marked.
    pub fn return_from_resume(&mut self, opened: Option<&str>) {
        self.should_quit = false;
        self.outcome = None;
        self.needs_full_redraw = true;
        if let Some(session_id) = opened {
            self.opened_sessions.insert(session_id.to_string());
        }
    }

    /// Reset all search result state to idle (no results, no error, no status).
    /// Shared by `clear_input()` (Ctrl-C) and `tick()` (backspace-to-empty).
    fn reset_search_state(&mut self) {
        self.last_query.clear();
        self.search.results_count = 0;
        self.search.search_truncated = false;
        self.search.all_groups.clear();
        self.search.groups.clear();
        self.search.results_query.clear();
        self.search.group_cursor = 0;
        self.search.sub_cursor = 0;
        self.search.expanded = false;
        self.preview_mode = false;
        self.search.latest_chains.clear();
        if let Some(handle) = self.search.current.take() {
            handle.cancel.store(true, Ordering::Relaxed);
        }
        self.search.error = None;
        if let Some(flag) = self.search.message_count_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
        self.search.message_count_rx = None;
    }

    /// Clear input and reset search state (Ctrl-C behavior)
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.typing = false;
        self.last_keystroke = None;
        self.reset_search_state();
    }

    pub fn move_cursor_left(&mut self) {
        self.input.move_left();
    }

    pub fn move_cursor_right(&mut self) {
        self.input.move_right();
    }

    pub fn move_cursor_word_left(&mut self) {
        self.input.move_word_left();
    }

    pub fn move_cursor_word_right(&mut self) {
        self.input.move_word_right();
    }

    pub fn move_cursor_home(&mut self) {
        self.input.move_home();
    }

    pub fn move_cursor_end(&mut self) {
        self.input.move_end();
    }

    pub fn delete_word_left(&mut self) {
        self.edit_input(InputState::delete_word_left);
    }

    pub fn delete_word_right(&mut self) {
        self.edit_input(InputState::delete_word_right);
    }

    /// Build a `KeyContext` snapshot for `classify_key`.
    pub fn key_context(&self) -> KeyContext {
        KeyContext {
            tree_mode: self.tree_mode,
            input_empty: self.input.is_empty(),
            preview_mode: self.preview_mode,
            in_recent_sessions_mode: self.in_recent_sessions_mode(),
            has_recent_sessions: !self.recent.filtered.is_empty(),
            has_groups: !self.search.groups.is_empty(),
            ai_mode: self.ai.active,
            ai_input_empty: self.ai.query.is_empty(),
            ai_searching: self.ai.searching(),
            ai_has_hits: self.ai.hits.is_some(),
        }
    }

    /// Dispatch a `KeyAction` to the appropriate handler.
    pub fn handle_action(&mut self, action: KeyAction) {
        // While AI mode is active, query edits, Enter and the list keys
        // (see `handle_ai_list_action`) are consumed by AI mode; Esc, Ctrl+G,
        // toggles and Up/Down in the input phase fall through to the regular
        // handlers below.
        if self.ai.active && self.handle_ai_action(&action) {
            return;
        }
        let handled = self.handle_search_nav_action(&action)
            || self.handle_input_edit_action(&action)
            || self.handle_cursor_move_action(&action)
            || self.handle_toggle_action(&action)
            || self.handle_mode_switch_action(&action)
            || self.handle_tree_action(&action);
        if handled {
            return;
        }
        if let KeyAction::Quit = action {
            self.should_quit = true;
        }
        // KeyAction::Noop: do nothing.
    }

    /// Route an action while AI mode is active. Returns true if the action
    /// was consumed; anything else (Up/Down in the input phase, Esc, Ctrl+G,
    /// toggles) falls through to the regular handlers.
    fn handle_ai_action(&mut self, action: &KeyAction) -> bool {
        if self.handle_ai_list_action(action) {
            return true;
        }
        self.handle_ai_query_edit(action) || self.handle_ai_cursor_move(action)
    }

    /// Keys that would act on a session list. While the agent runs they do
    /// nothing; with results on screen they act on the selected hit, never
    /// on the recent/search list hidden behind it.
    fn handle_ai_list_action(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::Enter => self.on_ai_enter(),
            KeyAction::CancelAiSearch => {
                self.ai.search = None;
                self.ai.progress.clear();
            }
            KeyAction::EnterTreeMode | KeyAction::EnterTreeModeRecent if self.ai.searching() => {}
            KeyAction::Up | KeyAction::Down if self.ai.searching() => {}
            // No preview in AI mode: one opened before the hits arrive would
            // cover them, and Enter would open a hit the user cannot see.
            KeyAction::Tab | KeyAction::TogglePreview => {}
            _ if self.ai.hits.is_none() => return false,
            KeyAction::EnterTreeMode | KeyAction::EnterTreeModeRecent | KeyAction::Right => {
                self.enter_tree_mode_for_hit()
            }
            KeyAction::Up => self.ai.hit_cursor = self.ai.hit_cursor.saturating_sub(1),
            KeyAction::Down => {
                let last = self
                    .ai
                    .hits
                    .as_ref()
                    .map_or(0, |h| h.len().saturating_sub(1));
                self.ai.hit_cursor = (self.ai.hit_cursor + 1).min(last);
            }
            _ => return false,
        }
        true
    }

    /// Enter in AI mode: open the selected hit when results are shown,
    /// otherwise start a search (a no-op while one is running).
    fn on_ai_enter(&mut self) {
        if self.ai.searching() {
            return;
        }
        match self.ai.selected_hit() {
            Some(hit) => {
                self.outcome = Some(session_outcome(&hit.session, self.picker_mode));
                self.should_quit = true;
            }
            None => self.submit_ai_query(),
        }
    }

    fn enter_tree_mode_for_hit(&mut self) {
        if let Some(file_path) = self.ai.selected_hit().map(|h| h.session.file_path.clone()) {
            self.enter_tree_mode_for_file(&file_path);
        }
    }

    /// AI query mutations. Any change to the buffer stops a running agent
    /// and drops shown hits and errors, so Enter searches for the new text.
    /// Returns true if the action was consumed.
    fn handle_ai_query_edit(&mut self, action: &KeyAction) -> bool {
        let changed = match action {
            KeyAction::InputChar(c) => {
                self.ai.query.push_char(*c);
                true
            }
            KeyAction::Backspace => self.ai.query.backspace(),
            KeyAction::Delete => self.ai.query.delete_forward(),
            KeyAction::ClearInput => {
                let had_text = !self.ai.query.is_empty();
                self.ai.query.clear();
                had_text
            }
            KeyAction::DeleteWordLeft => self.ai.query.delete_word_left(),
            KeyAction::DeleteWordRight => self.ai.query.delete_word_right(),
            _ => return false,
        };
        if changed {
            self.ai.reset_search();
        }
        true
    }

    /// AI query cursor movement — keeps a running agent and shown hits.
    /// Returns true if the action was consumed.
    fn handle_ai_cursor_move(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::MoveWordLeft => self.ai.query.move_word_left(),
            KeyAction::MoveWordRight => self.ai.query.move_word_right(),
            KeyAction::MoveHome => self.ai.query.move_home(),
            KeyAction::MoveEnd => self.ai.query.move_end(),
            KeyAction::Left => self.ai.query.move_left(),
            KeyAction::Right => self.ai.query.move_right(),
            _ => return false,
        }
        true
    }

    /// Search mode: result-list navigation. Returns true if handled.
    fn handle_search_nav_action(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::Up => self.on_up(),
            KeyAction::Down => self.on_down(),
            KeyAction::Left => self.on_left(),
            KeyAction::Right => self.on_right(),
            KeyAction::Tab => self.on_tab(),
            KeyAction::Enter => self.on_enter(),
            _ => return false,
        }
        true
    }

    /// Search mode: input editing. Returns true if handled.
    fn handle_input_edit_action(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::InputChar(c) => self.on_key(*c),
            KeyAction::Backspace => self.on_backspace(),
            KeyAction::Delete => self.on_delete(),
            KeyAction::ClearInput => self.clear_input(),
            KeyAction::DeleteWordLeft => self.delete_word_left(),
            KeyAction::DeleteWordRight => self.delete_word_right(),
            _ => return false,
        }
        true
    }

    /// Search mode: cursor movement within the input. Returns true if handled.
    fn handle_cursor_move_action(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::MoveWordLeft => self.move_cursor_word_left(),
            KeyAction::MoveWordRight => self.move_cursor_word_right(),
            KeyAction::MoveHome => self.move_cursor_home(),
            KeyAction::MoveEnd => self.move_cursor_end(),
            _ => return false,
        }
        true
    }

    /// Search mode: filter and preview toggles. Returns true if handled.
    fn handle_toggle_action(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::ToggleRegex => self.on_toggle_regex(),
            KeyAction::ToggleProjectFilter => self.toggle_project_filter(),
            KeyAction::ToggleAutomationFilter => self.toggle_automation_filter(),
            KeyAction::TogglePreview => self.on_tab(),
            KeyAction::ExitPreview => self.preview_mode = false,
            _ => return false,
        }
        true
    }

    /// AI and tree mode entry/exit. Returns true if handled.
    fn handle_mode_switch_action(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::EnterAiMode => self.enter_ai_mode(),
            KeyAction::ExitAiMode => self.exit_ai_mode(),
            KeyAction::EnterTreeMode => self.enter_tree_mode(),
            KeyAction::EnterTreeModeRecent => self.enter_tree_mode_recent(),
            _ => return false,
        }
        true
    }

    /// Tree mode: navigation and selection. Returns true if handled.
    fn handle_tree_action(&mut self, action: &KeyAction) -> bool {
        match action {
            KeyAction::TreeUp => self.on_up_tree(),
            KeyAction::TreeDown => self.on_down_tree(),
            KeyAction::TreeLeft => self.on_left_tree(),
            KeyAction::TreeRight => self.on_right_tree(),
            KeyAction::TreeTab => self.on_tab_tree(),
            KeyAction::TreeEnter => self.on_enter_tree(),
            KeyAction::ExitTreeMode => self.exit_tree_mode(),
            _ => return false,
        }
        true
    }

    pub fn tick(&mut self) {
        // The event loop ticks once more after Enter or Esc set `should_quit`.
        // Background results landing then would replace the screen the user
        // just chose from, which a quick return from a resumed session shows.
        if self.should_quit {
            return;
        }
        self.poll_recent_loads();
        self.poll_tree_load();
        self.poll_message_counts();
        self.poll_ai_result();

        // Check for search results from background threads
        while let Ok(result) = self.search.search_rx.try_recv() {
            self.handle_search_result(result);
        }

        self.process_debounce();
    }

    /// Poll the recent-session background loaders and refresh the filtered
    /// views.
    fn poll_recent_loads(&mut self) {
        let (global_loaded, project_loaded) = self.recent.poll();
        if global_loaded {
            apply_recent_automation_to_groups(
                &mut self.search.all_groups,
                &self.recent.all,
                &mut self.automation_cache,
            );
            self.apply_groups_filter();
            self.apply_recent_sessions_filter();
        }
        if project_loaded {
            self.apply_recent_sessions_filter();
        }
    }

    /// Poll the background tree loader and install the result.
    fn poll_tree_load(&mut self) {
        let Some(ref rx) = self.tree.tree_load_rx else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        match result {
            Ok(tree) => {
                self.tree.session_tree = Some(tree);
                self.tree.tree_loading = false;
                self.needs_full_redraw = true;
            }
            Err(e) => {
                self.search.error = Some(format!("Tree load error: {}", e));
                self.tree.tree_loading = false;
                self.tree_mode = false;
                self.needs_full_redraw = true;
            }
        }
        self.tree.tree_load_rx = None;
    }

    /// Drain background message counts. Update the count in both the
    /// `all_groups` store and the currently-filtered `groups` view in
    /// place — a `message_count` update never changes which groups pass
    /// the automation filter, so cloning `all_groups` via
    /// `apply_groups_filter` on every trickled update is pure waste. In
    /// debug builds that clone was dominating per-keystroke latency when
    /// scrolling while background count workers were still finishing.
    fn poll_message_counts(&mut self) {
        let Some(ref rx) = self.search.message_count_rx else {
            return;
        };
        while let Ok((file_path, count, compacted)) = rx.try_recv() {
            set_group_message_count(&mut self.search.all_groups, &file_path, count, compacted);
            set_group_message_count(&mut self.search.groups, &file_path, count, compacted);
        }
    }

    /// Drain the agent's events: progress updates, then its final answer.
    fn poll_ai_result(&mut self) {
        loop {
            let Some(search) = &self.ai.search else {
                return;
            };
            match search.rx.try_recv() {
                Ok(AgentEvent::Progress(queries)) => self.ai.progress = queries,
                Ok(AgentEvent::Done(result)) => {
                    self.ai.search = None;
                    self.ai.progress.clear();
                    match result {
                        Ok(hits) => {
                            self.ai.hits = Some(hits);
                            self.ai.hit_cursor = 0;
                            self.preview_mode = false;
                        }
                        Err(e) => self.ai.error = Some(e),
                    }
                    return;
                }
                Err(TryRecvError::Empty) => return,
                // The handle is still installed, so the TUI did not cancel
                // it: the agent's thread ended without an answer.
                Err(TryRecvError::Disconnected) => {
                    self.ai.search = None;
                    self.ai.progress.clear();
                    self.ai.error = Some("agent stopped unexpectedly".to_string());
                    return;
                }
            }
        }
    }

    /// Fire the debounced search once the debounce period has elapsed.
    fn process_debounce(&mut self) {
        let Some(last) = self.last_keystroke else {
            return;
        };
        if last.elapsed() < Duration::from_millis(DEBOUNCE_MS) {
            return;
        }
        self.last_keystroke = None;
        self.typing = false;
        self.apply_debounced_search();
    }

    /// Re-search if the query, regex mode, or search scope changed; reset
    /// to the idle state when the user backspaced to empty.
    fn apply_debounced_search(&mut self) {
        let query_changed = self.input.text() != self.last_query;
        let mode_changed = self.regex_mode != self.last_regex_mode;
        let scope_changed = self.search_paths != self.last_search_paths
            || self.project_filter != self.last_project_filter;
        if query_changed && self.input.is_empty() {
            // User backspaced to empty — reset to idle state
            self.reset_search_state();
        } else if !self.input.is_empty() && (query_changed || mode_changed || scope_changed) {
            self.start_search();
        }
    }

    /// Process a background search result. Returns silently (no state
    /// mutation) if no handle is in flight, or if the result's `seq` does
    /// not match the current handle's `seq`. Otherwise consumes `current`,
    /// stores the matches into `groups`, and clears the searching state.
    ///
    /// The handle is the single source of truth: a result is fresh iff
    /// its seq matches the current request's seq. Newer requests cancel
    /// older ones before they can deliver, so any received result with
    /// `seq == current.seq` is by construction consistent with the
    /// request that produced it — secondary checks on query / paths /
    /// use_regex are redundant and were removed because they could
    /// strand the prior `searching: bool` flag at `true`.
    pub(crate) fn handle_search_result(
        &mut self,
        BackgroundSearchResult { seq, query, result }: BackgroundSearchResult,
    ) {
        let Some(current) = self.search.current.as_ref() else {
            return;
        };
        if seq != current.seq {
            return;
        }
        self.search.current = None;

        match result {
            Ok(search_result) => {
                self.search.results_query = query;
                let count = search_result.matches.len();
                self.search.search_truncated = search_result.truncated;
                let mut groups = group_by_session(search_result.matches);
                apply_recent_automation_to_groups(
                    &mut groups,
                    &self.recent.all,
                    &mut self.automation_cache,
                );
                self.search.all_groups = groups;
                self.apply_groups_filter();
                self.search.results_count = count;
                self.search.group_cursor = 0;
                self.search.sub_cursor = 0;
                self.search.expanded = false;
                self.search.error = None;
                self.search.latest_chains.clear();

                // Spawn background thread to count total messages per session file
                if let Some(flag) = self.search.message_count_cancel.take() {
                    flag.store(true, Ordering::Relaxed);
                }
                let file_paths: Vec<String> = self
                    .search
                    .all_groups
                    .iter()
                    .map(|g| g.file_path.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                if !file_paths.is_empty() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let cancel = Arc::new(AtomicBool::new(false));
                    let cancel_clone = cancel.clone();
                    std::thread::spawn(move || {
                        for fp in file_paths {
                            if cancel_clone.load(Ordering::Relaxed) {
                                break;
                            }
                            let (msg_count, compacted) = crate::search::count_session_messages(&fp);
                            if tx.send((fp, msg_count, compacted)).is_err() {
                                break;
                            }
                        }
                    });
                    self.search.message_count_rx = Some(rx);
                    self.search.message_count_cancel = Some(cancel);
                }
            }
            Err(e) => {
                self.search.error = Some(e);
                self.search.search_truncated = false;
            }
        }
    }

    pub(crate) fn apply_recent_sessions_filter(&mut self) {
        self.recent.apply_filter(
            self.project_filter,
            &self.current_project_paths,
            self.current_cwd.as_deref(),
            &self.automation_filter,
        );
    }

    /// Rebuild `groups` from `all_groups` based on project and automation filters.
    pub(crate) fn apply_groups_filter(&mut self) {
        // Split-borrow: `all_groups` is read while `opencode_cwd_cache`
        // is mutated by `group_matches_project`. Bind each field of `self`
        // to its own local so the borrow checker tracks them independently.
        let project_filter = self.project_filter;
        let automation_filter = self.automation_filter;
        let project_paths = &self.current_project_paths;
        let current_cwd = self.current_cwd.as_deref();
        let opencode_cwd_cache = &mut self.opencode_cwd_cache;
        let all_groups = &self.search.all_groups;

        let new_groups: Vec<SessionGroup> = all_groups
            .iter()
            .filter(|g| {
                !project_filter
                    || group_matches_project(g, project_paths, current_cwd, opencode_cwd_cache)
            })
            .filter(|g| match automation_filter {
                AutomationFilter::All => true,
                AutomationFilter::Manual => g.automation.is_none(),
                AutomationFilter::Auto => g.automation.is_some(),
            })
            .cloned()
            .collect();

        self.search.groups = new_groups;
        // Clamp cursor so it stays valid after the filtered list shrinks
        // (e.g. async automation metadata arrives while Manual/Auto filter is active).
        if self.search.groups.is_empty() {
            self.search.group_cursor = 0;
            self.search.sub_cursor = 0;
            self.search.expanded = false;
        } else if self.search.group_cursor >= self.search.groups.len() {
            self.search.group_cursor = self.search.groups.len() - 1;
            self.search.sub_cursor = 0;
            self.search.expanded = false;
        }
    }

    // -- AI mode methods --

    fn enter_ai_mode(&mut self) {
        self.ai.active = true;
        self.ai.query.clear();
        self.ai.reset_search();
        // Drop preview: otherwise it covers the agent's results.
        self.preview_mode = false;
    }

    pub fn exit_ai_mode(&mut self) {
        self.ai.active = false;
        self.ai.reset_search();
    }

    fn submit_ai_query(&mut self) {
        if self.ai.query.is_empty() || self.ai.searching() {
            return;
        }
        let query = self.ai.query.text().to_string();
        match crate::config::load_default_config() {
            Ok(config) => {
                let claude = which::which("claude")
                    .map_err(|_| "Claude binary not found in PATH".to_string());
                self.start_agent_search(query, claude, config.agent_search);
            }
            Err(e) => self.ai.error = Some(e),
        }
    }

    /// Start the agent for `query`. Kept separate from `submit_ai_query` so
    /// tests can pass the binary and config instead of resolving them.
    fn start_agent_search(
        &mut self,
        query: String,
        claude: Result<PathBuf, String>,
        cfg: AgentSearchConfig,
    ) {
        self.ai.reset_search();
        let claude = match claude {
            Ok(path) => path,
            Err(e) => {
                self.ai.error = Some(e);
                return;
            }
        };
        let ccs = match &self.ccs_path {
            Ok(path) => path.clone(),
            Err(e) => {
                self.ai.error = Some(e.clone());
                return;
            }
        };
        self.ai.search = Some(crate::agent_search::spawn_agent_search(
            query, claude, ccs, cfg,
        ));
    }

    /// Start an async search by spawning a dedicated per-request thread.
    ///
    /// Cancels any prior in-flight handle (cooperative + forced via the
    /// ripgrep wrapper's `child.kill()` on the next line iteration) before
    /// installing the new handle. The result is dispatched via
    /// `search.result_tx`; `tick()` polls `search.search_rx` and routes
    /// matching-`seq` results through `handle_search_result`.
    ///
    /// Must be called from the UI thread; concurrent calls would race on
    /// `search.current`.
    pub(crate) fn start_search(&mut self) {
        self.search.search_seq += 1;
        let seq = self.search.search_seq;
        let query = self.input.text().to_string();
        self.last_query = query.clone();
        self.last_regex_mode = self.regex_mode;
        self.last_search_paths = self.search_paths.clone();
        self.last_project_filter = self.project_filter;
        self.search.search_truncated = false;

        if let Some(prev) = self.search.current.take() {
            prev.cancel.store(true, Ordering::Relaxed);
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = cancel.clone();
        let tx = self.search.result_tx.clone();
        let paths = self.search_paths.clone();
        let use_regex = self.regex_mode;

        thread::spawn(move || {
            let result = search_multiple_paths(&query, &paths, use_regex, &cancel_worker);
            // The race window between the final `cancel.load()` inside
            // ripgrep and this `send` is closed by the seq discriminator
            // in `handle_search_result`: a stale result that slips through
            // here will be dropped on arrival.
            if !cancel_worker.load(Ordering::Relaxed) {
                let _ = tx.send(BackgroundSearchResult { seq, query, result });
            }
        });

        self.search.current = Some(SearchHandle { seq, cancel });
    }

    /// Whether a search request is currently in flight.
    /// Derived from `search.current`, replacing the prior stored
    /// `searching: bool` flag.
    pub fn is_searching(&self) -> bool {
        self.search.current.is_some()
    }

    /// Test-only helper to install or clear an in-flight search handle.
    /// Used by external integration tests (e.g. render snapshots) to
    /// exercise the "Searching…" status without spawning a real thread.
    /// `SearchHandle` itself is `pub(crate)`, so external (integration
    /// test) callers cannot construct one directly — this helper bridges
    /// that gap.
    #[doc(hidden)]
    pub fn set_searching_for_test(&mut self, searching: bool) {
        if searching {
            self.search.current = Some(SearchHandle {
                seq: self.search.search_seq,
                cancel: Arc::new(AtomicBool::new(false)),
            });
        } else if let Some(h) = self.search.current.take() {
            h.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// On TUI shutdown (Quit / Resume / Pick / panic / unwind), cancel any
/// in-flight search so the per-request worker thread and its `rg` child
/// exit instead of running to completion in the background. Without this
/// drop, exiting the TUI mid-search leaves an orphaned `rg` process and a
/// worker thread blocked in `read_line()` until ripgrep finishes naturally
/// — wasting CPU and battery on a result no one will read. The watchdog
/// thread (in the ripgrep wrapper) observes this flag and kills `rg`
/// within ~50 ms.
impl Drop for App {
    fn drop(&mut self) {
        if let Some(h) = self.search.current.take() {
            h.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(flag) = self.search.message_count_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Message, RipgrepMatch};
    use chrono::{DateTime, Utc};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_recent_session(file_path: &str) -> RecentSession {
        RecentSession {
            session_id: file_path.to_string(),
            file_path: file_path.to_string(),
            project: "proj".to_string(),
            source: SessionSource::CLI,
            timestamp: Utc::now(),
            summary: "summary".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
            cwd: None,
        }
    }

    /// An agent hit for the session file `file_path`.
    fn found(file_path: &str) -> FoundSession {
        FoundSession {
            session: make_recent_session(file_path),
            why: format!("why {file_path}"),
        }
    }

    fn make_group(session_id: &str, file_path: &str) -> SessionGroup {
        SessionGroup {
            session_id: session_id.to_string(),
            file_path: file_path.to_string(),
            matches: vec![RipgrepMatch {
                file_path: file_path.to_string(),
                message: Some(Message {
                    session_id: session_id.to_string(),
                    role: "user".to_string(),
                    content: "needle".to_string(),
                    timestamp: Utc::now(),
                    line_number: 1,
                    ..Default::default()
                }),
                source: SessionSource::from_path(file_path),
            }],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }
    }

    fn write_codex_session(path: &std::path::Path, session_id: &str, cwd: &std::path::Path) {
        let parent = path
            .parent()
            .expect("codex session path should have parent");
        std::fs::create_dir_all(parent).unwrap();
        let line = serde_json::json!({
            "timestamp": "2026-05-03T10:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "cwd": cwd.to_str().unwrap(),
                "source": "cli"
            }
        })
        .to_string();
        std::fs::write(path, format!("{line}\n")).unwrap();
    }

    #[test]
    fn test_app_new() {
        let app = App::new(vec!["/test/path".to_string()]);

        assert_eq!(app.search_paths, vec!["/test/path".to_string()]);
        assert!(app.input.is_empty());
        assert!(app.search.groups.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn test_app_new_detects_direct_claude_project_search_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let encoded = encode_path_for_claude(repo.to_str().unwrap());
        let project_root = dir.path().join("projects").join(encoded);
        std::fs::create_dir_all(&project_root).unwrap();
        let project_root = project_root.to_string_lossy().to_string();

        let app = App::new_with_current_dir(vec![project_root.clone()], Some(&repo));

        assert_eq!(app.current_project_paths, vec![project_root]);
    }

    #[test]
    fn test_app_new_detects_scoped_claude_project_search_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let encoded = encode_path_for_claude(repo.to_str().unwrap());
        let scoped_root = dir
            .path()
            .join("projects")
            .join(encoded)
            .join("session-scope");
        std::fs::create_dir_all(&scoped_root).unwrap();
        let scoped_root = scoped_root.to_string_lossy().to_string();

        let app = App::new_with_current_dir(vec![scoped_root.clone()], Some(&repo));

        assert_eq!(app.current_project_paths, vec![scoped_root]);
    }

    #[test]
    fn test_app_initializes_with_empty_recent_sessions() {
        let app = App::new(vec!["/nonexistent/path".to_string()]);
        assert!(app.recent.filtered.is_empty());
        assert_eq!(app.recent.cursor, 0);
        assert!(app.recent.loading);
        assert!(app.recent.load_rx.is_some());
    }

    #[test]
    fn test_app_receives_recent_sessions_from_background() {
        // Use a temp dir with a real JSONL file so the background thread finds something
        let dir = tempfile::TempDir::new().unwrap();
        let proj_dir = dir.path().join("-Users-user-proj");
        std::fs::create_dir_all(&proj_dir).unwrap();
        let session_file = proj_dir.join("sess1.jsonl");
        std::fs::write(
            &session_file,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello world"}]},"sessionId":"sess-1","timestamp":"2025-06-01T10:00:00Z"}"#,
        )
        .unwrap();

        let mut app = App::new(vec![dir.path().to_str().unwrap().to_string()]);

        // Poll tick() until recent sessions arrive (with timeout)
        let start = Instant::now();
        while app.recent.loading && start.elapsed() < Duration::from_secs(5) {
            app.tick();
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(!app.recent.loading);
        assert!(app.recent.load_rx.is_none());
        assert_eq!(app.recent.filtered.len(), 1);
        assert_eq!(app.recent.filtered[0].session_id, "sess-1");
        assert_eq!(app.recent.filtered[0].summary, "hello world");
    }

    #[test]
    fn test_apply_recent_sessions_filter_matches_mixed_separators() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.project_filter = true;
        app.current_project_paths = vec![r"C:/Users/test/project".to_string()];
        app.recent.all = vec![
            make_recent_session(r"C:\Users\test\project\session.jsonl"),
            make_recent_session(r"C:\Users\test\project-other\session.jsonl"),
        ];

        app.apply_recent_sessions_filter();

        assert_eq!(app.recent.filtered.len(), 1);
        assert_eq!(
            app.recent.filtered[0].file_path,
            r"C:\Users\test\project\session.jsonl"
        );
    }

    fn claude_session_in_project() -> RecentSession {
        RecentSession {
            session_id: "claude-1".to_string(),
            file_path: "/proj/-Users-x-y/abc.jsonl".to_string(),
            project: "y".to_string(),
            source: SessionSource::CLI,
            timestamp: Utc::now(),
            summary: "claude summary".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
            cwd: None,
        }
    }

    fn codex_session_with_cwd(session_id: &str, cwd: &str) -> RecentSession {
        RecentSession {
            session_id: session_id.to_string(),
            file_path: format!(
                "/home/u/.codex/sessions/2026/05/01/rollout-2026-05-01T10-00-00-{}.jsonl",
                session_id
            ),
            project: "codex-proj".to_string(),
            source: SessionSource::CLI,
            timestamp: Utc::now(),
            summary: "codex summary".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
            cwd: Some(cwd.to_string()),
        }
    }

    #[test]
    fn test_apply_filter_keeps_codex_in_all_after_claude_project_load() {
        let claude = claude_session_in_project();
        let codex = codex_session_with_cwd("codex-1", "/proj");

        let mut state = RecentState::new(vec!["/test".to_string()]);
        state.all = vec![codex.clone()];
        state.project = Some(vec![claude.clone()]);

        state.apply_filter(
            true,
            &["/proj/-Users-x-y".to_string()],
            Some("/proj"),
            &AutomationFilter::All,
        );

        assert_eq!(state.filtered.len(), 2, "expected union of Claude+Codex");
        let session_ids: Vec<&str> = state
            .filtered
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert!(
            session_ids.contains(&"codex-1"),
            "Codex session should remain in filtered output, got {:?}",
            session_ids
        );
        assert!(
            session_ids.contains(&"claude-1"),
            "Claude session should remain in filtered output, got {:?}",
            session_ids
        );
    }

    #[test]
    fn test_apply_filter_drops_codex_with_unrelated_cwd() {
        let codex = codex_session_with_cwd("codex-1", "/other-proj");

        let mut state = RecentState::new(vec!["/test".to_string()]);
        state.all = vec![codex];
        state.project = Some(vec![]);

        state.apply_filter(true, &[], Some("/proj"), &AutomationFilter::All);

        assert!(
            state.filtered.is_empty(),
            "Codex session with unrelated cwd must be filtered out"
        );
    }

    #[test]
    fn test_apply_filter_sorts_union_by_timestamp_desc() {
        let mut claude_old = claude_session_in_project();
        claude_old.timestamp = DateTime::parse_from_rfc3339("2026-05-07T16:23:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut codex_new = codex_session_with_cwd("codex-new", "/proj");
        codex_new.timestamp = DateTime::parse_from_rfc3339("2026-05-11T18:25:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut codex_mid = codex_session_with_cwd("codex-mid", "/proj");
        codex_mid.timestamp = DateTime::parse_from_rfc3339("2026-05-11T18:21:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut state = RecentState::new(vec!["/test".to_string()]);
        state.all = vec![codex_new, codex_mid];
        state.project = Some(vec![claude_old]);

        state.apply_filter(
            true,
            &["/proj/-Users-x-y".to_string()],
            Some("/proj"),
            &AutomationFilter::All,
        );

        let ids: Vec<&str> = state
            .filtered
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["codex-new", "codex-mid", "claude-1"],
            "union must be sorted by timestamp desc (newest first)"
        );
    }

    #[test]
    fn test_apply_filter_dedupes_codex_already_in_project() {
        let codex_in_project = codex_session_with_cwd("codex-1", "/proj");
        let codex_in_all = codex_session_with_cwd("codex-1", "/proj");

        let mut state = RecentState::new(vec!["/test".to_string()]);
        state.all = vec![codex_in_all];
        state.project = Some(vec![codex_in_project]);

        state.apply_filter(true, &[], Some("/proj"), &AutomationFilter::All);

        assert_eq!(
            state.filtered.len(),
            1,
            "duplicate Codex sessions (same session_id) must dedupe"
        );
    }

    #[test]
    fn test_total_count_includes_codex_in_union() {
        let claude = claude_session_in_project();
        let codex = codex_session_with_cwd("codex-1", "/proj");

        let mut state = RecentState::new(vec!["/test".to_string()]);
        state.all = vec![codex];
        state.project = Some(vec![claude]);

        assert_eq!(
            state.total_count(true),
            2,
            "total_count must reflect union of Claude project + Codex from all"
        );
    }

    #[test]
    fn test_total_count_no_double_count_for_codex_already_in_project() {
        let codex_a = codex_session_with_cwd("codex-1", "/proj");
        let codex_b = codex_session_with_cwd("codex-1", "/proj");

        let mut state = RecentState::new(vec!["/test".to_string()]);
        state.all = vec![codex_a];
        state.project = Some(vec![codex_b]);

        assert_eq!(
            state.total_count(true),
            1,
            "duplicate Codex sessions must not be double-counted"
        );
    }

    #[test]
    fn test_handle_search_result_reuses_recent_session_automation() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text("later");
        app.search.search_seq = 1;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.recent.all = vec![RecentSession {
            session_id: "auto-session".to_string(),
            file_path: "/sessions/auto-session.jsonl".to_string(),
            project: "proj".to_string(),
            source: SessionSource::CLI,
            timestamp: Utc::now(),
            summary: "summary".to_string(),
            automation: Some("ralphex".to_string()),
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
            cwd: None,
        }];

        let result = RipgrepMatch {
            file_path: "/sessions/agent-123.jsonl".to_string(),
            message: Some(Message {
                session_id: "auto-session".to_string(),
                role: "assistant".to_string(),
                content: "Later answer".to_string(),
                timestamp: Utc::now(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::CLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "later".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![result],
                truncated: false,
            }),
        });

        assert_eq!(app.search.all_groups.len(), 1);
        assert_eq!(
            app.search.all_groups[0].automation,
            Some("ralphex".to_string())
        );
    }

    #[test]
    fn test_handle_search_result_detects_automation_outside_recent_sessions() {
        let mut session_file = NamedTempFile::new().unwrap();
        writeln!(session_file, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"When done output <<<RALPHEX:ALL_TASKS_DONE>>>"}}]}},"sessionId":"old-auto","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(session_file, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Automation reply"}}]}},"sessionId":"old-auto","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text("reply");
        app.search.search_seq = 1;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.automation_filter = AutomationFilter::Auto;

        let result = RipgrepMatch {
            file_path: session_file.path().to_string_lossy().to_string(),
            message: Some(Message {
                session_id: "old-auto".to_string(),
                role: "assistant".to_string(),
                content: "Automation reply".to_string(),
                timestamp: Utc::now(),
                line_number: 2,
                ..Default::default()
            }),
            source: SessionSource::CLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "reply".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![result],
                truncated: false,
            }),
        });

        assert_eq!(app.search.all_groups.len(), 1);
        assert_eq!(
            app.search.all_groups[0].automation,
            Some("ralphex".to_string())
        );
        assert_eq!(app.search.groups.len(), 1);
    }

    #[test]
    fn test_handle_search_result_ignores_later_quoted_automation_markers() {
        let mut session_file = NamedTempFile::new().unwrap();
        writeln!(session_file, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"How can I distinguish ralphex transcripts from manual sessions?"}}]}},"sessionId":"manual-session","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(session_file, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Let's inspect the markers."}}]}},"sessionId":"manual-session","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();
        writeln!(session_file, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"такие тоже надо детектить <scheduled-task name=\"chezmoi-sync\">"}}]}},"sessionId":"manual-session","timestamp":"2025-06-01T10:02:00Z"}}"#).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text("detekt");
        app.search.search_seq = 1;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });

        let result = RipgrepMatch {
            file_path: session_file.path().to_string_lossy().to_string(),
            message: Some(Message {
                session_id: "manual-session".to_string(),
                role: "user".to_string(),
                content: r#"такие тоже надо детектить <scheduled-task name="chezmoi-sync">"#
                    .to_string(),
                timestamp: Utc::now(),
                line_number: 3,
                ..Default::default()
            }),
            source: SessionSource::CLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "detekt".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![result],
                truncated: false,
            }),
        });

        assert_eq!(app.search.all_groups.len(), 1);
        assert_eq!(app.search.all_groups[0].automation, None);
    }

    #[test]
    fn test_search_truncated_clears_on_non_truncated_result() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.search_seq = 1;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.input.set_text("query");
        app.regex_mode = false;

        // First result: truncated
        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "query".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![],
                truncated: true,
            }),
        });
        assert!(app.search.search_truncated);

        // Second result: not truncated — flag must clear
        app.search.search_seq = 2;
        app.search.current = Some(SearchHandle {
            seq: 2,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.handle_search_result(BackgroundSearchResult {
            seq: 2,
            query: "query".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![],
                truncated: false,
            }),
        });
        assert!(!app.search.search_truncated);
    }

    #[test]
    fn test_search_path_supports_custom_codex_home_scope() {
        let _lock = crate::TEST_ENV_MUTEX.lock().unwrap();
        let prev_codex = std::env::var("CODEX_HOME").ok();
        unsafe { std::env::set_var("CODEX_HOME", "/tmp/custom-codex") };

        assert!(search_path_supports_codex_cwd_scope("/tmp/custom-codex"));
        assert!(search_path_supports_codex_cwd_scope(
            "/tmp/custom-codex/sessions/2026/05/01"
        ));
        assert!(search_path_supports_codex_cwd_scope(
            "/tmp/custom-codex/archived_sessions"
        ));
        assert!(!search_path_supports_codex_cwd_scope(
            "/tmp/custom-codexish/sessions"
        ));

        unsafe { std::env::remove_var("CODEX_HOME") };
        if let Some(v) = prev_codex {
            unsafe { std::env::set_var("CODEX_HOME", v) };
        }
    }

    fn make_session_with_cwd(file_path: &str, cwd: Option<&str>) -> RecentSession {
        RecentSession {
            session_id: file_path.to_string(),
            file_path: file_path.to_string(),
            project: "proj".to_string(),
            source: SessionSource::CLI,
            timestamp: Utc::now(),
            summary: "summary".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
            cwd: cwd.map(String::from),
        }
    }

    #[test]
    fn test_session_matches_project_same_cwd() {
        let session = make_session_with_cwd(
            "/tmp/.codex/sessions/2026/05/01/rollout.jsonl",
            Some("/repo"),
        );
        assert!(session_matches_project(&session, &[], Some("/repo")));
    }

    #[test]
    fn test_session_matches_project_session_in_subdir_of_current() {
        let session = make_session_with_cwd(
            "/tmp/.codex/sessions/2026/05/01/rollout.jsonl",
            Some("/repo/sub"),
        );
        assert!(session_matches_project(&session, &[], Some("/repo")));
    }

    #[test]
    fn test_session_matches_project_ignores_claude_cwd_without_project_path() {
        let session = make_session_with_cwd("/tmp/claude/rollout.jsonl", Some("/repo"));
        assert!(!session_matches_project(&session, &[], Some("/repo")));
    }

    #[test]
    fn test_session_matches_project_current_in_subdir_of_session_does_not_match() {
        // Was true under symmetric matching. Made false intentionally:
        // symmetric matching falsely included Codex sessions recorded at
        // common ancestors like $HOME for every subdir the user cd's into.
        // Abstract paths /repo and /repo/sub stand in for the
        // $HOME / $HOME/projects/foo case.
        let session = make_session_with_cwd(
            "/tmp/.codex/sessions/2026/05/01/rollout.jsonl",
            Some("/repo"),
        );
        assert!(!session_matches_project(&session, &[], Some("/repo/sub")));
    }

    #[test]
    fn test_session_matches_project_siblings() {
        let session = make_session_with_cwd(
            "/tmp/.codex/sessions/2026/05/01/rollout.jsonl",
            Some("/repo-other"),
        );
        assert!(!session_matches_project(&session, &[], Some("/repo")));
    }

    #[test]
    fn test_session_matches_project_no_cwd_either_side() {
        let session = make_session_with_cwd("/tmp/.codex/sessions/2026/05/01/rollout.jsonl", None);
        assert!(!session_matches_project(&session, &[], None));
    }

    #[test]
    fn test_session_matches_project_opencode_session_in_cwd() {
        // Opencode sessions use synthetic paths `<db>#<session_id>` that
        // never match Claude project_paths. The cwd fallback (previously
        // Codex-only) must now also cover Opencode so the project filter
        // doesn't silently drop them.
        let session = RecentSession {
            session_id: "ses_OC".to_string(),
            file_path: "/tmp/opencode/opencode.db#ses_OC".to_string(),
            project: "proj".to_string(),
            source: SessionSource::CLI,
            timestamp: Utc::now(),
            summary: "s".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
            cwd: Some("/repo".to_string()),
        };
        assert!(session_matches_project(&session, &[], Some("/repo")));
        assert!(!session_matches_project(&session, &[], Some("/repo-other")));
    }

    #[test]
    fn test_project_scoped_search_paths_includes_opencode_db() {
        // When project filter is on, Opencode DB paths must remain in the
        // ripgrep root list — otherwise the SQLite-backed Opencode search
        // never runs and Opencode hits silently drop from project-filtered
        // search results.
        let claude_proj = "/tmp/.claude/projects/-Users-u-proj".to_string();
        let oc_db = "/tmp/opencode/opencode.db".to_string();
        let mut app = App::new(vec![claude_proj.clone(), oc_db.clone()]);
        app.current_project_paths = vec![claude_proj.clone()];
        app.current_cwd = Some("/repo".to_string());

        let scoped = app.project_scoped_search_paths();
        assert!(
            scoped.contains(&claude_proj),
            "claude project path should be in scope: {:?}",
            scoped
        );
        assert!(
            scoped.contains(&oc_db),
            "opencode db must be added to project scope when cwd filter applies: {:?}",
            scoped
        );
    }

    #[test]
    fn test_session_matches_project_only_current_cwd() {
        let session = make_session_with_cwd("/tmp/.codex/sessions/2026/05/01/rollout.jsonl", None);
        assert!(!session_matches_project(&session, &[], Some("/repo")));
    }

    #[test]
    fn test_session_matches_project_only_session_cwd() {
        let session = make_session_with_cwd(
            "/tmp/.codex/sessions/2026/05/01/rollout.jsonl",
            Some("/repo"),
        );
        assert!(!session_matches_project(&session, &[], None));
    }

    #[test]
    fn test_session_matches_project_file_path_match_overrides_cwd() {
        let session = make_session_with_cwd("/proj/-Users-x-y/abc.jsonl", Some("/unrelated"));
        assert!(session_matches_project(
            &session,
            &["/proj/-Users-x-y".to_string()],
            Some("/different"),
        ));
    }

    #[test]
    fn test_session_matches_project_project_paths_no_match_and_cwd_no_match_returns_false() {
        let session = make_session_with_cwd(
            "/tmp/.codex/sessions/2026/05/01/rollout.jsonl",
            Some("/other-project"),
        );
        assert!(!session_matches_project(
            &session,
            &["/proj/-Users-x-y".to_string()],
            Some("/repo"),
        ));
    }

    #[test]
    fn test_apply_groups_filter_applies_project_scope_to_codex_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        let other_repo = dir.path().join("repo-other");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&other_repo).unwrap();

        let claude_project_dir = dir.path().join("projects").join("-Users-test-repo");
        let claude_file = claude_project_dir.join("claude.jsonl");

        let codex_dir = dir.path().join(".codex/sessions/2026/05/03");
        let codex_in = codex_dir.join("rollout-2026-05-03T10-00-00-codex-in.jsonl");
        let codex_out = codex_dir.join("rollout-2026-05-03T10-00-00-codex-out.jsonl");
        write_codex_session(&codex_in, "codex-in", &repo);
        write_codex_session(&codex_out, "codex-out", &other_repo);

        let mut app = App::new(vec![dir.path().to_string_lossy().to_string()]);
        app.project_filter = true;
        app.current_project_paths = vec![claude_project_dir.to_string_lossy().to_string()];
        app.current_cwd = canonicalize_path_to_string(&repo);
        app.search.all_groups = vec![
            make_group("claude", &claude_file.to_string_lossy()),
            make_group("codex-in", &codex_in.to_string_lossy()),
            make_group("codex-out", &codex_out.to_string_lossy()),
        ];

        app.apply_groups_filter();

        let ids = app
            .search
            .groups
            .iter()
            .map(|g| g.session_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["claude", "codex-in"]);
    }

    #[test]
    fn test_handle_search_result_filters_codex_only_project_scope() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        let other_repo = dir.path().join("repo-other");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&other_repo).unwrap();

        let codex_dir = dir.path().join(".codex/sessions/2026/05/03");
        let codex_in = codex_dir.join("rollout-2026-05-03T10-00-00-codex-in.jsonl");
        let codex_out = codex_dir.join("rollout-2026-05-03T10-00-00-codex-out.jsonl");
        write_codex_session(&codex_in, "codex-in", &repo);
        write_codex_session(&codex_out, "codex-out", &other_repo);

        let mut app = App::new(vec![dir.path().to_string_lossy().to_string()]);
        app.project_filter = true;
        app.current_project_paths = vec![];
        app.current_cwd = canonicalize_path_to_string(&repo);
        app.search.search_seq = 1;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "needle".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![
                    make_group("codex-in", &codex_in.to_string_lossy())
                        .matches
                        .remove(0),
                    make_group("codex-out", &codex_out.to_string_lossy())
                        .matches
                        .remove(0),
                ],
                truncated: false,
            }),
        });

        assert_eq!(app.search.all_groups.len(), 2);
        let ids = app
            .search
            .groups
            .iter()
            .map(|g| g.session_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["codex-in"]);
    }

    #[test]
    fn test_app_new_captures_current_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();

        let app = App::new_with_current_dir(vec!["/test".to_string()], Some(dir.path()));

        assert_eq!(
            app.current_cwd.as_deref(),
            canonical.to_str(),
            "App should capture the injected current_cwd"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_app_new_current_cwd_is_canonicalized() {
        let dir = tempfile::TempDir::new().unwrap();
        let real_dir = dir.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        let real_canonical = std::fs::canonicalize(&real_dir).unwrap();

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let app = App::new_with_current_dir(vec!["/test".to_string()], Some(&link));

        let captured = app.current_cwd.clone().expect("current_cwd should be Some");
        assert_eq!(
            captured,
            real_canonical.to_str().unwrap(),
            "current_cwd should be canonicalized (resolve symlinks)"
        );
    }

    #[test]
    fn test_on_key() {
        let mut app = App::new(vec!["/test".to_string()]);

        app.on_key('h');
        app.on_key('e');
        app.on_key('l');
        app.on_key('l');
        app.on_key('o');

        assert_eq!(app.input.text(), "hello");
        assert!(app.typing);
    }

    #[test]
    fn test_on_backspace() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("hello", 5); // cursor at end

        app.on_backspace();

        assert_eq!(app.input.text(), "hell");
        assert_eq!(app.input.cursor_pos(), 4);
    }

    #[test]
    fn test_clear_input_resets_state() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Set up state as if a search has completed
        app.input.set_text_and_cursor("hello", 5);
        app.last_query = "hello".to_string();
        app.search.results_query = "hello".to_string();
        app.search.results_count = 1;
        app.search.groups = vec![SessionGroup {
            session_id: "abc123".to_string(),
            file_path: "/test/file.jsonl".to_string(),
            matches: vec![],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.group_cursor = 1;
        app.search.sub_cursor = 2;
        app.search.expanded = true;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.typing = true;
        app.last_keystroke = Some(Instant::now());
        app.search
            .latest_chains
            .insert("file".to_string(), HashSet::new());
        app.search.error = Some("stale error".to_string());
        app.preview_mode = true;

        app.clear_input();

        assert!(app.input.is_empty(), "input should be cleared");
        assert!(!app.typing, "typing should be false");
        assert!(
            app.last_keystroke.is_none(),
            "last_keystroke should be None"
        );
        assert!(!app.is_searching(), "is_searching should be false");
        assert!(app.last_query.is_empty(), "last_query should be cleared");
        assert_eq!(
            app.search.results_count, 0,
            "results_count should be cleared"
        );
        assert!(app.search.groups.is_empty(), "groups should be cleared");
        assert!(
            app.search.results_query.is_empty(),
            "results_query should be cleared"
        );
        assert_eq!(app.search.group_cursor, 0, "group_cursor should be reset");
        assert_eq!(app.search.sub_cursor, 0, "sub_cursor should be reset");
        assert!(!app.search.expanded, "expanded should be reset");
        assert!(
            app.search.latest_chains.is_empty(),
            "latest_chains should be cleared"
        );
        assert!(app.search.error.is_none(), "error should be cleared");
        assert!(!app.preview_mode, "preview_mode should be reset");
    }

    #[test]
    fn test_ctrl_c_empty_input_should_quit() {
        use crate::tui::dispatch::{classify_key, KeyAction};
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app = App::new(vec!["/test".to_string()]);

        // Empty input — Ctrl-C dispatches to Quit via classify_key
        assert!(app.input.is_empty());
        assert!(!app.should_quit);

        let ctx = app.key_context();
        let key = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let action = classify_key(key, &ctx);
        assert_eq!(action, KeyAction::Quit);
        app.handle_action(action);

        assert!(app.should_quit);
    }

    #[test]
    fn test_ctrl_c_with_input_clears_not_quits() {
        use crate::tui::dispatch::{classify_key, KeyAction};
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app = App::new(vec!["/test".to_string()]);

        app.on_key('t');
        app.on_key('e');
        app.on_key('s');
        app.on_key('t');

        let ctx = app.key_context();
        let key = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let action = classify_key(key, &ctx);
        assert_eq!(action, KeyAction::ClearInput);
        app.handle_action(action);

        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    // In AI mode with an empty AI query, Ctrl+C must exit AI mode (return to
    // search) — NOT quit the TUI. The regression being guarded: the Ctrl+C
    // classifier used to consult `self.input.is_empty()` (main search input)
    // even while AI mode was active, so a user who entered AI mode from a
    // blank search screen and then pressed Ctrl+C would have the TUI killed.
    #[test]
    fn test_ctrl_c_in_ai_mode_empty_query_exits_ai_not_quit() {
        use crate::tui::dispatch::{classify_key, KeyAction};
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        assert!(app.ai.active);
        assert!(app.ai.query.is_empty());

        let ctx = app.key_context();
        let key = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let action = classify_key(key, &ctx);
        assert_eq!(action, KeyAction::ExitAiMode);
        app.handle_action(action);

        assert!(!app.should_quit, "TUI must stay alive on Ctrl+C in AI mode");
        assert!(
            !app.ai.active,
            "Ctrl+C on empty AI query must leave AI mode"
        );
    }

    // In AI mode with a non-empty AI query, Ctrl+C clears the AI query and
    // stays in AI mode — mirroring how Ctrl+C clears the search input in
    // regular search mode. The ClearInput branch also drops shown hits.
    #[test]
    fn test_ctrl_c_in_ai_mode_nonempty_query_clears_stays_in_ai() {
        use crate::tui::dispatch::{classify_key, KeyAction};
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        app.ai.query.set_text("hello");
        app.ai.hits = Some(vec![found("/p/a.jsonl")]);

        let ctx = app.key_context();
        let key = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        let action = classify_key(key, &ctx);
        assert_eq!(action, KeyAction::ClearInput);
        app.handle_action(action);

        assert!(!app.should_quit);
        assert!(
            app.ai.active,
            "Ctrl+C on non-empty AI query must keep AI mode"
        );
        assert!(app.ai.query.is_empty(), "AI query must be cleared");
        assert!(
            app.ai.hits.is_none(),
            "clearing the AI query must drop the shown hits"
        );
    }

    #[test]
    fn test_on_key_inserts_at_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.on_key('a');
        app.on_key('c');
        // input = "ac", cursor at 2
        app.input.set_text_and_cursor("ac", 1); // move cursor between 'a' and 'c'
        app.on_key('b');
        assert_eq!(app.input.text(), "abc");
        assert_eq!(app.input.cursor_pos(), 2);
    }

    #[test]
    fn editable_input_when_deleted_then_restarts_debounce() {
        let edits: [fn(&mut App); 4] = [
            App::on_backspace,
            App::on_delete,
            App::delete_word_left,
            App::delete_word_right,
        ];
        for edit in edits {
            let mut app = App::new(vec!["/test".to_string()]);
            app.input.set_text_and_cursor("abc def", 2);
            let previous = Instant::now() - Duration::from_secs(1);
            app.last_keystroke = Some(previous);
            app.typing = false;

            edit(&mut app);

            assert_ne!(app.input.text(), "abc def");
            assert!(app.typing);
            assert!(app.last_keystroke.unwrap() > previous);
        }
    }

    #[test]
    fn input_at_boundary_when_delete_is_noop_then_preserves_debounce() {
        type Edit = fn(&mut App);
        let edits: [(Edit, usize); 4] = [
            (App::on_backspace, 0),
            (App::on_delete, 3),
            (App::delete_word_left, 0),
            (App::delete_word_right, 3),
        ];
        for (edit, cursor) in edits {
            for typing in [false, true] {
                let mut app = App::new(vec!["/test".to_string()]);
                app.input.set_text_and_cursor("abc", cursor);
                let previous = Instant::now() - Duration::from_secs(1);
                app.last_keystroke = Some(previous);
                app.typing = typing;

                edit(&mut app);

                assert_eq!(app.input.text(), "abc");
                assert_eq!(app.input.cursor_pos(), cursor);
                assert_eq!(app.typing, typing);
                assert_eq!(app.last_keystroke, Some(previous));
            }
        }
    }

    #[test]
    fn test_on_backspace_at_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("abc", 2); // cursor after 'b'
        app.on_backspace();
        assert_eq!(app.input.text(), "ac");
        assert_eq!(app.input.cursor_pos(), 1);
    }

    #[test]
    fn test_on_backspace_at_start_does_nothing() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("abc", 0);
        app.on_backspace();
        assert_eq!(app.input.text(), "abc");
        assert_eq!(app.input.cursor_pos(), 0);
    }

    #[test]
    fn test_on_delete_at_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("abc", 1); // cursor after 'a'
        app.on_delete();
        assert_eq!(app.input.text(), "ac");
        assert_eq!(app.input.cursor_pos(), 1);
    }

    #[test]
    fn test_move_cursor_word_left() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text("hello world foo"); // cursor at end

        app.move_cursor_word_left();
        assert_eq!(app.input.cursor_pos(), 12); // before "foo"

        app.move_cursor_word_left();
        assert_eq!(app.input.cursor_pos(), 6); // before "world"

        app.move_cursor_word_left();
        assert_eq!(app.input.cursor_pos(), 0); // before "hello"

        // At start, stays at 0
        app.move_cursor_word_left();
        assert_eq!(app.input.cursor_pos(), 0);
    }

    #[test]
    fn test_move_cursor_word_right() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("hello world foo", 0);

        app.move_cursor_word_right();
        assert_eq!(app.input.cursor_pos(), 6); // after "hello "

        app.move_cursor_word_right();
        assert_eq!(app.input.cursor_pos(), 12); // after "world "

        app.move_cursor_word_right();
        assert_eq!(app.input.cursor_pos(), 15); // end

        // At end, stays
        app.move_cursor_word_right();
        assert_eq!(app.input.cursor_pos(), 15);
    }

    #[test]
    fn test_delete_word_left() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text("hello world"); // cursor at end

        app.delete_word_left();
        assert_eq!(app.input.text(), "hello ");
        assert_eq!(app.input.cursor_pos(), 6);

        app.delete_word_left();
        assert_eq!(app.input.text(), "");
        assert_eq!(app.input.cursor_pos(), 0);
    }

    #[test]
    fn test_move_cursor_home_end() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("hello", 3);

        app.move_cursor_home();
        assert_eq!(app.input.cursor_pos(), 0);

        app.move_cursor_end();
        assert_eq!(app.input.cursor_pos(), 5);
    }

    #[test]
    fn test_cursor_bounds_empty_input() {
        let mut app = App::new(vec!["/test".to_string()]);

        // All operations on empty input should not panic
        app.move_cursor_left();
        app.move_cursor_right();
        app.move_cursor_word_left();
        app.move_cursor_word_right();
        app.move_cursor_home();
        app.move_cursor_end();
        app.on_backspace();
        app.on_delete();
        app.delete_word_left();

        assert_eq!(app.input.cursor_pos(), 0);
        assert!(app.input.is_empty());
    }

    #[test]
    fn test_move_cursor_left_right() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text("abc"); // cursor at end (3)

        app.move_cursor_left();
        assert_eq!(app.input.cursor_pos(), 2);

        app.move_cursor_left();
        assert_eq!(app.input.cursor_pos(), 1);

        app.move_cursor_right();
        assert_eq!(app.input.cursor_pos(), 2);
    }

    #[test]
    fn test_clear_input_resets_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("hello", 3);

        app.clear_input();

        assert_eq!(app.input.cursor_pos(), 0);
        assert!(app.input.is_empty());
    }

    #[test]
    fn test_delete_word_right() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("hello world foo", 0);

        app.delete_word_right();
        assert_eq!(app.input.text(), "world foo");
        assert_eq!(app.input.cursor_pos(), 0);

        app.delete_word_right();
        assert_eq!(app.input.text(), "foo");
        assert_eq!(app.input.cursor_pos(), 0);

        app.delete_word_right();
        assert_eq!(app.input.text(), "");
        assert_eq!(app.input.cursor_pos(), 0);
    }

    #[test]
    fn test_delete_word_right_at_end_does_nothing() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("hello", 5);

        app.delete_word_right();
        assert_eq!(app.input.text(), "hello");
        assert_eq!(app.input.cursor_pos(), 5);
    }

    #[test]
    fn test_tick_clears_state_when_query_becomes_empty() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Simulate: user had typed "hello", search completed, then backspaced to empty
        app.input.clear(); // empty — user backspaced everything
        app.last_query = "hello".to_string(); // previous query that produced results
        app.search.results_query = "hello".to_string();
        app.search.results_count = 1;
        app.search.groups = vec![SessionGroup {
            session_id: "abc123".to_string(),
            file_path: "/test/file.jsonl".to_string(),
            matches: vec![],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.group_cursor = 1;
        app.search.sub_cursor = 2;
        app.search.expanded = true;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.search
            .latest_chains
            .insert("file".to_string(), HashSet::new());
        app.search.error = Some("stale error".to_string());
        app.preview_mode = true;

        // Set debounce to fire: last keystroke was > DEBOUNCE_MS ago
        app.last_keystroke = Some(Instant::now() - Duration::from_millis(DEBOUNCE_MS + 50));
        app.typing = true;

        app.tick();

        assert_eq!(
            app.search.results_count, 0,
            "results_count should be cleared after tick with empty query"
        );
        assert!(
            app.search.groups.is_empty(),
            "groups should be cleared after tick with empty query"
        );
        assert!(
            app.search.results_query.is_empty(),
            "results_query should be cleared after tick with empty query"
        );
        assert!(
            app.last_query.is_empty(),
            "last_query should be updated to empty"
        );
        assert_eq!(app.search.group_cursor, 0, "group_cursor should be reset");
        assert_eq!(app.search.sub_cursor, 0, "sub_cursor should be reset");
        assert!(!app.search.expanded, "expanded should be reset");
        assert!(!app.typing, "typing should be false after debounce");
        assert!(!app.is_searching(), "is_searching should be false");
        assert!(
            app.search.latest_chains.is_empty(),
            "latest_chains should be cleared"
        );
        assert!(app.search.error.is_none(), "error should be cleared");
        assert!(!app.preview_mode, "preview_mode should be reset");
    }

    #[test]
    fn test_delete_word_right_from_middle() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("hello world", 5); // after "hello", on the space

        // First delete removes " " (skip non-alnum to next word boundary)
        app.delete_word_right();
        assert_eq!(app.input.text(), "helloworld");
        assert_eq!(app.input.cursor_pos(), 5);

        // Second delete removes "world"
        app.delete_word_right();
        assert_eq!(app.input.text(), "hello");
        assert_eq!(app.input.cursor_pos(), 5);
    }

    #[test]
    fn test_picked_session_to_key_value_cli() {
        let picked = PickedSession {
            session_id: "abc-123".to_string(),
            file_path: "/path/to/session.jsonl".to_string(),
            source: SessionSource::CLI,
            project: "my-project".to_string(),
            message_uuid: None,
        };
        let output = picked.to_key_value();
        assert_eq!(
            output,
            "session_id: abc-123\nfile_path: /path/to/session.jsonl\nsource: CLI\nproject: my-project"
        );
    }

    #[test]
    fn test_picked_session_to_key_value_desktop() {
        let picked = PickedSession {
            session_id: "desk-456".to_string(),
            file_path: "/Library/Application Support/Claude/local-agent-mode-sessions/sess.jsonl"
                .to_string(),
            source: SessionSource::ClaudeDesktop,
            project: "desktop-proj".to_string(),
            message_uuid: None,
        };
        let output = picked.to_key_value();
        assert!(output.contains("source: Desktop"));
        assert!(output.contains("session_id: desk-456"));
        assert!(output.contains("project: desktop-proj"));
    }

    #[test]
    fn test_picked_session_write_output_to_file() {
        let picked = PickedSession {
            session_id: "file-out-test".to_string(),
            file_path: "/sessions/test.jsonl".to_string(),
            source: SessionSource::CLI,
            project: "proj".to_string(),
            message_uuid: None,
        };
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        picked.write_output(Some(&path)).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, format!("{}\n", picked.to_key_value()));
    }

    #[test]
    fn test_picked_session_write_output_to_stdout() {
        let picked = PickedSession {
            session_id: "stdout-test".to_string(),
            file_path: "/sessions/test.jsonl".to_string(),
            source: SessionSource::CLI,
            project: "proj".to_string(),
            message_uuid: None,
        };
        // write_output(None) writes to stdout; just verify it doesn't error
        let result = picked.write_output(None);
        assert!(result.is_ok());
    }

    // =========================================================================
    // TuiOutcome tests
    // =========================================================================

    #[test]
    fn test_take_outcome_quit_when_no_selection() {
        let mut app = App::new(vec!["/test".to_string()]);
        assert_eq!(app.take_outcome(), TuiOutcome::Quit);
    }

    #[test]
    fn test_take_outcome_resume_when_outcome_set() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.outcome = Some(AppOutcome::Resume(ResumeTarget {
            session_id: "sess-1".to_string(),
            file_path: "/path/to/session.jsonl".to_string(),
            source: SessionSource::CLI,
            uuid: Some("uuid-42".to_string()),
        }));

        let outcome = app.take_outcome();
        assert_eq!(
            outcome,
            TuiOutcome::Resume {
                session_id: "sess-1".to_string(),
                file_path: "/path/to/session.jsonl".to_string(),
                source: SessionSource::CLI,
                uuid: Some("uuid-42".to_string()),
            }
        );
    }

    #[test]
    fn test_take_outcome_pick_when_outcome_set() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;
        app.outcome = Some(AppOutcome::Pick(PickedSession {
            session_id: "pick-1".to_string(),
            file_path: "/pick/session.jsonl".to_string(),
            source: SessionSource::ClaudeDesktop,
            project: "my-project".to_string(),
            message_uuid: None,
        }));

        let outcome = app.take_outcome();
        assert_eq!(
            outcome,
            TuiOutcome::Pick(PickedSession {
                session_id: "pick-1".to_string(),
                file_path: "/pick/session.jsonl".to_string(),
                source: SessionSource::ClaudeDesktop,
                project: "my-project".to_string(),
                message_uuid: None,
            })
        );
    }

    #[test]
    fn test_take_outcome_quit_when_picker_mode_no_selection() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;
        // No outcome set (user pressed Esc)
        assert_eq!(app.take_outcome(), TuiOutcome::Quit);
    }

    fn app_with_groups_resuming(session_id: &str) -> App {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.all_groups = ["s0", "s1", "s2"]
            .iter()
            .map(|sid| make_group(sid, &format!("/p/{sid}.jsonl")))
            .collect();
        app.search.groups = app.search.all_groups.clone();
        app.input.set_text("foo");
        app.last_query = "foo".to_string();
        app.search.group_cursor = 2;
        app.outcome = Some(AppOutcome::Resume(ResumeTarget {
            session_id: session_id.to_string(),
            file_path: format!("/p/{session_id}.jsonl"),
            source: SessionSource::CLI,
            uuid: None,
        }));
        app.should_quit = true;
        app
    }

    #[test]
    fn search_results_with_filters_when_returning_from_resume_then_state_is_kept() {
        let mut app = app_with_groups_resuming("s2");
        app.regex_mode = true;
        app.ai.active = true;
        app.ai.hits = Some(vec![found("/p/a.jsonl"), found("/p/b.jsonl")]);
        app.ai.hit_cursor = 1;

        let outcome = app.take_outcome();
        app.return_from_resume(Some("s2"));

        assert!(matches!(outcome, TuiOutcome::Resume { ref session_id, .. } if session_id == "s2"));
        assert_eq!(app.input.text(), "foo");
        assert_eq!(app.search.groups.len(), 3);
        assert_eq!(app.search.group_cursor, 2);
        assert!(app.regex_mode);
        assert!(app.ai.active);
        assert_eq!(app.ai.hits.as_ref().map(Vec::len), Some(2));
        assert_eq!(app.ai.hit_cursor, 1);
        assert!(!app.should_quit);
        assert!(app.outcome.is_none());
        assert!(app.needs_full_redraw);
        assert!(app.opened_sessions.contains("s2"));
    }

    #[test]
    fn pending_search_at_enter_when_returning_and_ticking_then_results_and_cursor_are_kept() {
        let mut app = app_with_groups_resuming("s2");
        app.search.expanded = true;
        // Typed "foo2" and pressed Enter before the debounce fired, while the
        // search for "foo" was still running.
        app.input.set_text("foo2");
        app.last_keystroke = Some(Instant::now() - Duration::from_millis(2 * DEBOUNCE_MS));
        app.typing = true;
        let cancel = Arc::new(AtomicBool::new(false));
        app.search.search_seq = 1;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: cancel.clone(),
        });
        app.search
            .result_tx
            .send(BackgroundSearchResult {
                seq: 1,
                query: "foo".to_string(),
                result: Ok(crate::search::SearchResult {
                    matches: vec![],
                    truncated: false,
                }),
            })
            .unwrap();

        app.take_outcome();
        app.return_from_resume(Some("s2"));
        app.tick();

        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.search.current.is_none());
        assert_eq!(app.search.groups.len(), 3);
        assert_eq!(app.search.group_cursor, 2);
        assert!(app.search.expanded);
    }

    #[test]
    fn search_result_arriving_after_enter_when_ticking_then_selected_results_are_kept() {
        // Enter set `should_quit`; the event loop still ticks once before
        // `take_outcome`, while a search result is already waiting.
        let mut app = app_with_groups_resuming("s2");
        app.search.search_seq = 1;
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        app.search
            .result_tx
            .send(BackgroundSearchResult {
                seq: 1,
                query: "foo".to_string(),
                result: Ok(crate::search::SearchResult {
                    matches: vec![],
                    truncated: false,
                }),
            })
            .unwrap();

        app.tick();

        assert_eq!(app.search.groups.len(), 3);
        assert_eq!(app.search.group_cursor, 2);
    }

    #[test]
    fn tree_mode_selection_when_returning_from_resume_then_tree_and_cursor_are_kept() {
        let mut app = app_with_groups_resuming("s1");
        app.tree_mode = true;
        app.tree.tree_cursor = 4;

        app.take_outcome();
        app.return_from_resume(Some("s1"));

        assert!(app.tree_mode);
        assert_eq!(app.tree.tree_cursor, 4);
    }

    #[test]
    fn recent_sessions_cursor_when_returning_from_resume_then_cursor_is_kept() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.filtered = ["/p/r0.jsonl", "/p/r1.jsonl", "/p/r2.jsonl"]
            .iter()
            .map(|path| make_recent_session(path))
            .collect();
        app.recent.cursor = 1;
        app.outcome = Some(AppOutcome::Resume(ResumeTarget {
            session_id: "r1".to_string(),
            file_path: "/p/r1.jsonl".to_string(),
            source: SessionSource::CLI,
            uuid: None,
        }));

        app.take_outcome();
        app.return_from_resume(Some("r1"));

        assert_eq!(app.recent.cursor, 1);
    }

    #[test]
    fn failed_launch_when_returning_from_resume_then_no_session_is_marked() {
        let mut app = app_with_groups_resuming("s2");

        app.take_outcome();
        app.return_from_resume(None);

        assert!(app.opened_sessions.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn test_take_outcome_pick_variant() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.outcome = Some(AppOutcome::Pick(PickedSession {
            session_id: "pick-1".to_string(),
            file_path: "/pick/session.jsonl".to_string(),
            source: SessionSource::CLI,
            project: "proj".to_string(),
            message_uuid: None,
        }));

        match app.take_outcome() {
            TuiOutcome::Pick(p) => assert_eq!(p.session_id, "pick-1"),
            other => panic!("Expected Pick, got {:?}", other),
        }
    }

    // =========================================================================
    // InputState isolated tests
    // =========================================================================

    #[test]
    fn test_input_state_new_is_empty() {
        let input = InputState::new();
        assert!(input.is_empty());
        assert_eq!(input.len(), 0);
        assert_eq!(input.cursor_pos(), 0);
        assert_eq!(input.text(), "");
    }

    #[test]
    fn test_input_state_push_char() {
        let mut input = InputState::new();
        input.push_char('h');
        input.push_char('i');
        assert_eq!(input.text(), "hi");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn test_input_state_push_char_utf8() {
        let mut input = InputState::new();
        input.push_char('ё');
        assert_eq!(input.text(), "ё");
        assert_eq!(input.cursor_pos(), 2); // 'ё' is 2 bytes in UTF-8
        input.push_char('!');
        assert_eq!(input.text(), "ё!");
        assert_eq!(input.cursor_pos(), 3);
    }

    #[test]
    fn test_input_state_push_char_at_middle() {
        let mut input = InputState::new();
        input.push_char('a');
        input.push_char('c');
        input.set_text_and_cursor("ac", 1);
        input.push_char('b');
        assert_eq!(input.text(), "abc");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn test_input_state_backspace() {
        let mut input = InputState::new();
        input.set_text("hello");
        assert!(input.backspace());
        assert_eq!(input.text(), "hell");
        assert_eq!(input.cursor_pos(), 4);
    }

    #[test]
    fn test_input_state_backspace_at_start() {
        let mut input = InputState::new();
        input.set_text_and_cursor("hello", 0);
        assert!(!input.backspace());
        assert_eq!(input.text(), "hello");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn test_input_state_backspace_empty() {
        let mut input = InputState::new();
        assert!(!input.backspace());
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn test_input_state_backspace_utf8_boundary() {
        let mut input = InputState::new();
        input.set_text("aё"); // 'a' = 1 byte, 'ё' = 2 bytes, total = 3
        assert_eq!(input.cursor_pos(), 3);
        assert!(input.backspace());
        assert_eq!(input.text(), "a");
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn test_input_state_delete_forward() {
        let mut input = InputState::new();
        input.set_text_and_cursor("abc", 1);
        assert!(input.delete_forward());
        assert_eq!(input.text(), "ac");
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn test_input_state_delete_forward_at_end() {
        let mut input = InputState::new();
        input.set_text("abc");
        assert!(!input.delete_forward());
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn test_input_state_move_left_right() {
        let mut input = InputState::new();
        input.set_text("abc");
        input.move_left();
        assert_eq!(input.cursor_pos(), 2);
        input.move_left();
        assert_eq!(input.cursor_pos(), 1);
        input.move_right();
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn test_input_state_move_left_at_start() {
        let mut input = InputState::new();
        input.set_text_and_cursor("abc", 0);
        input.move_left();
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn test_input_state_move_right_at_end() {
        let mut input = InputState::new();
        input.set_text("abc");
        input.move_right();
        assert_eq!(input.cursor_pos(), 3); // stays at end
    }

    #[test]
    fn test_input_state_word_navigation() {
        let mut input = InputState::new();
        input.set_text_and_cursor("hello world foo", 15);

        input.move_word_left();
        assert_eq!(input.cursor_pos(), 12);
        input.move_word_left();
        assert_eq!(input.cursor_pos(), 6);
        input.move_word_left();
        assert_eq!(input.cursor_pos(), 0);

        input.move_word_right();
        assert_eq!(input.cursor_pos(), 6);
        input.move_word_right();
        assert_eq!(input.cursor_pos(), 12);
        input.move_word_right();
        assert_eq!(input.cursor_pos(), 15);
    }

    #[test]
    fn test_input_state_delete_word_left() {
        let mut input = InputState::new();
        input.set_text("hello world");
        assert!(input.delete_word_left());
        assert_eq!(input.text(), "hello ");
        assert_eq!(input.cursor_pos(), 6);
    }

    #[test]
    fn test_input_state_delete_word_left_at_start() {
        let mut input = InputState::new();
        input.set_text_and_cursor("hello", 0);
        assert!(!input.delete_word_left());
        assert_eq!(input.text(), "hello");
    }

    #[test]
    fn test_input_state_delete_word_right() {
        let mut input = InputState::new();
        input.set_text_and_cursor("hello world", 0);
        assert!(input.delete_word_right());
        assert_eq!(input.text(), "world");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn test_input_state_delete_word_right_at_end() {
        let mut input = InputState::new();
        input.set_text("hello");
        assert!(!input.delete_word_right());
        assert_eq!(input.text(), "hello");
    }

    #[test]
    fn test_input_state_home_end() {
        let mut input = InputState::new();
        input.set_text_and_cursor("hello", 3);
        input.move_home();
        assert_eq!(input.cursor_pos(), 0);
        input.move_end();
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn test_input_state_clear() {
        let mut input = InputState::new();
        input.set_text("hello");
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn test_input_state_set_text_places_cursor_at_end() {
        let mut input = InputState::new();
        input.set_text("test");
        assert_eq!(input.cursor_pos(), 4);
    }

    #[test]
    fn test_input_state_set_text_and_cursor_clamps() {
        let mut input = InputState::new();
        input.set_text_and_cursor("abc", 100);
        assert_eq!(input.cursor_pos(), 3); // clamped to len
    }

    #[test]
    fn test_input_state_set_text_and_cursor_clamps_to_utf8_boundary() {
        let mut input = InputState::new();
        input.set_text_and_cursor("aёb", 2); // middle of 'ё' (bytes: a=0..1, ё=1..3)
        assert_eq!(input.cursor_pos(), 1);

        assert!(input.delete_forward());
        assert_eq!(input.text(), "ab");
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn test_input_state_into_text() {
        let mut input = InputState::new();
        input.set_text("hello");
        let text = input.into_text();
        assert_eq!(text, "hello");
    }

    #[test]
    fn test_input_state_all_ops_on_empty() {
        let mut input = InputState::new();
        // All operations on empty input should not panic
        input.move_left();
        input.move_right();
        input.move_word_left();
        input.move_word_right();
        input.move_home();
        input.move_end();
        assert!(!input.backspace());
        assert!(!input.delete_forward());
        assert!(!input.delete_word_left());
        assert!(!input.delete_word_right());
        assert_eq!(input.cursor_pos(), 0);
        assert!(input.is_empty());
    }

    // Entering AI mode with an open preview must close it — otherwise the
    // preview covers the agent's hits when they arrive.
    #[test]
    fn enter_ai_mode_closes_preview() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.preview_mode = true;

        app.enter_ai_mode();

        assert!(
            !app.preview_mode,
            "enter_ai_mode must clear preview_mode so the hits are not covered"
        );
    }

    // =========================================================================
    // Task 2: SearchHandle / `current` source-of-truth tests
    // =========================================================================

    // `start_search` must (a) install a fresh `SearchHandle` in `current`,
    // (b) cancel the prior handle by setting its `cancel` flag to `true`,
    // and (c) bump `search_seq` so the new handle is distinguishable from
    // the cancelled one. Empty `search_paths` keeps the spawned worker
    // thread idle (search_multiple_paths returns Ok({matches: [], …})
    // immediately) so the test does not race with real ripgrep work.
    #[test]
    fn start_search_cancels_prior_handle_and_bumps_seq() {
        let mut app = App::new(vec![]);
        app.input.set_text("foo");

        app.start_search();
        let first = app.search.current.as_ref().expect("first handle present");
        let first_seq = first.seq;
        let first_cancel = first.cancel.clone();

        app.start_search();
        let second = app
            .search
            .current
            .as_ref()
            .expect("second handle present after re-issue");

        assert!(
            first_cancel.load(Ordering::Relaxed),
            "prior handle's cancel flag must be set"
        );
        assert!(
            second.seq > first_seq,
            "second seq ({}) must be greater than first ({})",
            second.seq,
            first_seq
        );
        assert!(
            !second.cancel.load(Ordering::Relaxed),
            "new handle must start uncancelled"
        );
    }

    // `handle_search_result` discriminates by `seq` only and ignores results
    // when no handle is in flight. The matching-seq case clears `current`
    // and processes the matches.
    #[test]
    fn handle_search_result_no_op_when_no_current() {
        let mut app = App::new(vec!["/test".to_string()]);
        assert!(app.search.current.is_none());

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "ignored".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![],
                truncated: false,
            }),
        });

        assert!(app.search.results_query.is_empty());
        assert!(app.search.groups.is_empty());
    }

    #[test]
    fn handle_search_result_drops_mismatched_seq() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.search_seq = 5;
        let cancel = Arc::new(AtomicBool::new(false));
        app.search.current = Some(SearchHandle {
            seq: 5,
            cancel: cancel.clone(),
        });

        app.handle_search_result(BackgroundSearchResult {
            seq: 3,
            query: "stale".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![],
                truncated: false,
            }),
        });

        assert!(
            app.search.current.is_some(),
            "current handle must survive a mismatched seq"
        );
        assert_eq!(app.search.results_query, "");
    }

    #[test]
    fn handle_search_result_consumes_current_on_seq_match() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.search_seq = 7;
        app.search.current = Some(SearchHandle {
            seq: 7,
            cancel: Arc::new(AtomicBool::new(false)),
        });

        let m = RipgrepMatch {
            file_path: "/sessions/match.jsonl".to_string(),
            message: Some(Message {
                session_id: "match".to_string(),
                role: "user".to_string(),
                content: "needle".to_string(),
                timestamp: Utc::now(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::CLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 7,
            query: "needle".to_string(),
            result: Ok(crate::search::SearchResult {
                matches: vec![m],
                truncated: false,
            }),
        });

        assert!(
            app.search.current.is_none(),
            "matching seq must clear current"
        );
        assert!(!app.is_searching(), "is_searching must be false");
        assert_eq!(app.search.results_query, "needle");
        assert_eq!(app.search.groups.len(), 1);
        assert_eq!(app.search.groups[0].session_id, "match");
    }

    // The Err arm of `handle_search_result` must clear `current` (so
    // `is_searching()` returns false) and store the error message. A
    // regression that re-introduced an early-return in this arm — for
    // example checking the error before the seq match and bailing — could
    // strand `current` at `Some(_)` and resurface the original
    // stuck-`searching` behaviour. Pin both invariants here.
    #[test]
    fn handle_search_result_err_clears_current_and_stores_error() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.search_seq = 4;
        app.search.current = Some(SearchHandle {
            seq: 4,
            cancel: Arc::new(AtomicBool::new(false)),
        });

        app.handle_search_result(BackgroundSearchResult {
            seq: 4,
            query: "boom".to_string(),
            result: Err("ripgrep search failed: exit status 2".to_string()),
        });

        assert!(
            app.search.current.is_none(),
            "Err with matching seq must clear current"
        );
        assert!(
            !app.is_searching(),
            "is_searching must be false after Err lands"
        );
        assert_eq!(
            app.search.error.as_deref(),
            Some("ripgrep search failed: exit status 2"),
            "error must be surfaced for the renderer"
        );
    }

    // `clear_input` (Ctrl-C with non-empty input) must cancel any in-flight
    // search by setting the handle's cancel flag and clearing `current`.
    #[test]
    fn clear_input_cancels_in_flight_search() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text_and_cursor("query", 5);
        let cancel = Arc::new(AtomicBool::new(false));
        app.search.current = Some(SearchHandle {
            seq: 1,
            cancel: cancel.clone(),
        });

        app.clear_input();

        assert!(
            cancel.load(Ordering::Relaxed),
            "clear_input must set the cancel flag of the in-flight handle"
        );
        assert!(
            app.search.current.is_none(),
            "current handle must be taken/cleared"
        );
        assert!(!app.is_searching());
    }

    // End-to-end regression test for the original stuck-`searching` bug,
    // now expressed against spawn-per-request semantics. Two
    // `start_search` calls back-to-back must converge on `is_searching()
    // == false` once the latest result lands. This exercises real ripgrep
    // children: each request spawns its own thread that streams `rg`
    // output, checks the cancel flag, and either delivers its
    // `BackgroundSearchResult` (matching `current.seq`) or has its send
    // suppressed by `if !cancel.load()`. Without the legacy long-lived
    // worker thread, the seq discriminator in `handle_search_result` is
    // the only mechanism that decides which result wins — this test
    // proves it does end-to-end.
    //
    // The assertion intentionally only verifies the
    // `is_searching() == false` invariant: the cancellation contract
    // guarantees at most two delivered results (the matching one plus a
    // potential straggler that won the load-vs-send race), but
    // sandboxing the channel-count to "exactly 1" is racy on slow CI
    // hosts where the cancelled worker's send may sneak in before its
    // `cancel.load()` check fires. The seq-mismatch guard in
    // `handle_search_result` filters any straggler out of `groups`, and
    // the `current.is_none()` invariant is the actual contract this
    // test pins.
    #[test]
    fn back_to_back_start_search_delivers_one_result_and_clears_searching() {
        // One short JSONL fixture with a line that matches "foo".
        let dir = tempfile::TempDir::new().unwrap();
        let proj_dir = dir.path().join("-Users-user-proj");
        std::fs::create_dir_all(&proj_dir).unwrap();
        let session_file = proj_dir.join("sess1.jsonl");
        std::fs::write(
            &session_file,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"foo bar"}]},"sessionId":"sess-1","timestamp":"2025-06-01T10:00:00Z"}"#,
        )
        .unwrap();

        let mut app = App::new(vec![dir.path().to_str().unwrap().to_string()]);
        app.input.set_text("foo");

        app.start_search();
        app.start_search();

        // Poll `tick()` (which drains and processes the channel via
        // `handle_search_result`) until the latest seq lands and clears
        // `current`. Generous deadline shields against slow CI hosts;
        // the steady-state expectation is sub-100 ms.
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.is_searching() && Instant::now() < deadline {
            app.tick();
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            !app.is_searching(),
            "is_searching() must be false after the latest result lands"
        );
    }

    // Regression for the original stuck-`searching` bug, expressed as a
    // synthetic channel-injection test. After two `start_search` calls, we
    // jump `current.seq` to a far-out value (100) so synthetic injections
    // cannot collide with real-worker results. We then push a stale (seq=99)
    // and a fresh (seq=100) result onto the channel; `tick()` must drain
    // both, keep only seq=100's data, and converge on `is_searching() ==
    // false`. The earlier code's secondary filter could discard the matching
    // seq result on a query/paths/use_regex mismatch and never clear the
    // stored `searching: bool`; this test pins the contract that seq is the
    // only discriminator and `current` is the single source of truth.
    #[test]
    fn back_to_back_start_search_stale_then_fresh_clears_is_searching() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut app = App::new(vec![dir.path().to_str().unwrap().to_string()]);
        app.input.set_text("query");

        // Two requests: the first is cancelled by the second. `start_search`
        // also spawns real worker threads; with an empty corpus they may
        // produce empty `Ok` results that race with our synthetic injections.
        // To eliminate the collision risk entirely, after the two real
        // start_search calls we jump `search_seq` (and `current.seq`) to a
        // value far above what the real workers can produce, then inject
        // synthetic results with seqs `99` (stale) and `100` (fresh). Real
        // worker results carrying seq `1` or `2` will be dropped by the
        // seq-mismatch guard in `handle_search_result`.
        app.start_search();
        app.start_search();
        // Bump the current handle's seq above the real-worker range and
        // sync `search_seq` so any future `start_search` continues
        // monotonically. We rebuild the handle in place to keep its
        // `cancel` flag tied to the new seq number we are about to inject
        // results for. Before swapping the handle out, also cancel the
        // second real worker explicitly: `start_search` only cancels the
        // *previous* request, so worker 2's cancel flag is otherwise still
        // `false`. Without this it can win the load-vs-send race and push
        // a real seq=2 result onto the channel; the seq-mismatch guard in
        // `handle_search_result` would still drop it, but cancelling here
        // makes the test deterministic by suppressing the send entirely.
        app.search.search_seq = 100;
        let prev_cancel = app
            .search
            .current
            .take()
            .map(|h| h.cancel)
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        prev_cancel.store(true, Ordering::Relaxed);
        app.search.current = Some(SearchHandle {
            seq: 100,
            cancel: Arc::new(AtomicBool::new(false)),
        });

        // Inject the stale seq=99 result first (must be silently dropped),
        // then the fresh seq=100 result that carries the only data we
        // expect to see materialise in `groups`. Real-worker stragglers
        // with seq <= 2 will likewise mismatch and be dropped.
        let stale_match = RipgrepMatch {
            file_path: "/sessions/stale.jsonl".to_string(),
            message: Some(Message {
                session_id: "stale".to_string(),
                role: "user".to_string(),
                content: "query".to_string(),
                timestamp: Utc::now(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::CLI,
        };
        let fresh_match = RipgrepMatch {
            file_path: "/sessions/fresh.jsonl".to_string(),
            message: Some(Message {
                session_id: "fresh".to_string(),
                role: "user".to_string(),
                content: "query".to_string(),
                timestamp: Utc::now(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::CLI,
        };

        app.search
            .result_tx
            .send(BackgroundSearchResult {
                seq: 99,
                query: "query".to_string(),
                result: Ok(crate::search::SearchResult {
                    matches: vec![stale_match],
                    truncated: false,
                }),
            })
            .expect("send stale seq=99 result");
        app.search
            .result_tx
            .send(BackgroundSearchResult {
                seq: 100,
                query: "query".to_string(),
                result: Ok(crate::search::SearchResult {
                    matches: vec![fresh_match],
                    truncated: false,
                }),
            })
            .expect("send fresh seq=100 result");

        // Drain everything currently buffered: real-worker results (seq
        // 1 or 2) are dropped by the mismatch guard, the stale seq=99 is
        // dropped, the fresh seq=100 lands and clears `current`.
        app.tick();

        assert!(
            !app.is_searching(),
            "is_searching() must be false after the matching seq=100 result lands"
        );
        assert!(
            app.search.current.is_none(),
            "current must be cleared when seq matches"
        );
        assert_eq!(
            app.search.groups.len(),
            1,
            "only the seq=100 result must populate groups"
        );
        assert_eq!(
            app.search.groups[0].session_id, "fresh",
            "groups must contain only the fresh seq=100 data, not the stale seq=99 or any real-worker data"
        );
    }

    // Drop on `App` must cancel any in-flight search so the per-request
    // worker thread and its `rg` child unwind instead of running to
    // completion after the TUI exits. This catches all outcome paths
    // (Quit / Resume / Pick / panic / unwind) without scattering the
    // cancellation across each one.
    #[test]
    fn dropping_app_cancels_in_flight_search() {
        let app = App::new(vec!["/test".to_string()]);
        // Build a handle whose cancel flag we can observe from the test
        // after the App is dropped. We hold our own clone of the Arc so
        // dropping App does not free the AtomicBool out from under us.
        let cancel = Arc::new(AtomicBool::new(false));
        let observer = cancel.clone();

        // Install via direct field access — the test module shares the
        // module's pub(crate) visibility for `SearchHandle`.
        let mut app = app;
        app.search.current = Some(SearchHandle {
            seq: app.search.search_seq + 1,
            cancel,
        });
        assert!(app.is_searching());
        assert!(
            !observer.load(Ordering::Relaxed),
            "cancel flag must start false"
        );

        drop(app);

        assert!(
            observer.load(Ordering::Relaxed),
            "Drop on App must set the in-flight handle's cancel flag"
        );
    }

    #[test]
    fn test_read_opencode_session_cwd_cached_survives_db_removal() {
        // First call hits the SQLite DB; the resolved directory is stored in
        // the cache. Removing the DB on disk between calls must NOT cause
        // the second call to miss — the cache is the load-bearing path for
        // repeated project-filter rebuilds.
        use rusqlite::Connection;

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT,
                name TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                slug TEXT NOT NULL DEFAULT '', directory TEXT, title TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
            INSERT INTO project VALUES ('p', '/tmp/p', 'git', 'p', 1, 2);
            INSERT INTO session VALUES ('ses_OC', 'p', '', '/tmp/cached-cwd', 't', 1, 2);
            "#,
        )
        .unwrap();
        drop(conn);

        let synth = format!("{}#ses_OC", db_path.to_string_lossy());
        let mut cache: HashMap<String, Option<String>> = HashMap::new();

        let first = read_opencode_session_cwd_cached(&synth, &mut cache);
        assert_eq!(first.as_deref(), Some("/tmp/cached-cwd"));
        assert!(
            cache.contains_key(&synth),
            "cache must store the resolved cwd after the first lookup"
        );

        // Wipe the DB so any uncached lookup would return None.
        std::fs::remove_file(&db_path).unwrap();

        let second = read_opencode_session_cwd_cached(&synth, &mut cache);
        assert_eq!(
            second.as_deref(),
            Some("/tmp/cached-cwd"),
            "second call must return cached value despite DB removal"
        );

        // Negative lookups are also cached so repeated misses stay O(1).
        let missing_synth = format!("{}#ses_MISSING", db_path.to_string_lossy());
        let first_miss = read_opencode_session_cwd_cached(&missing_synth, &mut cache);
        assert!(first_miss.is_none());
        assert!(
            cache.contains_key(&missing_synth),
            "negative lookups must be cached"
        );
        let second_miss = read_opencode_session_cwd_cached(&missing_synth, &mut cache);
        assert!(second_miss.is_none());
    }

    // Drop on `App` must also cancel the background message-count thread
    // so it exits quickly after the TUI shuts down instead of finishing
    // its file scan in the background. Same all-paths-covered argument as
    // the search cancellation above.
    #[test]
    fn dropping_app_cancels_in_flight_message_count() {
        let app = App::new(vec!["/test".to_string()]);
        let flag = Arc::new(AtomicBool::new(false));
        let observer = flag.clone();

        let mut app = app;
        app.search.message_count_cancel = Some(flag);
        assert!(!observer.load(Ordering::Relaxed));

        drop(app);

        assert!(
            observer.load(Ordering::Relaxed),
            "Drop on App must set the message-count cancel flag"
        );
    }

    // =========================================================================
    // handle_action dispatch tests
    // =========================================================================

    // Drives one action from every dispatch category through `handle_action`
    // so each sub-dispatcher arm is exercised. The semantics of the
    // individual handlers are covered by their own tests; this pins the
    // wiring from `KeyAction` variant to handler.
    #[test]
    fn handle_action_dispatches_every_variant_without_panic() {
        let mut app = App::new(vec!["/test".to_string()]);
        // Keep the project toggle deterministic across machines: no project
        // scope and no cwd means ToggleProjectFilter early-returns.
        app.current_project_paths = vec![];
        app.current_cwd = None;

        let actions = vec![
            KeyAction::Up,
            KeyAction::Down,
            KeyAction::Left,
            KeyAction::Right,
            KeyAction::Tab,
            KeyAction::Enter,
            KeyAction::InputChar('q'),
            KeyAction::Backspace,
            KeyAction::Delete,
            KeyAction::ClearInput,
            KeyAction::DeleteWordLeft,
            KeyAction::DeleteWordRight,
            KeyAction::MoveWordLeft,
            KeyAction::MoveWordRight,
            KeyAction::MoveHome,
            KeyAction::MoveEnd,
            KeyAction::ToggleRegex,
            KeyAction::ToggleProjectFilter,
            KeyAction::ToggleAutomationFilter,
            KeyAction::TogglePreview,
            KeyAction::ExitPreview,
            KeyAction::EnterAiMode,
            KeyAction::ExitAiMode,
            KeyAction::CancelAiSearch,
            KeyAction::EnterTreeMode,
            KeyAction::EnterTreeModeRecent,
            KeyAction::TreeUp,
            KeyAction::TreeDown,
            KeyAction::TreeLeft,
            KeyAction::TreeRight,
            KeyAction::TreeTab,
            KeyAction::TreeEnter,
            KeyAction::ExitTreeMode,
            KeyAction::Noop,
        ];
        for action in actions {
            app.handle_action(action);
        }

        assert!(!app.should_quit, "no non-Quit action may quit the TUI");
        assert!(app.regex_mode, "ToggleRegex must flip regex_mode");
        assert_eq!(
            app.automation_filter,
            AutomationFilter::Auto,
            "ToggleAutomationFilter must advance Manual -> Auto"
        );
        assert!(!app.ai.active, "ExitAiMode must leave AI mode");
        assert!(app.outcome.is_none());

        app.handle_action(KeyAction::Quit);
        assert!(app.should_quit, "Quit must set should_quit");
    }

    // =========================================================================
    // tick() polling-phase tests
    // =========================================================================

    #[test]
    fn tick_applies_message_count_updates_in_place() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.all_groups = vec![make_group("s1", "/sessions/s1.jsonl")];
        app.search.groups = vec![make_group("s1", "/sessions/s1.jsonl")];
        let (tx, rx) = mpsc::channel();
        app.search.message_count_rx = Some(rx);
        tx.send(("/sessions/s1.jsonl".to_string(), 42, true))
            .unwrap();

        app.tick();

        assert_eq!(app.search.all_groups[0].message_count, Some(42));
        assert!(app.search.all_groups[0].message_count_compacted);
        assert_eq!(
            app.search.groups[0].message_count,
            Some(42),
            "the filtered view must be updated in place too"
        );
        assert!(app.search.groups[0].message_count_compacted);
    }

    #[test]
    fn tick_tree_load_success_installs_tree() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.tree_mode = true;
        app.tree.tree_loading = true;
        let (tx, rx) = mpsc::channel();
        app.tree.tree_load_rx = Some(rx);
        tx.send(Ok(SessionTree::new_for_test(
            "sess-tree".to_string(),
            "/sessions/tree.jsonl".to_string(),
            SessionSource::CLI,
            vec![],
        )))
        .unwrap();

        app.tick();

        assert!(app.tree.session_tree.is_some());
        assert!(!app.tree.tree_loading);
        assert!(app.tree.tree_load_rx.is_none());
        assert!(app.needs_full_redraw);
        assert!(app.tree_mode, "successful load must stay in tree mode");
    }

    #[test]
    fn tick_tree_load_error_surfaces_and_exits_tree_mode() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.tree_mode = true;
        app.tree.tree_loading = true;
        let (tx, rx) = mpsc::channel();
        app.tree.tree_load_rx = Some(rx);
        tx.send(Err("boom".to_string())).unwrap();

        app.tick();

        assert_eq!(app.search.error.as_deref(), Some("Tree load error: boom"));
        assert!(!app.tree_mode, "load error must fall back to search mode");
        assert!(!app.tree.tree_loading);
        assert!(app.tree.tree_load_rx.is_none());
        assert!(app.needs_full_redraw);
    }

    // =========================================================================
    // Ctrl+G agent search tests
    // =========================================================================

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
    }

    fn ai_app() -> App {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.filtered = vec![make_recent_session("/p/recent.jsonl")];
        app.enter_ai_mode();
        app.ai.query.set_text("kotlin");
        app
    }

    /// Install a fake running agent; returns its sender and cancel flag.
    fn install_agent(app: &mut App) -> (Sender<AgentEvent>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel();
        let (handle, cancel) = AgentSearchHandle::for_test(rx);
        app.ai.search = Some(handle);
        (tx, cancel)
    }

    fn with_hits(app: &mut App, paths: &[&str]) {
        app.ai.hits = Some(paths.iter().map(|p| found(p)).collect());
        app.ai.hit_cursor = 0;
    }

    #[test]
    fn ai_mode_claude_missing_submit_shows_error_and_stays_in_input() {
        let mut app = ai_app();
        app.start_agent_search(
            "x".into(),
            Err("Claude binary not found in PATH".into()),
            AgentSearchConfig::default(),
        );
        assert_eq!(
            app.ai.error.as_deref(),
            Some("Claude binary not found in PATH")
        );
        assert!(!app.ai.searching());
        assert!(app.ai.hits.is_none());
        assert!(app.ai.active);
    }

    #[test]
    fn ai_enter_when_query_empty_then_no_agent_started() {
        let mut app = ai_app();
        app.ai.query.set_text("");

        app.handle_action(KeyAction::Enter);

        assert!(!app.ai.searching());
        assert!(app.ai.error.is_none());
    }

    #[test]
    fn ai_cursor_keys_when_agent_running_then_cursor_moves_and_agent_kept() {
        let mut app = ai_app();
        app.ai.query.set_text("kotlin flow");
        let (_tx, cancel) = install_agent(&mut app);

        let mut positions = Vec::new();
        for action in [
            KeyAction::MoveHome,
            KeyAction::Right,
            KeyAction::MoveWordRight,
            KeyAction::Left,
            KeyAction::MoveWordLeft,
            KeyAction::MoveEnd,
        ] {
            app.handle_action(action);
            positions.push(app.ai.query.cursor_pos());
        }

        assert_eq!(positions, vec![0, 1, 7, 6, 0, 11]);
        assert!(app.ai.searching());
        assert!(!cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn searching_esc_cancels_and_keeps_query() {
        let mut app = ai_app();
        let (_tx, cancel) = install_agent(&mut app);
        app.ai.progress = vec!["kotlin".into()];

        app.handle_action(KeyAction::CancelAiSearch);

        assert!(!app.ai.searching());
        assert!(app.ai.active);
        assert_eq!(app.ai.query.text(), "kotlin");
        assert!(app.ai.progress.is_empty());
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn searching_ctrl_g_exits_ai_mode_and_cancels() {
        let mut app = ai_app();
        let (_tx, cancel) = install_agent(&mut app);

        app.handle_action(KeyAction::ExitAiMode);

        assert!(!app.ai.active);
        assert!(!app.ai.searching());
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn searching_ctrl_b_does_nothing() {
        let mut app = ai_app();
        let _agent = install_agent(&mut app);

        app.handle_action(KeyAction::EnterTreeModeRecent);
        app.handle_action(KeyAction::EnterTreeMode);

        assert!(!app.tree_mode);
        assert!(app.ai.searching());
    }

    #[test]
    fn searching_when_resume_taken_then_agent_cancelled_and_progress_cleared() {
        let mut app = ai_app();
        let (_tx, cancel) = install_agent(&mut app);
        app.ai.progress = vec!["x".into()];
        app.outcome = Some(AppOutcome::Resume(ResumeTarget {
            session_id: "s".into(),
            file_path: "/p/s.jsonl".into(),
            source: SessionSource::CLI,
            uuid: None,
        }));

        let _ = app.take_outcome();

        assert!(!app.ai.searching());
        assert!(app.ai.progress.is_empty());
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn restart_after_error_clears_error_and_progress() {
        let mut app = ai_app();
        app.ai.error = Some("old".into());
        app.ai.progress = vec!["old".into()];
        with_hits(&mut app, &["/p/a.jsonl"]);

        app.start_agent_search(
            "q".into(),
            Ok("/nonexistent/claude".into()),
            AgentSearchConfig::default(),
        );

        assert_eq!(app.ai.error, None);
        assert!(app.ai.progress.is_empty());
        assert!(app.ai.hits.is_none());
        assert_eq!(app.ai.hit_cursor, 0);
        assert!(app.ai.searching());
    }

    #[test]
    fn edit_after_error_clears_error() {
        let mut app = ai_app();
        app.ai.error = Some("Not logged in".into());

        app.handle_action(KeyAction::InputChar('a'));

        assert_eq!(app.ai.error, None);
    }

    #[test]
    fn tick_disconnect_without_done_shows_stopped_unexpectedly() {
        let mut app = ai_app();
        let (tx, _cancel) = install_agent(&mut app);
        drop(tx);

        app.tick();

        assert!(!app.ai.searching());
        assert_eq!(app.ai.error.as_deref(), Some("agent stopped unexpectedly"));
    }

    #[test]
    fn searching_query_edit_cancels_search() {
        let mut app = ai_app();
        let (_tx, cancel) = install_agent(&mut app);

        app.handle_action(KeyAction::InputChar('a'));

        assert!(!app.ai.searching());
        assert!(cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn tick_progress_event_updates_progress() {
        let mut app = ai_app();
        let (tx, _cancel) = install_agent(&mut app);
        tx.send(AgentEvent::Progress(vec!["kitty".into()])).unwrap();

        app.tick();

        assert_eq!(app.ai.progress, vec!["kitty".to_string()]);
        assert!(app.ai.searching());
    }

    #[test]
    fn tick_done_ok_installs_hits_and_resets_cursor() {
        let mut app = ai_app();
        let (tx, _cancel) = install_agent(&mut app);
        app.ai.hit_cursor = 3;
        tx.send(AgentEvent::Done(Ok(vec![
            found("/p/a.jsonl"),
            found("/p/b.jsonl"),
        ])))
        .unwrap();

        app.tick();

        assert_eq!(app.ai.hits.as_ref().map(Vec::len), Some(2));
        assert_eq!(app.ai.hit_cursor, 0);
        assert!(!app.ai.searching());
    }

    #[test]
    fn tick_done_err_shows_error_without_hits() {
        let mut app = ai_app();
        let (tx, _cancel) = install_agent(&mut app);
        tx.send(AgentEvent::Done(Err("Not logged in".into())))
            .unwrap();

        app.tick();

        assert_eq!(app.ai.error.as_deref(), Some("Not logged in"));
        assert!(app.ai.hits.is_none());
        assert!(!app.ai.searching());
    }

    #[test]
    fn results_down_up_move_hit_cursor_within_bounds() {
        let mut app = ai_app();
        with_hits(&mut app, &["/p/a.jsonl", "/p/b.jsonl"]);

        app.handle_action(KeyAction::Down);
        app.handle_action(KeyAction::Down);
        assert_eq!(app.ai.hit_cursor, 1);
        app.handle_action(KeyAction::Up);
        app.handle_action(KeyAction::Up);
        assert_eq!(app.ai.hit_cursor, 0);
    }

    #[test]
    fn results_enter_resumes_hit_at_latest_point() {
        let mut app = ai_app();
        with_hits(&mut app, &["/p/a.jsonl", "/p/b.jsonl"]);
        app.ai.hit_cursor = 1;

        app.handle_action(KeyAction::Enter);

        assert_eq!(
            app.outcome,
            Some(AppOutcome::Resume(ResumeTarget {
                session_id: "/p/b.jsonl".into(),
                file_path: "/p/b.jsonl".into(),
                source: SessionSource::CLI,
                uuid: None,
            }))
        );
        assert!(app.should_quit);
    }

    #[test]
    fn results_enter_in_picker_mode_picks_hit() {
        let mut app = ai_app();
        app.picker_mode = true;
        with_hits(&mut app, &["/p/a.jsonl"]);

        app.handle_action(KeyAction::Enter);

        assert!(matches!(
            app.outcome,
            Some(AppOutcome::Pick(PickedSession { ref session_id, message_uuid: None, .. }))
                if session_id == "/p/a.jsonl"
        ));
    }

    #[test]
    fn results_right_opens_tree_for_hit_and_exit_tree_returns_to_results() {
        let mut app = ai_app();
        with_hits(&mut app, &[&fixture("linear_session.jsonl")]);

        app.handle_action(KeyAction::Right);
        assert!(app.tree_mode);
        app.exit_tree_mode();

        assert!(!app.tree_mode);
        assert!(app.ai.hits.is_some());
        assert!(app.ai.active);
    }

    #[test]
    fn results_ctrl_b_opens_tree_for_hit() {
        let mut app = ai_app();
        with_hits(
            &mut app,
            &["/p/missing.jsonl", &fixture("linear_session.jsonl")],
        );
        app.ai.hit_cursor = 1;

        app.handle_action(KeyAction::EnterTreeModeRecent);

        assert!(app.tree_mode);
        // The recent list's session (/p/recent.jsonl) does not exist; only
        // the selected hit's file loads.
        let loaded = app
            .tree
            .tree_load_rx
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(loaded.is_ok());
    }

    #[test]
    fn ai_input_and_searching_tab_and_ctrl_v_do_not_open_preview() {
        let mut app = ai_app();
        app.input.set_text("needle");
        app.search.groups = vec![make_group("g", "/p/g.jsonl")];

        app.handle_action(KeyAction::Tab);
        assert!(
            !app.preview_mode,
            "Tab in input must not preview the hidden list"
        );
        app.handle_action(KeyAction::TogglePreview);
        assert!(
            !app.preview_mode,
            "Ctrl+V in input must not preview the hidden list"
        );

        let _agent = install_agent(&mut app);
        app.handle_action(KeyAction::Tab);
        assert!(
            !app.preview_mode,
            "Tab while searching must not preview the hidden list"
        );
        app.handle_action(KeyAction::TogglePreview);
        assert!(
            !app.preview_mode,
            "Ctrl+V while searching must not preview the hidden list"
        );
    }

    #[test]
    fn tick_done_ok_with_preview_open_shows_hits() {
        let mut app = ai_app();
        let (tx, _cancel) = install_agent(&mut app);
        app.preview_mode = true;
        tx.send(AgentEvent::Done(Ok(vec![found("/p/a.jsonl")])))
            .unwrap();

        app.tick();

        assert!(app.ai.hits.is_some());
        assert!(!app.preview_mode, "a preview must not cover the hits");
    }

    #[test]
    fn results_ctrl_b_key_with_empty_hidden_list_opens_tree_for_hit() {
        use crate::tui::dispatch::classify_key;
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let mut app = ai_app();
        app.recent.filtered.clear();
        with_hits(&mut app, &[&fixture("linear_session.jsonl")]);
        let key = KeyEvent {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };

        let action = classify_key(key, &app.key_context());
        app.handle_action(action);

        assert!(app.tree_mode, "Ctrl+B must open the hit's tree");
    }

    #[test]
    fn results_tab_and_ctrl_v_do_nothing() {
        let mut app = ai_app();
        app.input.set_text("needle");
        app.search.groups = vec![make_group("g", "/p/g.jsonl")];
        with_hits(&mut app, &["/p/a.jsonl"]);

        app.handle_action(KeyAction::Tab);
        assert!(!app.preview_mode, "Tab must not preview the hidden list");
        app.handle_action(KeyAction::TogglePreview);
        assert!(!app.preview_mode, "Ctrl+V must not preview the hidden list");
        assert!(app.ai.hits.is_some());
    }

    #[test]
    fn results_char_clears_hits_and_returns_to_input() {
        let mut app = ai_app();
        with_hits(&mut app, &["/p/a.jsonl"]);

        app.handle_action(KeyAction::InputChar('z'));

        assert!(app.ai.hits.is_none());
        assert!(app.ai.query.text().ends_with('z'));
    }

    #[test]
    fn results_esc_exits_ai_mode_and_keeps_recent_list_order() {
        let mut app = ai_app();
        app.recent.filtered = vec![
            make_recent_session("/p/r1.jsonl"),
            make_recent_session("/p/r2.jsonl"),
        ];
        let before: Vec<String> = app
            .recent
            .filtered
            .iter()
            .map(|s| s.session_id.clone())
            .collect();
        with_hits(&mut app, &["/p/r2.jsonl"]);

        app.handle_action(KeyAction::ExitAiMode);

        let after: Vec<String> = app
            .recent
            .filtered
            .iter()
            .map(|s| s.session_id.clone())
            .collect();
        assert_eq!(before, after);
        assert!(!app.ai.active);
        assert!(app.ai.hits.is_none());
    }

    #[test]
    fn toggles_in_ai_mode_do_not_clear_hits() {
        let mut app = ai_app();
        with_hits(&mut app, &["/p/a.jsonl"]);

        app.handle_action(KeyAction::ToggleRegex);
        app.handle_action(KeyAction::ToggleProjectFilter);
        app.handle_action(KeyAction::ToggleAutomationFilter);

        assert!(app.ai.hits.is_some());
    }
}
