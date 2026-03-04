use crate::search::{group_by_session, search_multiple_paths, RipgrepMatch, SessionGroup, SessionSource};
use crate::tree::SessionTree;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

const DEBOUNCE_MS: u64 = 300;

/// Result from background search thread
type SearchResult = Result<(String, bool, Vec<RipgrepMatch>), String>;

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
    last_regex_mode: bool,
    /// Channel to receive search results from background thread
    search_rx: Receiver<SearchResult>,
    /// Channel to send search requests to background thread (query, paths, regex_mode)
    search_tx: Sender<(String, Vec<String>, bool)>,
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
    tree_load_rx: Option<Receiver<Result<SessionTree, String>>>,
    /// Whether tree mode was the initial mode (launched with --tree)
    pub tree_mode_standalone: bool,
}

impl App {
    pub fn new(search_paths: Vec<String>) -> Self {
        // Create channels for async search
        let (result_tx, result_rx) = mpsc::channel::<SearchResult>();
        let (query_tx, query_rx) = mpsc::channel::<(String, Vec<String>, bool)>();

        // Spawn background search thread
        thread::spawn(move || {
            while let Ok((query, paths, use_regex)) = query_rx.recv() {
                let result = search_multiple_paths(&query, &paths, use_regex)
                    .map(|r| (query, use_regex, r));
                let _ = result_tx.send(result);
            }
        });

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
            search_rx: result_rx,
            search_tx: query_tx,
            latest_chains: HashMap::new(),
            tree_mode: false,
            session_tree: None,
            tree_cursor: 0,
            tree_scroll_offset: 0,
            tree_loading: false,
            tree_load_rx: None,
            tree_mode_standalone: false,
        }
    }

    pub fn on_key(&mut self, c: char) {
        self.input.push(c);
        self.typing = true;
        self.last_keystroke = Some(Instant::now());
    }

    pub fn on_backspace(&mut self) {
        self.input.pop();
        self.typing = true;
        self.last_keystroke = Some(Instant::now());
    }

    pub fn on_up(&mut self) {
        if self.groups.is_empty() {
            return;
        }

        let old_cursor = (self.group_cursor, self.sub_cursor);

        if self.expanded && self.sub_cursor > 0 {
            self.sub_cursor -= 1;
        } else if self.group_cursor > 0 {
            self.group_cursor -= 1;
            self.sub_cursor = 0;
            self.expanded = false;
        }

        // Force full redraw in preview mode when selection changed
        if self.preview_mode && (self.group_cursor, self.sub_cursor) != old_cursor {
            self.needs_full_redraw = true;
        }
    }

    pub fn on_down(&mut self) {
        if self.groups.is_empty() {
            return;
        }

        let old_cursor = (self.group_cursor, self.sub_cursor);

        if self.expanded {
            if let Some(group) = self.selected_group() {
                if self.sub_cursor < group.matches.len().saturating_sub(1) {
                    self.sub_cursor += 1;
                    // Force full redraw in preview mode
                    if self.preview_mode {
                        self.needs_full_redraw = true;
                    }
                    return;
                }
            }
        }

        if self.group_cursor < self.groups.len().saturating_sub(1) {
            self.group_cursor += 1;
            self.sub_cursor = 0;
            self.expanded = false;
        }

        // Force full redraw in preview mode when selection changed
        if self.preview_mode && (self.group_cursor, self.sub_cursor) != old_cursor {
            self.needs_full_redraw = true;
        }
    }

    pub fn on_right(&mut self) {
        if !self.groups.is_empty() && self.group_cursor < self.groups.len() {
            self.expanded = true;
            // Precompute latest chain for the expanded group (for fork indicator)
            if let Some(group) = self.groups.get(self.group_cursor) {
                let fp = group.file_path.clone();
                if !self.latest_chains.contains_key(&fp) {
                    if let Some(chain) = crate::resume::build_chain_from_tip(&fp) {
                        self.latest_chains.insert(fp, chain);
                    }
                }
            }
        }
    }

    pub fn on_left(&mut self) {
        self.expanded = false;
        self.sub_cursor = 0;
    }

    pub fn on_tab(&mut self) {
        if !self.groups.is_empty() && self.selected_match().is_some() {
            self.preview_mode = !self.preview_mode;
            // Force full redraw when toggling preview mode
            self.needs_full_redraw = true;
        }
    }

    /// Clear input and reset search state (Ctrl-C behavior)
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.typing = false;
        self.last_keystroke = None;
        self.searching = false;
        self.last_query.clear();
    }

    pub fn on_toggle_regex(&mut self) {
        self.regex_mode = !self.regex_mode;
        // Trigger re-search if we have a query
        if !self.input.is_empty() {
            self.last_keystroke = Some(Instant::now());
            self.typing = true;
        }
    }

    pub fn on_enter(&mut self) {
        if self.preview_mode {
            self.preview_mode = false;
            return;
        }

        // Extract values first to avoid borrow issues
        let resume_info = self.selected_match().and_then(|m| {
            m.message.as_ref().map(|msg| {
                (msg.session_id.clone(), m.file_path.clone(), m.source, msg.uuid.clone())
            })
        });

        if let Some((session_id, file_path, source, uuid)) = resume_info {
            self.resume_id = Some(session_id);
            self.resume_file_path = Some(file_path);
            self.resume_source = Some(source);
            self.resume_uuid = uuid;
            self.should_quit = true;
        }
    }

    // --- Tree mode methods ---

    /// Enter tree mode from search results (press 'b' on a session group)
    pub fn enter_tree_mode(&mut self) {
        let file_path = match self.selected_group() {
            Some(group) => group.file_path.clone(),
            None => return,
        };
        self.enter_tree_mode_for_file(&file_path);
    }

    /// Enter tree mode directly for a file path or session ID
    pub fn enter_tree_mode_direct(&mut self, target: &str) {
        // If target looks like a file path, use directly
        let file_path = if target.contains('/') || target.ends_with(".jsonl") {
            target.to_string()
        } else {
            // Search for session ID in known paths
            match self.find_session_file(target) {
                Some(path) => path,
                None => {
                    self.error = Some(format!("Session not found: {}", target));
                    return;
                }
            }
        };
        self.tree_mode_standalone = true;
        self.enter_tree_mode_for_file(&file_path);
    }

    fn enter_tree_mode_for_file(&mut self, file_path: &str) {
        self.tree_mode = true;
        self.tree_loading = true;
        self.tree_cursor = 0;
        self.tree_scroll_offset = 0;
        self.session_tree = None;
        self.preview_mode = false;
        self.needs_full_redraw = true;

        let fp = file_path.to_string();
        let (tx, rx) = mpsc::channel();
        self.tree_load_rx = Some(rx);

        thread::spawn(move || {
            let result = SessionTree::from_file(&fp);
            let _ = tx.send(result);
        });
    }

    /// Search for a JSONL file by session ID across search paths.
    /// Checks both CLI format (projects/<encoded>/<id>.jsonl) and
    /// Desktop format (deep hierarchy with audit.jsonl containing session_id).
    fn find_session_file(&self, session_id: &str) -> Option<String> {
        use std::fs;
        let target_filename = format!("{}.jsonl", session_id);

        for search_path in &self.search_paths {
            if let Ok(entries) = fs::read_dir(search_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        // CLI: ~/.claude/projects/<encoded-path>/<session-id>.jsonl
                        let candidate = path.join(&target_filename);
                        if candidate.exists() {
                            return Some(candidate.to_string_lossy().to_string());
                        }
                        // Desktop: deeper hierarchy, recurse one more level
                        if let Ok(subentries) = fs::read_dir(&path) {
                            for subentry in subentries.flatten() {
                                let subpath = subentry.path();
                                if subpath.is_dir() {
                                    let candidate = subpath.join(&target_filename);
                                    if candidate.exists() {
                                        return Some(candidate.to_string_lossy().to_string());
                                    }
                                    // Desktop local_<id>/audit.jsonl — check by reading first line
                                    let audit = subpath.join("audit.jsonl");
                                    if audit.exists() {
                                        if Self::file_contains_session_id(&audit, session_id) {
                                            return Some(audit.to_string_lossy().to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Quick check if a JSONL file contains the given session ID (reads first 5 lines).
    fn file_contains_session_id(path: &std::path::Path, session_id: &str) -> bool {
        use std::io::{BufRead, BufReader};
        let Ok(file) = std::fs::File::open(path) else { return false };
        let reader = BufReader::new(file);
        for line in reader.lines().take(5) {
            if let Ok(line) = line {
                if line.contains(session_id) {
                    return true;
                }
            }
        }
        false
    }

    pub fn exit_tree_mode(&mut self) {
        if self.tree_mode_standalone {
            self.should_quit = true;
            return;
        }
        self.tree_mode = false;
        self.session_tree = None;
        self.tree_loading = false;
        self.tree_load_rx = None;
        self.preview_mode = false;
        self.needs_full_redraw = true;
    }

    pub fn on_up_tree(&mut self) {
        if self.tree_cursor > 0 {
            self.tree_cursor -= 1;
            self.adjust_tree_scroll();
            if self.preview_mode {
                self.needs_full_redraw = true;
            }
        }
    }

    pub fn on_down_tree(&mut self) {
        if let Some(ref tree) = self.session_tree {
            if self.tree_cursor < tree.rows.len().saturating_sub(1) {
                self.tree_cursor += 1;
                self.adjust_tree_scroll();
                if self.preview_mode {
                    self.needs_full_redraw = true;
                }
            }
        }
    }

    pub fn on_left_tree(&mut self) {
        // Jump to previous branch point
        if let Some(ref tree) = self.session_tree {
            for i in (0..self.tree_cursor).rev() {
                if tree.rows[i].is_branch_point {
                    self.tree_cursor = i;
                    self.adjust_tree_scroll();
                    if self.preview_mode {
                        self.needs_full_redraw = true;
                    }
                    return;
                }
            }
        }
    }

    pub fn on_right_tree(&mut self) {
        // Jump to next branch point
        if let Some(ref tree) = self.session_tree {
            for i in (self.tree_cursor + 1)..tree.rows.len() {
                if tree.rows[i].is_branch_point {
                    self.tree_cursor = i;
                    self.adjust_tree_scroll();
                    if self.preview_mode {
                        self.needs_full_redraw = true;
                    }
                    return;
                }
            }
        }
    }

    pub fn on_enter_tree(&mut self) {
        if self.preview_mode {
            self.preview_mode = false;
            self.needs_full_redraw = true;
            return;
        }

        if let Some(ref tree) = self.session_tree {
            if let Some(row) = tree.rows.get(self.tree_cursor) {
                self.resume_uuid = Some(row.uuid.clone());
                self.resume_id = Some(tree.session_id.clone());
                self.resume_file_path = Some(tree.file_path.clone());
                self.resume_source = Some(tree.source);
                self.should_quit = true;
            }
        }
    }

    pub fn on_tab_tree(&mut self) {
        if let Some(ref tree) = self.session_tree {
            if !tree.rows.is_empty() {
                self.preview_mode = !self.preview_mode;
                self.needs_full_redraw = true;
            }
        }
    }

    fn adjust_tree_scroll(&mut self) {
        let visible = 20; // approximate visible height
        if self.tree_cursor < self.tree_scroll_offset {
            self.tree_scroll_offset = self.tree_cursor;
        } else if self.tree_cursor >= self.tree_scroll_offset + visible {
            self.tree_scroll_offset = self.tree_cursor.saturating_sub(visible) + 1;
        }
    }

    pub fn selected_group(&self) -> Option<&SessionGroup> {
        self.groups.get(self.group_cursor)
    }

    pub fn selected_match(&self) -> Option<&RipgrepMatch> {
        self.selected_group()
            .and_then(|g| g.matches.get(self.sub_cursor))
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
            match result {
                Ok((query, use_regex, results)) => {
                    // Only apply results if query and regex mode match current state
                    // (ignore stale results from old queries or mode changes)
                    if query == self.input && use_regex == self.regex_mode {
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
                }
                Err(e) => {
                    self.error = Some(e);
                    self.searching = false;
                }
            }
        }

        // Check if debounce period passed
        if let Some(last) = self.last_keystroke {
            if last.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                self.last_keystroke = None;
                self.typing = false;

                // Re-search if query changed or regex mode changed
                let query_changed = self.input != self.last_query;
                let mode_changed = self.regex_mode != self.last_regex_mode;
                if !self.input.is_empty() && (query_changed || mode_changed) {
                    self.start_search();
                }
            }
        }
    }

    /// Start an async search in the background thread
    fn start_search(&mut self) {
        self.last_query = self.input.clone();
        self.last_regex_mode = self.regex_mode;
        self.searching = true;
        let _ = self.search_tx.send((self.input.clone(), self.search_paths.clone(), self.regex_mode));
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

        app.on_backspace();

        assert_eq!(app.input, "hell");
    }

    #[test]
    fn test_navigation_empty_groups() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Should not panic with empty groups
        app.on_up();
        app.on_down();
        app.on_left();
        app.on_right();

        assert_eq!(app.group_cursor, 0);
    }

    #[test]
    fn test_expand_collapse() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Setup some groups
        app.groups = vec![SessionGroup {
            session_id: "test".to_string(),
            file_path: "/test.jsonl".to_string(),
            matches: vec![],
        }];

        app.on_right();
        assert!(app.expanded);

        app.on_left();
        assert!(!app.expanded);
    }

    #[test]
    fn test_preview_toggle() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Without groups, preview should not toggle
        app.on_tab();
        assert!(!app.preview_mode);
    }

    #[test]
    fn test_clear_input_resets_state() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Simulate typing a query
        app.on_key('h');
        app.on_key('i');
        app.last_query = "hi".to_string();
        app.searching = true;

        app.clear_input();

        assert!(app.input.is_empty());
        assert!(!app.typing);
        assert!(app.last_keystroke.is_none());
        assert!(!app.searching);
        assert!(app.last_query.is_empty());
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
    fn test_exit_tree_mode_returns_to_search() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.tree_mode = true;
        app.tree_mode_standalone = false;

        app.exit_tree_mode();

        assert!(!app.tree_mode);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_exit_tree_mode_standalone_quits() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.tree_mode = true;
        app.tree_mode_standalone = true;

        app.exit_tree_mode();

        assert!(app.should_quit);
    }
}
