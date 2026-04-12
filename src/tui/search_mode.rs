use crate::search::{extract_project_from_path, RipgrepMatch, SessionGroup};
use crate::tui::state::{AppOutcome, PickedSession, ResumeTarget};
use crate::tui::App;
use std::time::Instant;

impl App {
    /// Whether the app is currently showing the recent sessions list
    /// (input is empty, no search results, not loading search).
    pub fn in_recent_sessions_mode(&self) -> bool {
        self.input.is_empty() && self.search.groups.is_empty()
    }

    pub fn on_up(&mut self) {
        // Recent sessions navigation
        if self.in_recent_sessions_mode() {
            if !self.recent.filtered.is_empty() && self.recent.cursor > 0 {
                self.recent.cursor -= 1;
                self.recent.adjust_scroll(self.last_tree_visible_height);
            }
            return;
        }

        if self.search.groups.is_empty() {
            return;
        }

        let old_cursor = (self.search.group_cursor, self.search.sub_cursor);

        if self.search.expanded && self.search.sub_cursor > 0 {
            self.search.sub_cursor -= 1;
        } else if self.search.group_cursor > 0 {
            self.search.group_cursor -= 1;
            self.search.sub_cursor = 0;
            self.search.expanded = false;
        }

        // Force full redraw in preview mode when selection changed
        if self.preview_mode && (self.search.group_cursor, self.search.sub_cursor) != old_cursor {
            self.needs_full_redraw = true;
        }
    }

    pub fn on_down(&mut self) {
        // Recent sessions navigation
        if self.in_recent_sessions_mode() {
            if !self.recent.filtered.is_empty()
                && self.recent.cursor < self.recent.filtered.len().saturating_sub(1)
            {
                self.recent.cursor += 1;
                self.recent.adjust_scroll(self.last_tree_visible_height);
            }
            return;
        }

        if self.search.groups.is_empty() {
            return;
        }

        let old_cursor = (self.search.group_cursor, self.search.sub_cursor);

        if self.search.expanded {
            if let Some(group) = self.selected_group() {
                if self.search.sub_cursor < group.matches.len().saturating_sub(1) {
                    self.search.sub_cursor += 1;
                    // Force full redraw in preview mode
                    if self.preview_mode {
                        self.needs_full_redraw = true;
                    }
                    return;
                }
            }
        }

        if self.search.group_cursor < self.search.groups.len().saturating_sub(1) {
            self.search.group_cursor += 1;
            self.search.sub_cursor = 0;
            self.search.expanded = false;
        }

        // Force full redraw in preview mode when selection changed
        if self.preview_mode && (self.search.group_cursor, self.search.sub_cursor) != old_cursor {
            self.needs_full_redraw = true;
        }
    }

    pub fn on_right(&mut self) {
        if self.input.cursor_pos() < self.input.len() {
            self.move_cursor_right();
        } else if !self.search.groups.is_empty()
            && self.search.group_cursor < self.search.groups.len()
        {
            self.search.expanded = true;
            // Precompute latest chain for the expanded group (for fork indicator)
            if let Some(group) = self.search.groups.get(self.search.group_cursor) {
                let fp = group.file_path.clone();
                if let std::collections::hash_map::Entry::Vacant(e) =
                    self.search.latest_chains.entry(fp.clone())
                {
                    if let Some(chain) = crate::resume::build_chain_from_tip(&fp) {
                        e.insert(chain);
                    }
                }
            }
        }
    }

    pub fn on_left(&mut self) {
        if self.search.expanded {
            self.search.expanded = false;
            self.search.sub_cursor = 0;
        } else if self.input.cursor_pos() > 0 {
            self.move_cursor_left();
        } else {
            self.search.expanded = false;
            self.search.sub_cursor = 0;
        }
    }

    pub fn on_tab(&mut self) {
        if !self.search.groups.is_empty() && self.selected_match().is_some() {
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

    pub fn toggle_automation_filter(&mut self) {
        use crate::tui::state::AutomationFilter;
        self.automation_filter = match self.automation_filter {
            AutomationFilter::All => AutomationFilter::Manual,
            AutomationFilter::Manual => AutomationFilter::Auto,
            AutomationFilter::Auto => AutomationFilter::All,
        };
        self.apply_recent_sessions_filter();
        self.apply_groups_filter();
        self.recent.cursor = 0;
        self.recent.scroll_offset = 0;
        self.search.group_cursor = 0;
        self.search.sub_cursor = 0;
        self.search.expanded = false;
    }

    pub fn toggle_project_filter(&mut self) {
        if self.current_project_paths.is_empty() {
            return;
        }
        self.project_filter = !self.project_filter;
        self.search_paths = if self.project_filter {
            self.current_project_paths.clone()
        } else {
            self.all_search_paths.clone()
        };
        if self.project_filter {
            self.recent
                .start_project_load(self.current_project_paths.clone());
        }
        self.apply_recent_sessions_filter();
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

        // Recent sessions: resume/pick selected session
        if self.in_recent_sessions_mode() {
            if let Some(session) = self.recent.filtered.get(self.recent.cursor) {
                if self.picker_mode {
                    self.outcome = Some(AppOutcome::Pick(PickedSession {
                        session_id: session.session_id.clone(),
                        file_path: session.file_path.clone(),
                        source: session.source,
                        project: session.project.clone(),
                        message_uuid: None,
                    }));
                } else {
                    self.outcome = Some(AppOutcome::Resume(ResumeTarget {
                        session_id: session.session_id.clone(),
                        file_path: session.file_path.clone(),
                        source: session.source,
                        uuid: None,
                    }));
                }
                self.should_quit = true;
            }
            return;
        }

        // Extract values first to avoid borrow issues
        let resume_info = self.selected_match().and_then(|m| {
            m.message.as_ref().map(|msg| {
                (
                    msg.session_id.clone(),
                    m.file_path.clone(),
                    m.source,
                    msg.uuid.clone(),
                )
            })
        });

        if let Some((session_id, file_path, source, uuid)) = resume_info {
            if self.picker_mode {
                let project = extract_project_from_path(&file_path);
                self.outcome = Some(AppOutcome::Pick(PickedSession {
                    session_id,
                    file_path,
                    source,
                    project,
                    message_uuid: uuid,
                }));
            } else {
                // Don't pass message uuid from search results — it triggers
                // fork logic when the message is not on the latest chain.
                // UUID-based resume is only meaningful from tree view where
                // the user explicitly selects a specific branch point.
                self.outcome = Some(AppOutcome::Resume(ResumeTarget {
                    session_id,
                    file_path,
                    source,
                    uuid: None,
                }));
            }
            self.should_quit = true;
        }
    }

    /// Enter tree mode for the currently selected recent session.
    pub fn enter_tree_mode_recent(&mut self) {
        if let Some(session) = self.recent.filtered.get(self.recent.cursor) {
            let file_path = session.file_path.clone();
            self.enter_tree_mode_for_file(&file_path);
        }
    }

    pub fn selected_group(&self) -> Option<&SessionGroup> {
        self.search.groups.get(self.search.group_cursor)
    }

    pub fn selected_match(&self) -> Option<&RipgrepMatch> {
        self.selected_group()
            .and_then(|g| g.matches.get(self.search.sub_cursor))
    }
}

#[cfg(test)]
mod tests {
    use crate::recent::RecentSession;
    use crate::search::{RipgrepMatch, SessionGroup};
    use crate::session::SessionSource;
    use crate::tui::state::{AppOutcome, DEBOUNCE_MS};
    use crate::tui::App;
    use chrono::Utc;
    use std::time::{Duration, Instant};

    fn make_recent_session(id: &str, project: &str, summary: &str) -> RecentSession {
        RecentSession {
            session_id: id.to_string(),
            file_path: format!("/tmp/{}.jsonl", id),
            project: project.to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc::now(),
            summary: summary.to_string(),
            automation: None,
        }
    }

    #[test]
    fn test_navigation_empty_groups() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Should not panic with empty groups
        app.on_up();
        app.on_down();
        app.on_left();
        app.on_right();

        assert_eq!(app.search.group_cursor, 0);
    }

    #[test]
    fn test_expand_collapse() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Setup some groups
        app.search.groups = vec![SessionGroup {
            session_id: "test".to_string(),
            file_path: "/test.jsonl".to_string(),
            matches: vec![],
            automation: None,
        }];

        app.on_right();
        assert!(app.search.expanded);

        app.on_left();
        assert!(!app.search.expanded);
    }

    #[test]
    fn test_left_collapses_expanded_group_even_with_input_cursor() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.groups = vec![SessionGroup {
            session_id: "test".to_string(),
            file_path: "/test.jsonl".to_string(),
            matches: vec![],
            automation: None,
        }];
        app.input.set_text("query"); // cursor at end (5)
        app.search.expanded = true;
        app.search.sub_cursor = 1;

        app.on_left();

        assert!(!app.search.expanded);
        assert_eq!(app.search.sub_cursor, 0);
        assert_eq!(app.input.cursor_pos(), 5);
    }

    #[test]
    fn test_preview_toggle() {
        let mut app = App::new(vec!["/test".to_string()]);

        // Without groups, preview should not toggle
        app.on_tab();
        assert!(!app.preview_mode);
    }

    #[test]
    fn test_toggle_project_filter_no_current_project() {
        let mut app = App::new(vec!["/test".to_string()]);
        assert!(!app.project_filter);
        app.toggle_project_filter();
        assert!(!app.project_filter); // unchanged — no current project detected
    }

    #[test]
    fn test_toggle_project_filter_switches_paths() {
        let mut app = App::new(vec!["/all".to_string()]);
        app.current_project_paths = vec!["/all/-Users-test-project".to_string()];

        assert!(!app.project_filter);
        assert_eq!(app.search_paths, vec!["/all".to_string()]);

        app.toggle_project_filter();
        assert!(app.project_filter);
        assert_eq!(
            app.search_paths,
            vec!["/all/-Users-test-project".to_string()]
        );

        app.toggle_project_filter();
        assert!(!app.project_filter);
        assert_eq!(app.search_paths, vec!["/all".to_string()]);
    }

    #[test]
    fn test_toggle_project_filter_triggers_research() {
        let mut app = App::new(vec!["/all".to_string()]);
        app.current_project_paths = vec!["/all/-Users-test".to_string()];
        app.input.set_text_and_cursor("query", 5);
        app.last_query = "query".to_string();

        app.toggle_project_filter();
        app.last_keystroke = Some(Instant::now() - Duration::from_millis(DEBOUNCE_MS + 1));
        app.tick();

        assert!(app.search.searching);
        assert_eq!(app.search.search_seq, 1);
        assert_eq!(app.last_search_paths, vec!["/all/-Users-test".to_string()]);
    }

    #[test]
    fn test_toggle_project_filter_no_research_empty_query() {
        let mut app = App::new(vec!["/all".to_string()]);
        app.current_project_paths = vec!["/all/-Users-test".to_string()];

        app.toggle_project_filter();

        assert!(app.project_filter);
        assert!(!app.typing);
    }

    #[test]
    fn test_stale_search_result_ignored_when_scope_changes() {
        let mut app = App::new(vec!["/all".to_string()]);
        app.input.set_text("query");
        app.search_paths = vec!["/project".to_string()];
        app.search.search_seq = 1;
        app.search.searching = true;

        let stale_result = crate::tui::state::BackgroundSearchResult {
            seq: 1,
            query: "query".to_string(),
            paths: vec!["/all".to_string()],
            use_regex: false,
            result: Ok(crate::search::SearchResult {
                matches: vec![RipgrepMatch {
                    file_path: "/all/session.jsonl".to_string(),
                    message: None,
                    source: SessionSource::ClaudeCodeCLI,
                }],
                truncated: false,
            }),
        };

        app.handle_search_result(stale_result);

        assert_eq!(app.search.results_count, 0);
        assert!(app.search.groups.is_empty());
        assert!(app.search.searching);
    }

    // =========================================================================
    // Recent sessions navigation tests
    // =========================================================================

    #[test]
    fn test_in_recent_sessions_mode() {
        let mut app = App::new(vec!["/test".to_string()]);
        // Empty input, empty groups → recent sessions mode
        assert!(app.in_recent_sessions_mode());

        // Typing something exits recent sessions mode
        app.on_key('h');
        assert!(!app.in_recent_sessions_mode());

        // Clearing input returns to recent sessions mode
        app.clear_input();
        assert!(app.in_recent_sessions_mode());
    }

    #[test]
    fn test_in_recent_sessions_mode_false_when_groups_present() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.groups = vec![SessionGroup {
            session_id: "test".to_string(),
            file_path: "/test.jsonl".to_string(),
            matches: vec![],
            automation: None,
        }];
        // Even with empty input, if groups exist we're in search results mode
        assert!(!app.in_recent_sessions_mode());
    }

    #[test]
    fn test_recent_sessions_up_down_navigation() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.filtered = vec![
            make_recent_session("s1", "proj-a", "first message"),
            make_recent_session("s2", "proj-b", "second message"),
            make_recent_session("s3", "proj-c", "third message"),
        ];

        assert_eq!(app.recent.cursor, 0);

        app.on_down();
        assert_eq!(app.recent.cursor, 1);

        app.on_down();
        assert_eq!(app.recent.cursor, 2);

        // At bottom, should not go further
        app.on_down();
        assert_eq!(app.recent.cursor, 2);

        app.on_up();
        assert_eq!(app.recent.cursor, 1);

        app.on_up();
        assert_eq!(app.recent.cursor, 0);

        // At top, should not go further
        app.on_up();
        assert_eq!(app.recent.cursor, 0);
    }

    #[test]
    fn test_recent_sessions_navigation_empty_list() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.filtered = vec![];

        // Should not panic or change cursor
        app.on_up();
        assert_eq!(app.recent.cursor, 0);
        app.on_down();
        assert_eq!(app.recent.cursor, 0);
    }

    #[test]
    fn test_recent_sessions_navigation_while_loading() {
        let mut app = App::new(vec!["/test".to_string()]);
        // recent_loading is true by default, recent_sessions is empty
        assert!(app.recent.loading);

        // Navigation should not panic
        app.on_up();
        app.on_down();
        assert_eq!(app.recent.cursor, 0);
    }

    #[test]
    fn test_recent_sessions_enter_resumes_session() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.filtered = vec![
            make_recent_session("s1", "proj-a", "first"),
            make_recent_session("s2", "proj-b", "second"),
        ];

        // Navigate to second session and press Enter
        app.on_down();
        app.on_enter();

        assert!(app.should_quit);
        let target = match &app.outcome {
            Some(AppOutcome::Resume(t)) => t,
            other => panic!("Expected Resume, got {:?}", other),
        };
        assert_eq!(target.session_id, "s2");
        assert_eq!(target.file_path, "/tmp/s2.jsonl");
        assert_eq!(target.source, SessionSource::ClaudeCodeCLI);
        assert!(target.uuid.is_none());
    }

    #[test]
    fn test_recent_sessions_enter_on_empty_list_does_nothing() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.filtered = vec![];

        app.on_enter();

        assert!(!app.should_quit);
        assert!(app.outcome.is_none());
    }

    #[test]
    fn test_typing_exits_recent_sessions_mode() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.filtered = vec![make_recent_session("s1", "proj-a", "first")];
        app.recent.cursor = 0;

        // Type a character — should switch to search mode
        app.on_key('h');
        assert!(!app.in_recent_sessions_mode());
        assert_eq!(app.input.text(), "h");
    }

    #[test]
    fn test_recent_cursor_preserved_on_clear_input() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.filtered = vec![
            make_recent_session("s1", "proj-a", "first"),
            make_recent_session("s2", "proj-b", "second"),
        ];

        // Navigate down, type something, then clear
        app.on_down();
        assert_eq!(app.recent.cursor, 1);

        app.on_key('x');
        app.clear_input();

        // Back in recent sessions mode — cursor preserved (not reset by clear_input)
        assert!(app.in_recent_sessions_mode());
        assert_eq!(app.recent.cursor, 1);
    }

    // =========================================================================
    // Picker mode tests
    // =========================================================================

    #[test]
    fn test_on_enter_picker_mode_sets_picked_session() {
        use crate::search::message::Message;
        use chrono::Utc;

        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;

        // Set up a search result with a message
        app.search.groups = vec![SessionGroup {
            session_id: "sess-123".to_string(),
            file_path: "/home/user/.claude/projects/-Users-user-projects-myapp/session.jsonl"
                .to_string(),
            matches: vec![RipgrepMatch {
                file_path: "/home/user/.claude/projects/-Users-user-projects-myapp/session.jsonl"
                    .to_string(),
                message: Some(Message {
                    session_id: "sess-123".to_string(),
                    role: "user".to_string(),
                    content: "hello".to_string(),
                    timestamp: Utc::now(),
                    branch: None,
                    line_number: 1,
                    uuid: Some("uuid-1".to_string()),
                    parent_uuid: None,
                }),
                source: SessionSource::ClaudeCodeCLI,
            }],
            automation: None,
        }];
        app.input.set_text("hello");

        app.on_enter();

        assert!(app.should_quit);
        let picked = match &app.outcome {
            Some(AppOutcome::Pick(p)) => p,
            other => panic!("Expected Pick, got {:?}", other),
        };
        assert_eq!(picked.session_id, "sess-123");
        assert_eq!(picked.source, SessionSource::ClaudeCodeCLI);
        assert_eq!(picked.project, "myapp");
    }

    #[test]
    fn test_on_enter_picker_mode_from_recent_sessions() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;
        app.recent.loading = false;
        app.recent.filtered = vec![
            make_recent_session("s1", "proj-a", "first"),
            make_recent_session("s2", "proj-b", "second"),
        ];

        app.on_down();
        app.on_enter();

        assert!(app.should_quit);
        let picked = match &app.outcome {
            Some(AppOutcome::Pick(p)) => p,
            other => panic!("Expected Pick, got {:?}", other),
        };
        assert_eq!(picked.session_id, "s2");
        assert_eq!(picked.project, "proj-b");
        assert_eq!(picked.file_path, "/tmp/s2.jsonl");
        assert_eq!(picked.source, SessionSource::ClaudeCodeCLI);
    }

    #[test]
    fn test_esc_in_picker_mode_leaves_outcome_none() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;
        app.recent.loading = false;
        app.recent.filtered = vec![make_recent_session("s1", "proj-a", "first")];

        // Esc clears input (if any) or quits — outcome stays None
        app.clear_input();

        assert!(app.outcome.is_none());
    }
}
