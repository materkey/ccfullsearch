use crate::search::{RipgrepMatch, SessionGroup};
use crate::tui::App;
use std::time::Instant;

impl App {
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
        if self.cursor_pos < self.input.len() {
            self.move_cursor_right();
        } else if !self.groups.is_empty() && self.group_cursor < self.groups.len() {
            self.expanded = true;
            // Precompute latest chain for the expanded group (for fork indicator)
            if let Some(group) = self.groups.get(self.group_cursor) {
                let fp = group.file_path.clone();
                if let std::collections::hash_map::Entry::Vacant(e) =
                    self.latest_chains.entry(fp.clone())
                {
                    if let Some(chain) = crate::resume::build_chain_from_tip(&fp) {
                        e.insert(chain);
                    }
                }
            }
        }
    }

    pub fn on_left(&mut self) {
        if self.cursor_pos > 0 {
            self.move_cursor_left();
        } else {
            self.expanded = false;
            self.sub_cursor = 0;
        }
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
                (
                    msg.session_id.clone(),
                    m.file_path.clone(),
                    m.source,
                    msg.uuid.clone(),
                )
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

    pub fn selected_group(&self) -> Option<&SessionGroup> {
        self.groups.get(self.group_cursor)
    }

    pub fn selected_match(&self) -> Option<&RipgrepMatch> {
        self.selected_group()
            .and_then(|g| g.matches.get(self.sub_cursor))
    }
}

#[cfg(test)]
mod tests {
    use crate::search::{RipgrepMatch, SessionGroup, SessionSource};
    use crate::tui::state::DEBOUNCE_MS;
    use crate::tui::App;
    use std::time::{Duration, Instant};

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
        app.input = "query".to_string();
        app.last_query = "query".to_string();
        app.cursor_pos = 5;

        app.toggle_project_filter();
        app.last_keystroke = Some(Instant::now() - Duration::from_millis(DEBOUNCE_MS + 1));
        app.tick();

        assert!(app.searching);
        assert_eq!(app.search_seq, 1);
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
        app.input = "query".to_string();
        app.search_paths = vec!["/project".to_string()];
        app.search_seq = 1;
        app.searching = true;

        let stale_result = (
            1,
            "query".to_string(),
            vec!["/all".to_string()],
            false,
            Ok(vec![RipgrepMatch {
                file_path: "/all/session.jsonl".to_string(),
                message: None,
                source: SessionSource::ClaudeCodeCLI,
            }]),
        );

        app.handle_search_result(stale_result);

        assert!(app.results.is_empty());
        assert!(app.groups.is_empty());
        assert!(app.searching);
    }
}
