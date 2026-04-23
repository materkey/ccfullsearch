use crate::recent::{collect_recent_sessions, detect_session_automation, RecentSession};
use crate::resume::encode_path_for_claude;
use crate::search::{group_by_session, search_multiple_paths, SessionGroup};
use crate::session::SessionSource;
use crate::tree::SessionTree;
use crate::tui::dispatch::{KeyAction, KeyContext};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
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

fn normalize_path_for_prefix_check(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized.trim_end_matches(['/', '\\']).to_string()
}

fn path_is_within_project(file_path: &str, project_path: &str) -> bool {
    let file_path = normalize_path_for_prefix_check(file_path);
    let project_path = normalize_path_for_prefix_check(project_path);

    file_path == project_path
        || file_path
            .strip_prefix(&project_path)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Recent sessions sub-state: encapsulates global/project data sources,
/// filtered view, background loaders, and navigation cursor.
pub struct RecentState {
    /// Unfiltered global sessions (loaded once at startup)
    pub(crate) all: Vec<RecentSession>,
    /// Project-specific sessions (loaded when project filter is activated)
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
    pub(crate) fn apply_filter(
        &mut self,
        project_filter: bool,
        project_paths: &[String],
        automation_filter: &AutomationFilter,
    ) {
        let project_filtered: Vec<_> = if project_filter && !project_paths.is_empty() {
            let source = self.project.as_ref().unwrap_or(&self.all);
            source
                .iter()
                .filter(|s| {
                    project_paths
                        .iter()
                        .any(|p| path_is_within_project(&s.file_path, p))
                })
                .cloned()
                .collect()
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

    /// Total count of sessions in the active source (for status text).
    pub fn total_count(&self, project_filter: bool) -> usize {
        if project_filter {
            self.project.as_ref().map_or(self.all.len(), |ps| ps.len())
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
        /// Current search query to restore on return (overlay mode)
        query: String,
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

/// Result from background search thread.
pub(crate) struct BackgroundSearchResult {
    pub seq: u64,
    pub query: String,
    pub paths: Vec<String>,
    pub use_regex: bool,
    pub result: Result<crate::search::SearchResult, String>,
}

/// Search-related state: results, cursors, background channel.
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
    pub searching: bool,
    pub error: Option<String>,
    /// Cache: file_path → set of uuids on the latest chain (for fork indicator)
    pub latest_chains: HashMap<String, HashSet<String>>,
    /// Channel to receive search results from background thread
    pub(crate) search_rx: Receiver<BackgroundSearchResult>,
    /// Channel to send search requests to background thread
    pub(crate) search_tx: Sender<(u64, String, Vec<String>, bool)>,
    /// Monotonic request sequence to ignore stale async results
    pub(crate) search_seq: u64,
    /// Whether the last search hit the per-file match limit (results may be incomplete)
    pub search_truncated: bool,
    /// Channel to receive (file_path, count, compacted) from background message-counting thread
    pub(crate) message_count_rx: Option<std::sync::mpsc::Receiver<(String, usize, bool)>>,
    /// Cancellation flag for the background counting thread
    pub(crate) message_count_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
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

/// AI search re-ranking state.
pub struct AiState {
    pub active: bool,
    pub query: InputState,
    pub thinking: bool,
    pub(crate) result_rx: Option<Receiver<crate::ai::AiRankResult>>,
    pub error: Option<String>,
    pub ranked_count: Option<usize>,
    pub(crate) original_recent_order: Option<Vec<RecentSession>>,
    pub(crate) original_groups_order: Option<Vec<crate::search::SessionGroup>>,
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
/// Consumed by `into_outcome()` to produce the public `TuiOutcome`.
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
    /// Outcome set by `on_enter` / `on_enter_tree` — consumed by `into_outcome()`.
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
    /// Tree explorer mode
    pub tree_mode: bool,
    /// Whether search is scoped to current project only (Ctrl+A toggle)
    pub project_filter: bool,
    /// Filter for automated vs manual sessions (Ctrl+H toggle)
    pub automation_filter: AutomationFilter,
    /// Cache: file_path -> resolved automation marker (including negative lookups)
    automation_cache: HashMap<String, Option<String>>,
    /// All search paths (for "all sessions" mode)
    pub(crate) all_search_paths: Vec<String>,
    /// Search path(s) for current project only
    pub current_project_paths: Vec<String>,
    /// Recent sessions sub-state
    pub recent: RecentState,
    /// AI search re-ranking sub-state
    pub ai: AiState,
    /// Picker mode: on_enter sets outcome to Pick instead of Resume
    pub picker_mode: bool,
    /// Last known tree area visible height (set from frame after draw, used for scroll calculations)
    pub last_tree_visible_height: usize,
}

impl App {
    pub fn new(search_paths: Vec<String>) -> Self {
        // Create channels for async search
        let (result_tx, result_rx) = mpsc::channel::<BackgroundSearchResult>();
        let (query_tx, query_rx) = mpsc::channel::<(u64, String, Vec<String>, bool)>();

        // Spawn background search thread
        thread::spawn(move || {
            while let Ok((seq, query, paths, use_regex)) = query_rx.recv() {
                let result = search_multiple_paths(&query, &paths, use_regex);
                let _ = result_tx.send(BackgroundSearchResult {
                    seq,
                    query,
                    paths,
                    use_regex,
                    result,
                });
            }
        });

        // Detect current project path for Ctrl+A filter
        let current_project_paths = std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.to_str().map(encode_path_for_claude))
            .map(|encoded| {
                search_paths
                    .iter()
                    .filter_map(|base| {
                        let candidate = format!("{}/{}", base, encoded);
                        if std::path::Path::new(&candidate).is_dir() {
                            Some(candidate)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let all_search_paths = search_paths.clone();

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
                searching: false,
                error: None,
                latest_chains: HashMap::new(),
                search_rx: result_rx,
                search_tx: query_tx,
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
            tree_mode: false,
            project_filter: false,
            automation_filter: AutomationFilter::Manual,
            automation_cache: HashMap::new(),
            all_search_paths,
            current_project_paths,
            recent,
            ai: AiState {
                active: false,
                query: InputState::new(),
                thinking: false,
                result_rx: None,
                error: None,
                ranked_count: None,
                original_recent_order: None,
                original_groups_order: None,
            },
            picker_mode: false,
            last_tree_visible_height: 20,
        }
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

    pub fn on_backspace(&mut self) {
        if self.input.backspace() {
            self.typing = true;
            self.last_keystroke = Some(Instant::now());
        }
    }

    pub fn on_delete(&mut self) {
        if self.input.delete_forward() {
            self.typing = true;
            self.last_keystroke = Some(Instant::now());
        }
    }

    /// Determine the outcome of the TUI session based on app state after the loop exits.
    pub fn into_outcome(self) -> TuiOutcome {
        match self.outcome {
            Some(AppOutcome::Pick(picked)) => TuiOutcome::Pick(picked),
            Some(AppOutcome::Resume(target)) => {
                ccs_debug!(
                    "[ccs:into_outcome] session_id={}, file_path={}, source={:?}, uuid={:?}",
                    target.session_id,
                    target.file_path,
                    target.source,
                    target.uuid
                );
                TuiOutcome::Resume {
                    session_id: target.session_id,
                    file_path: target.file_path,
                    source: target.source,
                    uuid: target.uuid,
                    query: self.input.into_text(),
                }
            }
            None => TuiOutcome::Quit,
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
        self.search.searching = false;
        self.search.error = None;
        if let Some(flag) = self.search.message_count_cancel.take() {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
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
        if self.input.delete_word_left() {
            self.typing = true;
            self.last_keystroke = Some(Instant::now());
        }
    }

    pub fn delete_word_right(&mut self) {
        if self.input.delete_word_right() {
            self.typing = true;
            self.last_keystroke = Some(Instant::now());
        }
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
        }
    }

    /// Dispatch a `KeyAction` to the appropriate handler.
    pub fn handle_action(&mut self, action: KeyAction) {
        // While AI mode is active, route text-editing keys to the AI query buffer
        if self.ai.active {
            match action {
                KeyAction::InputChar(c) => {
                    self.ai.query.push_char(c);
                    self.invalidate_ai_rank();
                    return;
                }
                KeyAction::Backspace => {
                    if self.ai.query.backspace() {
                        self.invalidate_ai_rank();
                    }
                    return;
                }
                KeyAction::Delete => {
                    if self.ai.query.delete_forward() {
                        self.invalidate_ai_rank();
                    }
                    return;
                }
                KeyAction::ClearInput => {
                    if !self.ai.query.is_empty() {
                        self.ai.query.clear();
                        self.invalidate_ai_rank();
                    }
                    return;
                }
                KeyAction::DeleteWordLeft => {
                    if self.ai.query.delete_word_left() {
                        self.invalidate_ai_rank();
                    }
                    return;
                }
                KeyAction::DeleteWordRight => {
                    if self.ai.query.delete_word_right() {
                        self.invalidate_ai_rank();
                    }
                    return;
                }
                KeyAction::MoveWordLeft => {
                    self.ai.query.move_word_left();
                    return;
                }
                KeyAction::MoveWordRight => {
                    self.ai.query.move_word_right();
                    return;
                }
                KeyAction::MoveHome => {
                    self.ai.query.move_home();
                    return;
                }
                KeyAction::MoveEnd => {
                    self.ai.query.move_end();
                    return;
                }
                KeyAction::Left => {
                    self.ai.query.move_left();
                    return;
                }
                KeyAction::Right => {
                    self.ai.query.move_right();
                    return;
                }
                KeyAction::Enter => {
                    if self.ai.ranked_count.is_some() {
                        self.on_enter_inner();
                    } else {
                        self.submit_ai_query();
                    }
                    return;
                }
                _ => {} // fall through for Up/Down navigation, Esc, Ctrl+G, etc.
            }
        }

        match action {
            KeyAction::Quit => self.should_quit = true,

            // Search mode: navigation
            KeyAction::Up => self.on_up(),
            KeyAction::Down => self.on_down(),
            KeyAction::Left => self.on_left(),
            KeyAction::Right => self.on_right(),
            KeyAction::Tab => self.on_tab(),
            KeyAction::Enter => self.on_enter(),

            // Search mode: editing
            KeyAction::InputChar(c) => self.on_key(c),
            KeyAction::Backspace => self.on_backspace(),
            KeyAction::Delete => self.on_delete(),
            KeyAction::ClearInput => self.clear_input(),
            KeyAction::DeleteWordLeft => self.delete_word_left(),
            KeyAction::DeleteWordRight => self.delete_word_right(),

            // Search mode: cursor movement
            KeyAction::MoveWordLeft => self.move_cursor_word_left(),
            KeyAction::MoveWordRight => self.move_cursor_word_right(),
            KeyAction::MoveHome => self.move_cursor_home(),
            KeyAction::MoveEnd => self.move_cursor_end(),

            // Search mode: toggles
            KeyAction::ToggleRegex => self.on_toggle_regex(),
            KeyAction::ToggleProjectFilter => self.toggle_project_filter(),
            KeyAction::ToggleAutomationFilter => self.toggle_automation_filter(),
            KeyAction::TogglePreview => self.on_tab(),
            KeyAction::ExitPreview => {
                self.preview_mode = false;
            }

            // AI mode
            KeyAction::EnterAiMode => self.enter_ai_mode(),
            KeyAction::ExitAiMode => self.exit_ai_mode(),

            // Search mode: tree entry
            KeyAction::EnterTreeMode => self.enter_tree_mode(),
            KeyAction::EnterTreeModeRecent => self.enter_tree_mode_recent(),

            // Tree mode
            KeyAction::TreeUp => self.on_up_tree(),
            KeyAction::TreeDown => self.on_down_tree(),
            KeyAction::TreeLeft => self.on_left_tree(),
            KeyAction::TreeRight => self.on_right_tree(),
            KeyAction::TreeTab => self.on_tab_tree(),
            KeyAction::TreeEnter => self.on_enter_tree(),
            KeyAction::ExitTreeMode => self.exit_tree_mode(),

            KeyAction::Noop => {}
        }
    }

    pub fn tick(&mut self) {
        let (global_loaded, project_loaded) = self.recent.poll();
        if global_loaded {
            apply_recent_automation_to_groups(
                &mut self.search.all_groups,
                &self.recent.all,
                &mut self.automation_cache,
            );
            // Freeze the filtered lists while an AI rank is applied or
            // in flight. Rebuilding an applied rank destroys its order;
            // rebuilding while a rank is in flight makes `handle_ai_result`
            // sort a different list than the one `submit_ai_query`
            // snapshotted, so the delivered rank lands on a mismatched
            // candidate set. After `invalidate_ai_rank` clears both
            // `ranked_count` and `thinking`, the refreshed data can reach
            // `filtered` / `groups` so the next `submit_ai_query` snapshot
            // reflects the new candidate set.
            if self.ai.ranked_count.is_none() && !self.ai.thinking {
                self.apply_groups_filter();
                self.apply_recent_sessions_filter();
            }
        }
        if project_loaded && self.ai.ranked_count.is_none() && !self.ai.thinking {
            self.apply_recent_sessions_filter();
        }

        // Check for tree load results
        if let Some(ref rx) = self.tree.tree_load_rx {
            if let Ok(result) = rx.try_recv() {
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
        }

        // Poll background message counts. Update the count in both the
        // `all_groups` store and the currently-filtered `groups` view in
        // place — a `message_count` update never changes which groups pass
        // the automation filter, so cloning `all_groups` via
        // `apply_groups_filter` on every trickled update is pure waste. In
        // debug builds that clone was dominating per-keystroke latency when
        // scrolling while background count workers were still finishing.
        if let Some(ref rx) = self.search.message_count_rx {
            while let Ok((file_path, count, compacted)) = rx.try_recv() {
                for group in &mut self.search.all_groups {
                    if group.file_path == file_path {
                        group.message_count = Some(count);
                        group.message_count_compacted = compacted;
                    }
                }
                for group in &mut self.search.groups {
                    if group.file_path == file_path {
                        group.message_count = Some(count);
                        group.message_count_compacted = compacted;
                    }
                }
            }
        }

        // Check for AI ranking result
        if let Some(ref rx) = self.ai.result_rx {
            if let Ok(result) = rx.try_recv() {
                self.ai.result_rx = None;
                self.ai.thinking = false;
                self.handle_ai_result(result);
            }
        }

        // Check for search results from background thread
        while let Ok(result) = self.search.search_rx.try_recv() {
            self.handle_search_result(result);
        }

        // Check if debounce period passed
        if let Some(last) = self.last_keystroke {
            if last.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                self.last_keystroke = None;
                self.typing = false;

                // Re-search if query, regex mode, or search scope changed
                let query_changed = self.input.text() != self.last_query;
                let mode_changed = self.regex_mode != self.last_regex_mode;
                let scope_changed = self.search_paths != self.last_search_paths;
                if query_changed && self.input.is_empty() {
                    // User backspaced to empty — reset to idle state
                    self.reset_search_state();
                } else if !self.input.is_empty() && (query_changed || mode_changed || scope_changed)
                {
                    self.start_search();
                }
            }
        }
    }

    pub(crate) fn handle_search_result(
        &mut self,
        BackgroundSearchResult {
            seq,
            query,
            paths,
            use_regex,
            result,
        }: BackgroundSearchResult,
    ) {
        // Ignore stale async results if query text, mode, path scope, or request sequence changed.
        if seq != self.search.search_seq
            || query != self.input.text()
            || use_regex != self.regex_mode
            || paths != self.search_paths
        {
            return;
        }

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
                // Freeze `groups` while an AI rank is applied or in
                // flight. Rebuilding an applied rank destroys its order;
                // rebuilding while a rank is in flight makes
                // `handle_ai_result` sort a different list than the one
                // `submit_ai_query` snapshotted. `invalidate_ai_rank`
                // clears both flags, after which the new search results
                // can land in `groups` for the next submit.
                if self.ai.ranked_count.is_none() && !self.ai.thinking {
                    self.apply_groups_filter();
                }
                self.search.results_count = count;
                self.search.group_cursor = 0;
                self.search.sub_cursor = 0;
                self.search.expanded = false;
                self.search.error = None;
                self.search.latest_chains.clear();
                self.search.searching = false;

                // Spawn background thread to count total messages per session file
                if let Some(flag) = self.search.message_count_cancel.take() {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
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
                    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let cancel_clone = cancel.clone();
                    std::thread::spawn(move || {
                        for fp in file_paths {
                            if cancel_clone.load(std::sync::atomic::Ordering::Relaxed) {
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
                self.search.searching = false;
                self.search.search_truncated = false;
            }
        }
    }

    pub(crate) fn apply_recent_sessions_filter(&mut self) {
        self.recent.apply_filter(
            self.project_filter,
            &self.current_project_paths,
            &self.automation_filter,
        );
    }

    /// Rebuild `groups` from `all_groups` based on automation filter.
    pub(crate) fn apply_groups_filter(&mut self) {
        self.search.groups = match self.automation_filter {
            AutomationFilter::All => self.search.all_groups.clone(),
            AutomationFilter::Manual => self
                .search
                .all_groups
                .iter()
                .filter(|g| g.automation.is_none())
                .cloned()
                .collect(),
            AutomationFilter::Auto => self
                .search
                .all_groups
                .iter()
                .filter(|g| g.automation.is_some())
                .cloned()
                .collect(),
        };
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
        self.ai.thinking = false;
        self.ai.query.clear();
        self.ai.error = None;
        self.ai.ranked_count = None;
        // Drop preview: otherwise on_enter_inner's preview-close branch
        // swallows the first post-rank Enter instead of resuming.
        self.preview_mode = false;
    }

    /// Drop any applied or in-flight AI rank state so the next Enter re-ranks.
    /// `thinking` must be cleared too: if a rank was in flight, dropping
    /// `result_rx` orphans the background thread (its send will fail), and
    /// `tick()` will never observe a result to reset the flag — leaving
    /// `submit_ai_query` stuck on its `thinking` guard.
    pub(crate) fn invalidate_ai_rank(&mut self) {
        self.ai.error = None;
        self.ai.ranked_count = None;
        self.ai.result_rx = None;
        self.ai.thinking = false;
    }

    pub fn exit_ai_mode(&mut self) {
        self.ai.active = false;
        self.ai.thinking = false;
        self.ai.result_rx = None;
        self.ai.error = None;
        self.ai.ranked_count = None;
        // Re-apply filters from current data instead of restoring a potentially
        // stale snapshot (filters or background loads may have changed while in AI mode).
        if self.ai.original_recent_order.take().is_some() {
            self.apply_recent_sessions_filter();
            self.recent.cursor = 0;
            self.recent.scroll_offset = 0;
        }
        if self.ai.original_groups_order.take().is_some() {
            self.apply_groups_filter();
            self.search.group_cursor = 0;
        }
    }

    fn submit_ai_query(&mut self) {
        if self.ai.query.is_empty() || self.ai.thinking {
            return;
        }
        let query = self.ai.query.text().to_string();

        // Snapshot lightweight session descriptors — no file I/O on main thread
        let sessions: Vec<crate::ai::SessionInfo> = if self.in_recent_sessions_mode() {
            self.recent
                .filtered
                .iter()
                .map(|s| crate::ai::SessionInfo {
                    file_path: s.file_path.clone(),
                    session_id: s.session_id.clone(),
                    project: s.project.clone(),
                    summary: s.summary.clone(),
                })
                .collect()
        } else {
            self.search
                .groups
                .iter()
                .map(|g| crate::ai::SessionInfo {
                    file_path: g.file_path.clone(),
                    session_id: g.session_id.clone(),
                    project: crate::search::extract_project_from_path(&g.file_path),
                    summary: String::new(),
                })
                .collect()
        };

        if sessions.is_empty() {
            return;
        }

        match crate::ai::spawn_ai_rank(query, sessions) {
            Ok(rx) => {
                self.ai.thinking = true;
                self.ai.error = None;
                self.ai.result_rx = Some(rx);
            }
            Err(e) => {
                self.ai.error = Some(e);
            }
        }
    }

    fn handle_ai_result(&mut self, result: crate::ai::AiRankResult) {
        if let Some(err) = result.error {
            self.ai.error = Some(err);
            return;
        }
        if result.ranked_ids.is_empty() {
            self.ai.error = Some(crate::ai::AI_NO_RELEVANT_SESSIONS_MSG.to_string());
            self.ai.ranked_count = None;
            return;
        }

        self.ai.error = None;

        let rank: std::collections::HashMap<&str, usize> = result
            .ranked_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();

        if self.in_recent_sessions_mode() {
            if self.ai.original_recent_order.is_none() {
                self.ai.original_recent_order = Some(self.recent.filtered.clone());
            }
            self.recent.filtered.sort_by_key(|s| {
                rank.get(s.session_id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            self.recent.cursor = 0;
            self.recent.scroll_offset = 0;
        } else {
            if self.ai.original_groups_order.is_none() {
                self.ai.original_groups_order = Some(self.search.groups.clone());
            }
            self.search.groups.sort_by_key(|g| {
                rank.get(g.session_id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            self.search.group_cursor = 0;
        }

        let matched_count = if self.in_recent_sessions_mode() {
            self.recent
                .filtered
                .iter()
                .filter(|s| rank.contains_key(s.session_id.as_str()))
                .count()
        } else {
            self.search
                .groups
                .iter()
                .filter(|g| rank.contains_key(g.session_id.as_str()))
                .count()
        };
        self.ai.ranked_count = Some(matched_count);
    }

    /// Start an async search in the background thread
    pub(crate) fn start_search(&mut self) {
        self.search.search_seq += 1;
        self.last_query = self.input.text().to_string();
        self.last_regex_mode = self.regex_mode;
        self.last_search_paths = self.search_paths.clone();
        self.search.searching = true;
        self.search.search_truncated = false;
        let _ = self.search.search_tx.send((
            self.search.search_seq,
            self.input.text().to_string(),
            self.search_paths.clone(),
            self.regex_mode,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Message, RipgrepMatch};
    use chrono::Utc;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_recent_session(file_path: &str) -> RecentSession {
        RecentSession {
            session_id: file_path.to_string(),
            file_path: file_path.to_string(),
            project: "proj".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc::now(),
            summary: "summary".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
        }
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

    #[test]
    fn test_handle_search_result_reuses_recent_session_automation() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input.set_text("later");
        app.search.search_seq = 1;
        app.recent.all = vec![RecentSession {
            session_id: "auto-session".to_string(),
            file_path: "/sessions/auto-session.jsonl".to_string(),
            project: "proj".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc::now(),
            summary: "summary".to_string(),
            automation: Some("ralphex".to_string()),
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
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
            source: SessionSource::ClaudeCodeCLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "later".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
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
            source: SessionSource::ClaudeCodeCLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "reply".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
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
            source: SessionSource::ClaudeCodeCLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "detekt".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
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
        app.input.set_text("query");
        app.regex_mode = false;

        // First result: truncated
        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "query".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
            result: Ok(crate::search::SearchResult {
                matches: vec![],
                truncated: true,
            }),
        });
        assert!(app.search.search_truncated);

        // Second result: not truncated — flag must clear
        app.search.search_seq = 2;
        app.handle_search_result(BackgroundSearchResult {
            seq: 2,
            query: "query".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
            result: Ok(crate::search::SearchResult {
                matches: vec![],
                truncated: false,
            }),
        });
        assert!(!app.search.search_truncated);
    }

    #[test]
    fn test_path_is_within_project_rejects_sibling_prefixes() {
        assert!(path_is_within_project(
            r"C:\Users\test\project\session.jsonl",
            r"C:/Users/test/project"
        ));
        assert!(!path_is_within_project(
            r"C:\Users\test\project-other\session.jsonl",
            r"C:/Users/test/project"
        ));
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
        app.search.searching = true;
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
        assert!(!app.search.searching, "searching should be false");
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
        app.search.searching = true;
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
        assert!(!app.search.searching, "searching should be false");
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
            source: SessionSource::ClaudeCodeCLI,
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
            source: SessionSource::ClaudeCodeCLI,
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
            source: SessionSource::ClaudeCodeCLI,
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
    fn test_into_outcome_quit_when_no_selection() {
        let app = App::new(vec!["/test".to_string()]);
        assert_eq!(app.into_outcome(), TuiOutcome::Quit);
    }

    #[test]
    fn test_into_outcome_resume_when_outcome_set() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.outcome = Some(AppOutcome::Resume(ResumeTarget {
            session_id: "sess-1".to_string(),
            file_path: "/path/to/session.jsonl".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            uuid: Some("uuid-42".to_string()),
        }));
        app.input.set_text("my search query");

        let outcome = app.into_outcome();
        assert_eq!(
            outcome,
            TuiOutcome::Resume {
                session_id: "sess-1".to_string(),
                file_path: "/path/to/session.jsonl".to_string(),
                source: SessionSource::ClaudeCodeCLI,
                uuid: Some("uuid-42".to_string()),
                query: "my search query".to_string(),
            }
        );
    }

    #[test]
    fn test_into_outcome_pick_when_outcome_set() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;
        app.outcome = Some(AppOutcome::Pick(PickedSession {
            session_id: "pick-1".to_string(),
            file_path: "/pick/session.jsonl".to_string(),
            source: SessionSource::ClaudeDesktop,
            project: "my-project".to_string(),
            message_uuid: None,
        }));

        let outcome = app.into_outcome();
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
    fn test_into_outcome_quit_when_picker_mode_no_selection() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;
        // No outcome set (user pressed Esc)
        assert_eq!(app.into_outcome(), TuiOutcome::Quit);
    }

    #[test]
    fn test_into_outcome_pick_variant() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.outcome = Some(AppOutcome::Pick(PickedSession {
            session_id: "pick-1".to_string(),
            file_path: "/pick/session.jsonl".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            project: "proj".to_string(),
            message_uuid: None,
        }));

        match app.into_outcome() {
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

    // Sentinel test for the AI re-ranking Enter-resume flow (plan 2026-04-20).
    //
    // The AI-branch of `handle_action` currently routes `KeyAction::Enter`
    // unconditionally to `submit_ai_query()`, which makes it impossible to
    // fall through to a selected session once a rank has been applied. Even
    // if Enter were routed to `on_enter()`, the `if self.ai.active { return; }`
    // guard at the top of `on_enter` would swallow it.
    //
    // Task 2 extracts `on_enter_inner()` (no guard), and Task 3 routes the
    // AI-branch Enter to `on_enter_inner()` when `ranked_count.is_some()`.
    // Both have landed — the test is now an active regression.
    #[test]
    fn ai_mode_enter_with_ranked_count_triggers_resume() {
        use crate::tui::dispatch::KeyAction;

        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        app.recent.filtered = vec![make_recent_session("/sessions/sess.jsonl")];
        app.ai.ranked_count = Some(1);

        app.handle_action(KeyAction::Enter);

        match app.outcome {
            Some(AppOutcome::Resume(_)) => {}
            other => panic!(
                "Expected Resume outcome after Enter in AI mode with ranked_count=Some, got {:?}",
                other
            ),
        }
    }

    // Complement of the sentinel above: when AI mode is active but no rank has
    // been applied yet and the query is empty, Enter must route to
    // `submit_ai_query`, which itself no-ops (empty query guard). The outcome
    // stays `None` and `ai.thinking` stays `false` — no AI spawn is triggered.
    #[test]
    fn ai_mode_enter_without_ranked_count_triggers_submit_with_empty_query_is_noop() {
        use crate::tui::dispatch::KeyAction;

        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        app.recent.filtered = vec![make_recent_session("/sessions/sess.jsonl")];
        assert!(app.ai.ranked_count.is_none());
        assert!(app.ai.query.is_empty());

        app.handle_action(KeyAction::Enter);

        assert!(
            app.outcome.is_none(),
            "Expected no outcome when Enter triggers submit on empty query, got {:?}",
            app.outcome
        );
        assert!(
            !app.ai.thinking,
            "submit_ai_query must early-return on empty query — thinking should stay false"
        );
    }

    // Task 4 (plan 2026-04-20): every query-mutation branch in the AI block of
    // `handle_action` must clear `ranked_count` (so Enter routes back to
    // `submit_ai_query`), drop `result_rx` (so a stale in-flight AI response
    // cannot restore the "result applied" flag via `handle_ai_result`), and
    // clear `thinking` (otherwise `submit_ai_query` would be wedged on its
    // `thinking` guard since `tick()` can no longer observe a result).
    #[test]
    fn ai_query_mutation_clears_rank_and_receiver() {
        use crate::tui::dispatch::KeyAction;

        // For each mutation we pin (initial_text, cursor_pos, expected_text_after).
        let mutations: Vec<(KeyAction, &str, usize, &str)> = vec![
            (KeyAction::InputChar('x'), "hello", 5, "hellox"),
            (KeyAction::Backspace, "hello", 5, "hell"),
            (KeyAction::Delete, "hello", 0, "ello"),
            (KeyAction::ClearInput, "hello", 5, ""),
            (KeyAction::DeleteWordLeft, "one two", 7, "one "),
            (KeyAction::DeleteWordRight, "one two", 0, "two"),
        ];

        for (action, initial, cursor, expected) in mutations {
            let mut app = App::new(vec!["/test".to_string()]);
            app.enter_ai_mode();
            app.ai.query.set_text_and_cursor(initial, cursor);
            app.ai.ranked_count = Some(3);
            app.ai.thinking = true;
            let (_tx, rx) = mpsc::channel::<crate::ai::AiRankResult>();
            app.ai.result_rx = Some(rx);

            app.handle_action(action.clone());

            assert!(
                app.ai.ranked_count.is_none(),
                "ranked_count must be cleared after {:?}",
                action
            );
            assert!(
                app.ai.result_rx.is_none(),
                "result_rx must be dropped after {:?}",
                action
            );
            assert!(
                !app.ai.thinking,
                "thinking must be cleared after {:?} so submit_ai_query can run again",
                action
            );
            assert_eq!(
                app.ai.query.text(),
                expected,
                "query text must reflect mutation after {:?}",
                action
            );
        }
    }

    // Cursor-only movements must NOT clear `ranked_count` — the user is merely
    // navigating within the already-submitted query, not editing it, so the
    // applied rank stays valid and Enter must still route to resume.
    #[test]
    fn ai_cursor_movement_does_not_clear_rank() {
        use crate::tui::dispatch::KeyAction;

        let movements = [
            KeyAction::Left,
            KeyAction::Right,
            KeyAction::MoveHome,
            KeyAction::MoveEnd,
            KeyAction::MoveWordLeft,
            KeyAction::MoveWordRight,
        ];

        for action in movements {
            let mut app = App::new(vec!["/test".to_string()]);
            app.enter_ai_mode();
            app.ai.query.set_text("hello world");
            app.ai.ranked_count = Some(3);

            app.handle_action(action.clone());

            assert_eq!(
                app.ai.ranked_count,
                Some(3),
                "ranked_count must stay intact after cursor-only {:?}",
                action
            );
        }
    }

    // No-op delete keys (cursor at edge, or empty query for ClearInput) must
    // not clear ranked_count — otherwise a stray Backspace while cursor is at
    // pos 0 silently demotes a valid rank back to "rank" mode.
    #[test]
    fn ai_query_noop_delete_keeps_rank() {
        use crate::tui::dispatch::KeyAction;

        // (action, initial_text, cursor_pos) tuples where the action is a no-op.
        let noops: Vec<(KeyAction, &str, usize)> = vec![
            (KeyAction::Backspace, "hello", 0),
            (KeyAction::Delete, "hello", 5),
            (KeyAction::ClearInput, "", 0),
            (KeyAction::DeleteWordLeft, "hello", 0),
            (KeyAction::DeleteWordRight, "hello", 5),
        ];

        for (action, initial, cursor) in noops {
            let mut app = App::new(vec!["/test".to_string()]);
            app.enter_ai_mode();
            app.ai.query.set_text_and_cursor(initial, cursor);
            app.ai.ranked_count = Some(3);

            app.handle_action(action.clone());

            assert_eq!(
                app.ai.ranked_count,
                Some(3),
                "ranked_count must stay intact after no-op {:?}",
                action
            );
            assert_eq!(
                app.ai.query.text(),
                initial,
                "query text must be unchanged after no-op {:?}",
                action
            );
        }
    }

    // Filter/scope toggles (Ctrl+R, Ctrl+H, Ctrl+A) rebuild the candidate list
    // via `apply_recent_sessions_filter` / `apply_groups_filter`, so any rank
    // applied to the prior list is stale against the new one. Without
    // invalidation, Enter would route to `on_enter_inner` (resume) using the
    // freshly-filtered selection's index against a rank tied to a different
    // set — surprising the user by resuming rather than re-ranking.
    #[test]
    fn ai_toggle_regex_clears_rank() {
        let mut app = App::new(vec!["/test".to_string()]);
        let initial_regex = app.regex_mode;
        app.enter_ai_mode();
        app.ai.query.set_text("preserve me");
        app.ai.ranked_count = Some(5);
        app.ai.thinking = true;
        let (_tx, rx) = mpsc::channel::<crate::ai::AiRankResult>();
        app.ai.result_rx = Some(rx);

        app.on_toggle_regex();

        assert!(
            app.ai.ranked_count.is_none(),
            "ranked_count must be cleared after on_toggle_regex"
        );
        assert!(
            app.ai.result_rx.is_none(),
            "result_rx must be dropped after on_toggle_regex"
        );
        assert!(
            !app.ai.thinking,
            "thinking must be cleared after on_toggle_regex"
        );
        assert_ne!(
            app.regex_mode, initial_regex,
            "on_toggle_regex must still flip regex_mode — invalidation must not short-circuit the toggle"
        );
        assert!(
            app.ai.active,
            "ai.active must stay true — invalidate_ai_rank must not exit AI mode"
        );
        assert_eq!(
            app.ai.query.text(),
            "preserve me",
            "ai.query text must survive a toggle so the next Enter re-submits the same query"
        );
    }

    #[test]
    fn ai_toggle_automation_filter_clears_rank() {
        let mut app = App::new(vec!["/test".to_string()]);
        let initial_filter = app.automation_filter;
        app.enter_ai_mode();
        app.ai.query.set_text("preserve me");
        app.ai.ranked_count = Some(5);
        app.ai.thinking = true;
        let (_tx, rx) = mpsc::channel::<crate::ai::AiRankResult>();
        app.ai.result_rx = Some(rx);

        app.toggle_automation_filter();

        assert!(
            app.ai.ranked_count.is_none(),
            "ranked_count must be cleared after toggle_automation_filter"
        );
        assert!(
            app.ai.result_rx.is_none(),
            "result_rx must be dropped after toggle_automation_filter"
        );
        assert!(
            !app.ai.thinking,
            "thinking must be cleared after toggle_automation_filter"
        );
        assert_ne!(
            app.automation_filter, initial_filter,
            "toggle_automation_filter must still advance the cycle — invalidation must not short-circuit it"
        );
        assert!(
            app.ai.active,
            "ai.active must stay true — invalidate_ai_rank must not exit AI mode"
        );
        assert_eq!(
            app.ai.query.text(),
            "preserve me",
            "ai.query text must survive a toggle so the next Enter re-submits the same query"
        );
    }

    #[test]
    fn ai_toggle_project_filter_clears_rank() {
        let mut app = App::new(vec!["/test".to_string()]);
        // Must be non-empty, else `toggle_project_filter` early-returns before
        // reaching the invalidation guard.
        app.current_project_paths = vec!["/test/project".to_string()];
        let initial_project = app.project_filter;
        app.enter_ai_mode();
        app.ai.query.set_text("preserve me");
        app.ai.ranked_count = Some(5);
        app.ai.thinking = true;
        let (_tx, rx) = mpsc::channel::<crate::ai::AiRankResult>();
        app.ai.result_rx = Some(rx);

        app.toggle_project_filter();

        assert!(
            app.ai.ranked_count.is_none(),
            "ranked_count must be cleared after toggle_project_filter"
        );
        assert!(
            app.ai.result_rx.is_none(),
            "result_rx must be dropped after toggle_project_filter"
        );
        assert!(
            !app.ai.thinking,
            "thinking must be cleared after toggle_project_filter"
        );
        assert_ne!(
            app.project_filter, initial_project,
            "toggle_project_filter must still flip project_filter — invalidation must not short-circuit the toggle"
        );
        assert!(
            app.ai.active,
            "ai.active must stay true — invalidate_ai_rank must not exit AI mode"
        );
        assert_eq!(
            app.ai.query.text(),
            "preserve me",
            "ai.query text must survive a toggle so the next Enter re-submits the same query"
        );
    }

    // Entering AI mode with an open preview must close it — otherwise the
    // first post-rank Enter is consumed by on_enter_inner's preview-close
    // branch instead of resuming the selected session.
    #[test]
    fn enter_ai_mode_closes_preview() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.preview_mode = true;

        app.enter_ai_mode();

        assert!(
            !app.preview_mode,
            "enter_ai_mode must clear preview_mode so post-rank Enter resumes instead of closing preview"
        );
    }

    // After a toggle invalidates the rank in AI mode, a subsequent async
    // search result must refresh `search.groups`. Otherwise the next
    // `submit_ai_query` re-ranks the old AI-ordered list instead of the
    // newly-filtered candidates. Guards the invariant behind the fix for
    // Codex's follow-up finding on the async refresh skip.
    #[test]
    fn ai_handle_search_result_refreshes_groups_after_invalidation() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        // Simulate a post-invalidation state: rank was dropped by a Ctrl+R
        // toggle, but `ai.active` stays true.
        app.ai.ranked_count = None;
        app.input.set_text("query");
        app.search.search_seq = 1;
        // Pre-populate `search.groups` to emulate the stale AI-ranked list
        // that was visible when the user pressed Ctrl+R.
        app.search.groups = vec![SessionGroup {
            session_id: "stale".to_string(),
            file_path: "/sessions/stale.jsonl".to_string(),
            matches: vec![],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];

        let fresh = RipgrepMatch {
            file_path: "/sessions/fresh.jsonl".to_string(),
            message: Some(Message {
                session_id: "fresh".to_string(),
                role: "user".to_string(),
                content: "query".to_string(),
                timestamp: Utc::now(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::ClaudeCodeCLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "query".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
            result: Ok(crate::search::SearchResult {
                matches: vec![fresh],
                truncated: false,
            }),
        });

        assert_eq!(
            app.search.groups.len(),
            1,
            "groups must be rebuilt from fresh `all_groups` after invalidation"
        );
        assert_eq!(
            app.search.groups[0].session_id, "fresh",
            "visible groups must show the new search results, not the stale AI-ranked list"
        );
    }

    // Complement: while an AI rank is applied (`ranked_count=Some`), an
    // async search result must NOT rebuild `search.groups`, because doing
    // so would destroy the AI-ranked ordering the user is looking at.
    #[test]
    fn ai_handle_search_result_preserves_groups_when_rank_applied() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        app.ai.ranked_count = Some(1);
        app.input.set_text("query");
        app.search.search_seq = 1;
        let ranked = SessionGroup {
            session_id: "ranked".to_string(),
            file_path: "/sessions/ranked.jsonl".to_string(),
            matches: vec![],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        };
        app.search.groups = vec![ranked.clone()];

        let fresh = RipgrepMatch {
            file_path: "/sessions/fresh.jsonl".to_string(),
            message: Some(Message {
                session_id: "fresh".to_string(),
                role: "user".to_string(),
                content: "query".to_string(),
                timestamp: Utc::now(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::ClaudeCodeCLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "query".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
            result: Ok(crate::search::SearchResult {
                matches: vec![fresh],
                truncated: false,
            }),
        });

        assert_eq!(
            app.search.groups.len(),
            1,
            "groups must keep the AI-ranked list while `ranked_count` is Some"
        );
        assert_eq!(
            app.search.groups[0].session_id, "ranked",
            "AI-ranked group must not be replaced by the fresh search result"
        );
    }

    // While a submitted AI rank is still in flight (`thinking=true`,
    // `ranked_count=None`), an async search result must NOT rebuild
    // `search.groups`. `submit_ai_query` snapshotted session IDs from the
    // pre-refresh `groups`; if the list rebuilds now, the eventual
    // `handle_ai_result` sorts a different list than the one the model saw.
    #[test]
    fn ai_handle_search_result_preserves_groups_while_rank_in_flight() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        // Simulate submitted query waiting on the model: rank not yet
        // applied, result channel attached, thinking flag raised.
        app.ai.ranked_count = None;
        app.ai.thinking = true;
        let (_tx, rx) = mpsc::channel::<crate::ai::AiRankResult>();
        app.ai.result_rx = Some(rx);
        app.input.set_text("query");
        app.search.search_seq = 1;
        let snapshotted = SessionGroup {
            session_id: "snapshotted".to_string(),
            file_path: "/sessions/snapshotted.jsonl".to_string(),
            matches: vec![],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        };
        app.search.groups = vec![snapshotted.clone()];

        let fresh = RipgrepMatch {
            file_path: "/sessions/fresh.jsonl".to_string(),
            message: Some(Message {
                session_id: "fresh".to_string(),
                role: "user".to_string(),
                content: "query".to_string(),
                timestamp: Utc::now(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::ClaudeCodeCLI,
        };

        app.handle_search_result(BackgroundSearchResult {
            seq: 1,
            query: "query".to_string(),
            paths: app.search_paths.clone(),
            use_regex: false,
            result: Ok(crate::search::SearchResult {
                matches: vec![fresh],
                truncated: false,
            }),
        });

        assert_eq!(
            app.search.groups.len(),
            1,
            "groups must keep the snapshotted list while an AI rank is in flight"
        );
        assert_eq!(
            app.search.groups[0].session_id, "snapshotted",
            "in-flight snapshot must not be replaced by the fresh search result"
        );
        assert_eq!(
            app.search.all_groups.len(),
            1,
            "all_groups must still absorb the fresh search result"
        );
        assert_eq!(
            app.search.all_groups[0].session_id, "fresh",
            "all_groups stores the fresh data for post-invalidation refresh"
        );
    }

    // Complement for the tick path: while a rank is in flight, a
    // completing project load must NOT rebuild `recent.filtered` — doing so
    // would swap the snapshot out from under the in-flight `handle_ai_result`.
    #[test]
    fn ai_tick_preserves_recent_filtered_while_rank_in_flight() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        app.ai.ranked_count = None;
        app.ai.thinking = true;
        let (_result_tx, result_rx) = mpsc::channel::<crate::ai::AiRankResult>();
        app.ai.result_rx = Some(result_rx);
        app.project_filter = true;
        app.current_project_paths = vec!["/proj".to_string()];
        let snapshotted = RecentSession {
            session_id: "snapshotted".to_string(),
            file_path: "/proj/snapshotted.jsonl".to_string(),
            project: "proj".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc::now(),
            summary: "snapshotted".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
        };
        app.recent.filtered = vec![snapshotted.clone()];

        let fresh_session = RecentSession {
            session_id: "fresh".to_string(),
            file_path: "/proj/fresh.jsonl".to_string(),
            project: "proj".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc::now(),
            summary: "fresh project session".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
        };
        let (tx, rx) = mpsc::channel::<Vec<RecentSession>>();
        tx.send(vec![fresh_session]).unwrap();
        app.recent.project_load_rx = Some(rx);

        app.tick();

        assert_eq!(
            app.recent.filtered.len(),
            1,
            "recent.filtered must keep the snapshotted list while an AI rank is in flight"
        );
        assert_eq!(
            app.recent.filtered[0].session_id, "snapshotted",
            "in-flight snapshot must not be replaced by the freshly-loaded project data"
        );
        assert!(
            app.recent.project.is_some(),
            "project data must still be stored so a post-result refresh can use it"
        );
    }

    // After a Ctrl+A toggle invalidates the rank in AI mode, an async
    // project load completion must refresh `recent.filtered`, so the next
    // `submit_ai_query` uses the project-scoped candidate set instead of
    // the fallback-from-`recent.all` snapshot computed before the load.
    #[test]
    fn ai_tick_refreshes_recent_filtered_on_project_load_after_invalidation() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        app.ai.ranked_count = None;
        app.project_filter = true;
        app.current_project_paths = vec!["/proj".to_string()];

        // Simulate the project loader having produced a fresh list, ready
        // to be drained by the next `poll()` call.
        let fresh_session = RecentSession {
            session_id: "fresh".to_string(),
            file_path: "/proj/fresh.jsonl".to_string(),
            project: "proj".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc::now(),
            summary: "fresh project session".to_string(),
            automation: None,
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
        };
        let (tx, rx) = mpsc::channel::<Vec<RecentSession>>();
        tx.send(vec![fresh_session]).unwrap();
        app.recent.project_load_rx = Some(rx);

        app.tick();

        assert_eq!(
            app.recent.filtered.len(),
            1,
            "recent.filtered must be rebuilt from the freshly-loaded project data"
        );
        assert_eq!(
            app.recent.filtered[0].session_id, "fresh",
            "filtered list must show the project-loaded session after invalidation"
        );
    }

    // A well-formed empty `[]` from the AI means "nothing matched", so AI
    // mode must stay retryable: no `ranked_count`, no resume-on-Enter, and
    // no saved original order snapshot.
    #[test]
    fn test_handle_ai_result_empty_rank_stays_retryable() {
        fn sg(id: &str) -> SessionGroup {
            SessionGroup {
                session_id: id.to_string(),
                file_path: format!("/sessions/{id}.jsonl"),
                matches: vec![],
                automation: None,
                message_count: None,
                message_count_compacted: false,
            }
        }

        let mut app = App::new(vec!["/test".to_string()]);
        app.enter_ai_mode();
        let original = vec![sg("a"), sg("b")];
        app.search.groups = original.clone();

        app.handle_ai_result(crate::ai::AiRankResult {
            ranked_ids: vec![],
            error: None,
        });

        assert_eq!(
            app.ai.ranked_count, None,
            "empty ranking must stay retryable so Enter re-ranks instead of resuming"
        );
        assert!(
            app.ai.error == Some(crate::ai::AI_NO_RELEVANT_SESSIONS_MSG.to_string()),
            "empty ranking must surface a retryable no-match message"
        );
        assert!(
            app.ai.original_groups_order.is_none(),
            "no-match result must not capture a restorable ranked snapshot"
        );
        assert_eq!(
            app.search
                .groups
                .iter()
                .map(|g| &g.session_id)
                .collect::<Vec<_>>(),
            original.iter().map(|g| &g.session_id).collect::<Vec<_>>(),
            "no-match result must leave visible ordering untouched"
        );
    }
}
