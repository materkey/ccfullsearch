use crate::search::{group_by_session, search_multiple_paths, RipgrepMatch, SessionGroup, SessionSource};
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
            needs_full_redraw: false,
            regex_mode: false,
            last_regex_mode: false,
            search_rx: result_rx,
            search_tx: query_tx,
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
                (msg.session_id.clone(), m.file_path.clone(), m.source)
            })
        });

        if let Some((session_id, file_path, source)) = resume_info {
            self.resume_id = Some(session_id);
            self.resume_file_path = Some(file_path);
            self.resume_source = Some(source);
            self.should_quit = true;
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
}
