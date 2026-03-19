use crate::resume::encode_path_for_claude;
use crate::search::{
    group_by_session, search_multiple_paths, RipgrepMatch, SessionGroup, SessionSource,
};
use crate::tree::SessionTree;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const DEBOUNCE_MS: u64 = 300;

/// Result from background search thread:
/// (request seq, query, search paths, regex mode, search result)
pub(crate) type SearchResult = (
    u64,
    String,
    Vec<String>,
    bool,
    Result<Vec<RipgrepMatch>, String>,
);

pub struct App {
    pub input: String,
    pub results: Vec<RipgrepMatch>,
    pub groups: Vec<SessionGroup>,
    pub group_cursor: usize,
    pub sub_cursor: usize,
    pub expanded: bool,
    pub searching: bool,
    pub typing: bool,
    pub error: Option<String>,
    pub search_paths: Vec<String>,
    pub last_query: String,
    pub results_query: String,
    pub last_keystroke: Option<Instant>,
    pub preview_mode: bool,
    pub should_quit: bool,
    pub resume_id: Option<String>,
    pub resume_file_path: Option<String>,
    /// Session source for resume (CLI or Desktop)
    pub resume_source: Option<SessionSource>,
    /// UUID of the selected message (for branch-aware resume)
    pub resume_uuid: Option<String>,
    /// Flag to force a full terminal redraw (clears diff optimization artifacts)
    pub needs_full_redraw: bool,
    /// Regex search mode (Ctrl+R to toggle)
    pub regex_mode: bool,
    /// Track last regex mode used for search
    pub(crate) last_regex_mode: bool,
    /// Track last search path scope used for search
    pub(crate) last_search_paths: Vec<String>,
    /// Channel to receive search results from background thread
    pub(crate) search_rx: Receiver<SearchResult>,
    /// Channel to send search requests to background thread
    pub(crate) search_tx: Sender<(u64, String, Vec<String>, bool)>,
    /// Monotonic request sequence to ignore stale async results
    pub(crate) search_seq: u64,
    /// Cache: file_path → set of uuids on the latest chain (for fork indicator)
    pub latest_chains: HashMap<String, HashSet<String>>,
    /// Tree explorer mode
    pub tree_mode: bool,
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
    /// Cursor position in input (byte offset)
    pub cursor_pos: usize,
    /// Whether search is scoped to current project only (Ctrl+A toggle)
    pub project_filter: bool,
    /// All search paths (for "all sessions" mode)
    pub(crate) all_search_paths: Vec<String>,
    /// Search path(s) for current project only
    pub current_project_paths: Vec<String>,
}

impl App {
    pub fn new(search_paths: Vec<String>) -> Self {
        // Create channels for async search
        let (result_tx, result_rx) = mpsc::channel::<SearchResult>();
        let (query_tx, query_rx) = mpsc::channel::<(u64, String, Vec<String>, bool)>();

        // Spawn background search thread
        thread::spawn(move || {
            while let Ok((seq, query, paths, use_regex)) = query_rx.recv() {
                let result = search_multiple_paths(&query, &paths, use_regex);
                let result = (seq, query, paths, use_regex, result);
                let _ = result_tx.send(result);
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

        Self {
            input: String::new(),
            results: vec![],
            groups: vec![],
            group_cursor: 0,
            sub_cursor: 0,
            expanded: false,
            searching: false,
            typing: false,
            error: None,
            search_paths,
            last_query: String::new(),
            results_query: String::new(),
            last_keystroke: None,
            preview_mode: false,
            should_quit: false,
            resume_id: None,
            resume_file_path: None,
            resume_source: None,
            resume_uuid: None,
            needs_full_redraw: false,
            regex_mode: false,
            last_regex_mode: false,
            last_search_paths: all_search_paths.clone(),
            search_rx: result_rx,
            search_tx: query_tx,
            search_seq: 0,
            latest_chains: HashMap::new(),
            tree_mode: false,
            session_tree: None,
            tree_cursor: 0,
            tree_scroll_offset: 0,
            tree_loading: false,
            tree_load_rx: None,
            tree_mode_standalone: false,
            cursor_pos: 0,
            project_filter: false,
            all_search_paths,
            current_project_paths,
        }
    }

    pub fn on_key(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
        self.typing = true;
        self.last_keystroke = Some(Instant::now());
    }

    pub fn on_backspace(&mut self) {
        if self.cursor_pos > 0 {
            // Find the previous char boundary
            let prev = self.input[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.remove(prev);
            self.cursor_pos = prev;
            self.typing = true;
            self.last_keystroke = Some(Instant::now());
        }
    }

    pub fn on_delete(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.input.remove(self.cursor_pos);
            self.typing = true;
            self.last_keystroke = Some(Instant::now());
        }
    }

    /// Reset all search result state to idle (no results, no error, no status).
    /// Shared by `clear_input()` (Ctrl-C) and `tick()` (backspace-to-empty).
    fn reset_search_state(&mut self) {
        self.last_query.clear();
        self.results.clear();
        self.groups.clear();
        self.results_query.clear();
        self.group_cursor = 0;
        self.sub_cursor = 0;
        self.expanded = false;
        self.preview_mode = false;
        self.latest_chains.clear();
        self.searching = false;
        self.error = None;
    }

    /// Clear input and reset search state (Ctrl-C behavior)
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
        self.typing = false;
        self.last_keystroke = None;
        self.reset_search_state();
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.input[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos += self.input[self.cursor_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
        }
    }

    pub fn move_cursor_word_left(&mut self) {
        let bytes = self.input.as_bytes();
        let mut pos = self.cursor_pos;
        // Skip non-alphanumeric
        while pos > 0 && !bytes[pos - 1].is_ascii_alphanumeric() {
            pos -= 1;
        }
        // Skip alphanumeric
        while pos > 0 && bytes[pos - 1].is_ascii_alphanumeric() {
            pos -= 1;
        }
        self.cursor_pos = pos;
    }

    pub fn move_cursor_word_right(&mut self) {
        let bytes = self.input.as_bytes();
        let len = bytes.len();
        let mut pos = self.cursor_pos;
        // Skip alphanumeric
        while pos < len && bytes[pos].is_ascii_alphanumeric() {
            pos += 1;
        }
        // Skip non-alphanumeric
        while pos < len && !bytes[pos].is_ascii_alphanumeric() {
            pos += 1;
        }
        self.cursor_pos = pos;
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    pub fn delete_word_left(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let old_pos = self.cursor_pos;
        self.move_cursor_word_left();
        self.input.drain(self.cursor_pos..old_pos);
        self.typing = true;
        self.last_keystroke = Some(Instant::now());
    }

    pub fn delete_word_right(&mut self) {
        if self.cursor_pos >= self.input.len() {
            return;
        }
        let old_pos = self.cursor_pos;
        self.move_cursor_word_right();
        let new_pos = self.cursor_pos;
        self.cursor_pos = old_pos;
        self.input.drain(old_pos..new_pos);
        self.typing = true;
        self.last_keystroke = Some(Instant::now());
    }

    pub fn tick(&mut self) {
        // Check for tree load results
        if let Some(ref rx) = self.tree_load_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(tree) => {
                        self.session_tree = Some(tree);
                        self.tree_loading = false;
                        self.needs_full_redraw = true;
                    }
                    Err(e) => {
                        self.error = Some(format!("Tree load error: {}", e));
                        self.tree_loading = false;
                        self.tree_mode = false;
                        self.needs_full_redraw = true;
                    }
                }
                self.tree_load_rx = None;
            }
        }

        // Check for search results from background thread
        while let Ok(result) = self.search_rx.try_recv() {
            self.handle_search_result(result);
        }

        // Check if debounce period passed
        if let Some(last) = self.last_keystroke {
            if last.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                self.last_keystroke = None;
                self.typing = false;

                // Re-search if query, regex mode, or search scope changed
                let query_changed = self.input != self.last_query;
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
        (seq, query, paths, use_regex, result): SearchResult,
    ) {
        // Ignore stale async results if query text, mode, path scope, or request sequence changed.
        if seq != self.search_seq
            || query != self.input
            || use_regex != self.regex_mode
            || paths != self.search_paths
        {
            return;
        }

        match result {
            Ok(results) => {
                self.results_query = query;
                self.groups = group_by_session(results.clone());
                self.results = results;
                self.group_cursor = 0;
                self.sub_cursor = 0;
                self.expanded = false;
                self.error = None;
                self.latest_chains.clear();
                self.searching = false;
            }
            Err(e) => {
                self.error = Some(e);
                self.searching = false;
            }
        }
    }

    /// Start an async search in the background thread
    pub(crate) fn start_search(&mut self) {
        self.search_seq += 1;
        self.last_query = self.input.clone();
        self.last_regex_mode = self.regex_mode;
        self.last_search_paths = self.search_paths.clone();
        self.searching = true;
        let _ = self.search_tx.send((
            self.search_seq,
            self.input.clone(),
            self.search_paths.clone(),
            self.regex_mode,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_new() {
        let app = App::new(vec!["/test/path".to_string()]);

        assert_eq!(app.search_paths, vec!["/test/path".to_string()]);
        assert!(app.input.is_empty());
        assert!(app.groups.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn test_on_key() {
        let mut app = App::new(vec!["/test".to_string()]);

        app.on_key('h');
        app.on_key('e');
        app.on_key('l');
        app.on_key('l');
        app.on_key('o');

        assert_eq!(app.input, "hello");
        assert!(app.typing);
    }

    #[test]
    fn test_on_backspace() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello".to_string();
        app.cursor_pos = 5; // cursor at end

        app.on_backspace();

        assert_eq!(app.input, "hell");
        assert_eq!(app.cursor_pos, 4);
    }

    #[test]
    fn test_clear_input_resets_state() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Set up state as if a search has completed
        app.input = "hello".to_string();
        app.cursor_pos = 5;
        app.last_query = "hello".to_string();
        app.results_query = "hello".to_string();
        app.results = vec![RipgrepMatch {
            file_path: "/test/file.jsonl".to_string(),
            message: None,
            source: SessionSource::ClaudeCodeCLI,
        }];
        app.groups = vec![SessionGroup {
            session_id: "abc123".to_string(),
            file_path: "/test/file.jsonl".to_string(),
            matches: vec![],
        }];
        app.group_cursor = 1;
        app.sub_cursor = 2;
        app.expanded = true;
        app.searching = true;
        app.typing = true;
        app.last_keystroke = Some(Instant::now());
        app.latest_chains.insert("file".to_string(), HashSet::new());
        app.error = Some("stale error".to_string());
        app.preview_mode = true;

        app.clear_input();

        assert!(app.input.is_empty(), "input should be cleared");
        assert!(!app.typing, "typing should be false");
        assert!(
            app.last_keystroke.is_none(),
            "last_keystroke should be None"
        );
        assert!(!app.searching, "searching should be false");
        assert!(app.last_query.is_empty(), "last_query should be cleared");
        assert!(app.results.is_empty(), "results should be cleared");
        assert!(app.groups.is_empty(), "groups should be cleared");
        assert!(
            app.results_query.is_empty(),
            "results_query should be cleared"
        );
        assert_eq!(app.group_cursor, 0, "group_cursor should be reset");
        assert_eq!(app.sub_cursor, 0, "sub_cursor should be reset");
        assert!(!app.expanded, "expanded should be reset");
        assert!(
            app.latest_chains.is_empty(),
            "latest_chains should be cleared"
        );
        assert!(app.error.is_none(), "error should be cleared");
        assert!(!app.preview_mode, "preview_mode should be reset");
    }

    #[test]
    fn test_ctrl_c_empty_input_should_quit() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Empty input — Ctrl-C should signal quit
        assert!(app.input.is_empty());
        assert!(!app.should_quit);

        // Simulate the Ctrl-C logic from main.rs
        if app.input.is_empty() {
            app.should_quit = true;
        } else {
            app.clear_input();
        }

        assert!(app.should_quit);
    }

    #[test]
    fn test_ctrl_c_with_input_clears_not_quits() {
        let mut app = App::new(vec!["/test".to_string()]);

        app.on_key('t');
        app.on_key('e');
        app.on_key('s');
        app.on_key('t');

        // Simulate the Ctrl-C logic from main.rs
        if app.input.is_empty() {
            app.should_quit = true;
        } else {
            app.clear_input();
        }

        assert!(app.input.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn test_on_key_inserts_at_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.on_key('a');
        app.on_key('c');
        // input = "ac", cursor at 2
        app.cursor_pos = 1; // move cursor between 'a' and 'c'
        app.on_key('b');
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor_pos, 2);
    }

    #[test]
    fn test_on_backspace_at_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "abc".to_string();
        app.cursor_pos = 2; // cursor after 'b'
        app.on_backspace();
        assert_eq!(app.input, "ac");
        assert_eq!(app.cursor_pos, 1);
    }

    #[test]
    fn test_on_backspace_at_start_does_nothing() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "abc".to_string();
        app.cursor_pos = 0;
        app.on_backspace();
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor_pos, 0);
    }

    #[test]
    fn test_on_delete_at_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "abc".to_string();
        app.cursor_pos = 1; // cursor after 'a'
        app.on_delete();
        assert_eq!(app.input, "ac");
        assert_eq!(app.cursor_pos, 1);
    }

    #[test]
    fn test_move_cursor_word_left() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello world foo".to_string();
        app.cursor_pos = app.input.len(); // at end

        app.move_cursor_word_left();
        assert_eq!(app.cursor_pos, 12); // before "foo"

        app.move_cursor_word_left();
        assert_eq!(app.cursor_pos, 6); // before "world"

        app.move_cursor_word_left();
        assert_eq!(app.cursor_pos, 0); // before "hello"

        // At start, stays at 0
        app.move_cursor_word_left();
        assert_eq!(app.cursor_pos, 0);
    }

    #[test]
    fn test_move_cursor_word_right() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello world foo".to_string();
        app.cursor_pos = 0;

        app.move_cursor_word_right();
        assert_eq!(app.cursor_pos, 6); // after "hello "

        app.move_cursor_word_right();
        assert_eq!(app.cursor_pos, 12); // after "world "

        app.move_cursor_word_right();
        assert_eq!(app.cursor_pos, 15); // end

        // At end, stays
        app.move_cursor_word_right();
        assert_eq!(app.cursor_pos, 15);
    }

    #[test]
    fn test_delete_word_left() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello world".to_string();
        app.cursor_pos = app.input.len();

        app.delete_word_left();
        assert_eq!(app.input, "hello ");
        assert_eq!(app.cursor_pos, 6);

        app.delete_word_left();
        assert_eq!(app.input, "");
        assert_eq!(app.cursor_pos, 0);
    }

    #[test]
    fn test_move_cursor_home_end() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello".to_string();
        app.cursor_pos = 3;

        app.move_cursor_home();
        assert_eq!(app.cursor_pos, 0);

        app.move_cursor_end();
        assert_eq!(app.cursor_pos, 5);
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

        assert_eq!(app.cursor_pos, 0);
        assert!(app.input.is_empty());
    }

    #[test]
    fn test_move_cursor_left_right() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "abc".to_string();
        app.cursor_pos = 3;

        app.move_cursor_left();
        assert_eq!(app.cursor_pos, 2);

        app.move_cursor_left();
        assert_eq!(app.cursor_pos, 1);

        app.move_cursor_right();
        assert_eq!(app.cursor_pos, 2);
    }

    #[test]
    fn test_clear_input_resets_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello".to_string();
        app.cursor_pos = 3;

        app.clear_input();

        assert_eq!(app.cursor_pos, 0);
        assert!(app.input.is_empty());
    }

    #[test]
    fn test_delete_word_right() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello world foo".to_string();
        app.cursor_pos = 0;

        app.delete_word_right();
        assert_eq!(app.input, "world foo");
        assert_eq!(app.cursor_pos, 0);

        app.delete_word_right();
        assert_eq!(app.input, "foo");
        assert_eq!(app.cursor_pos, 0);

        app.delete_word_right();
        assert_eq!(app.input, "");
        assert_eq!(app.cursor_pos, 0);
    }

    #[test]
    fn test_delete_word_right_at_end_does_nothing() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello".to_string();
        app.cursor_pos = 5;

        app.delete_word_right();
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor_pos, 5);
    }

    #[test]
    fn test_tick_clears_state_when_query_becomes_empty() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Simulate: user had typed "hello", search completed, then backspaced to empty
        app.input = String::new(); // empty — user backspaced everything
        app.last_query = "hello".to_string(); // previous query that produced results
        app.results_query = "hello".to_string();
        app.results = vec![RipgrepMatch {
            file_path: "/test/file.jsonl".to_string(),
            message: None,
            source: SessionSource::ClaudeCodeCLI,
        }];
        app.groups = vec![SessionGroup {
            session_id: "abc123".to_string(),
            file_path: "/test/file.jsonl".to_string(),
            matches: vec![],
        }];
        app.group_cursor = 1;
        app.sub_cursor = 2;
        app.expanded = true;
        app.searching = true;
        app.latest_chains.insert("file".to_string(), HashSet::new());
        app.error = Some("stale error".to_string());
        app.preview_mode = true;

        // Set debounce to fire: last keystroke was > DEBOUNCE_MS ago
        app.last_keystroke = Some(Instant::now() - Duration::from_millis(DEBOUNCE_MS + 50));
        app.typing = true;

        app.tick();

        assert!(
            app.results.is_empty(),
            "results should be cleared after tick with empty query"
        );
        assert!(
            app.groups.is_empty(),
            "groups should be cleared after tick with empty query"
        );
        assert!(
            app.results_query.is_empty(),
            "results_query should be cleared after tick with empty query"
        );
        assert!(
            app.last_query.is_empty(),
            "last_query should be updated to empty"
        );
        assert_eq!(app.group_cursor, 0, "group_cursor should be reset");
        assert_eq!(app.sub_cursor, 0, "sub_cursor should be reset");
        assert!(!app.expanded, "expanded should be reset");
        assert!(!app.typing, "typing should be false after debounce");
        assert!(!app.searching, "searching should be false");
        assert!(
            app.latest_chains.is_empty(),
            "latest_chains should be cleared"
        );
        assert!(app.error.is_none(), "error should be cleared");
        assert!(!app.preview_mode, "preview_mode should be reset");
    }

    #[test]
    fn test_delete_word_right_from_middle() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.input = "hello world".to_string();
        app.cursor_pos = 5; // after "hello", on the space

        // First delete removes " " (skip non-alnum to next word boundary)
        app.delete_word_right();
        assert_eq!(app.input, "helloworld");
        assert_eq!(app.cursor_pos, 5);

        // Second delete removes "world"
        app.delete_word_right();
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor_pos, 5);
    }
}
