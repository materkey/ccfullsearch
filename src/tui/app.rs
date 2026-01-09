use crate::search::{group_by_session, search, RipgrepMatch, SessionGroup};
use ratatui::widgets::ListState;
use std::time::{Duration, Instant};

const DEBOUNCE_MS: u64 = 300;

pub struct App {
    pub input: String,
    pub results: Vec<RipgrepMatch>,
    pub groups: Vec<SessionGroup>,
    pub list_state: ListState,
    pub group_cursor: usize,
    pub sub_cursor: usize,
    pub expanded: bool,
    pub searching: bool,
    pub typing: bool,
    pub error: Option<String>,
    pub search_path: String,
    pub last_query: String,
    pub results_query: String,
    pub last_keystroke: Option<Instant>,
    pub preview_mode: bool,
    pub should_quit: bool,
    pub resume_id: Option<String>,
    pub resume_file_path: Option<String>,
}

impl App {
    pub fn new(search_path: String) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            input: String::new(),
            results: vec![],
            groups: vec![],
            list_state,
            group_cursor: 0,
            sub_cursor: 0,
            expanded: false,
            searching: false,
            typing: false,
            error: None,
            search_path,
            last_query: String::new(),
            results_query: String::new(),
            last_keystroke: None,
            preview_mode: false,
            should_quit: false,
            resume_id: None,
            resume_file_path: None,
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

        if self.expanded && self.sub_cursor > 0 {
            self.sub_cursor -= 1;
        } else if self.group_cursor > 0 {
            self.group_cursor -= 1;
            self.sub_cursor = 0;
            self.expanded = false;
        }
    }

    pub fn on_down(&mut self) {
        if self.groups.is_empty() {
            return;
        }

        if self.expanded {
            if let Some(group) = self.selected_group() {
                if self.sub_cursor < group.matches.len().saturating_sub(1) {
                    self.sub_cursor += 1;
                    return;
                }
            }
        }

        if self.group_cursor < self.groups.len().saturating_sub(1) {
            self.group_cursor += 1;
            self.sub_cursor = 0;
            self.expanded = false;
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
                (msg.session_id.clone(), m.file_path.clone())
            })
        });

        if let Some((session_id, file_path)) = resume_info {
            self.resume_id = Some(session_id);
            self.resume_file_path = Some(file_path);
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
        // Check if debounce period passed
        if let Some(last) = self.last_keystroke {
            if last.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                self.last_keystroke = None;
                self.typing = false;

                if !self.input.is_empty() && self.input != self.last_query {
                    self.do_search();
                }
            }
        }
    }

    pub fn do_search(&mut self) {
        self.last_query = self.input.clone();
        self.searching = true;

        match search(&self.input, &self.search_path) {
            Ok(results) => {
                self.results_query = self.input.clone();
                self.groups = group_by_session(results.clone());
                self.results = results;
                self.group_cursor = 0;
                self.sub_cursor = 0;
                self.expanded = false;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
            }
        }

        self.searching = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_new() {
        let app = App::new("/test/path".to_string());

        assert_eq!(app.search_path, "/test/path");
        assert!(app.input.is_empty());
        assert!(app.groups.is_empty());
        assert!(!app.should_quit);
    }

    #[test]
    fn test_on_key() {
        let mut app = App::new("/test".to_string());

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
        let mut app = App::new("/test".to_string());
        app.input = "hello".to_string();

        app.on_backspace();

        assert_eq!(app.input, "hell");
    }

    #[test]
    fn test_navigation_empty_groups() {
        let mut app = App::new("/test".to_string());

        // Should not panic with empty groups
        app.on_up();
        app.on_down();
        app.on_left();
        app.on_right();

        assert_eq!(app.group_cursor, 0);
    }

    #[test]
    fn test_expand_collapse() {
        let mut app = App::new("/test".to_string());

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
        let mut app = App::new("/test".to_string());

        // Without groups, preview should not toggle
        app.on_tab();
        assert!(!app.preview_mode);
    }
}
