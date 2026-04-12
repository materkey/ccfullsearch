use crate::search::extract_project_from_path;
use crate::tree::SessionTree;
use crate::tui::state::{AppOutcome, PickedSession, ResumeTarget};
use crate::tui::App;
use std::sync::mpsc;
use std::thread;

impl App {
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
                    self.search.error = Some(format!("Session not found: {}", target));
                    return;
                }
            }
        };
        self.tree.tree_mode_standalone = true;
        self.enter_tree_mode_for_file(&file_path);
    }

    pub(crate) fn enter_tree_mode_for_file(&mut self, file_path: &str) {
        self.tree_mode = true;
        self.tree.tree_loading = true;
        self.tree.tree_cursor = 0;
        self.tree.tree_scroll_offset = 0;
        self.tree.session_tree = None;
        self.preview_mode = false;
        self.needs_full_redraw = true;

        let fp = file_path.to_string();
        let (tx, rx) = mpsc::channel();
        self.tree.tree_load_rx = Some(rx);

        thread::spawn(move || {
            let result = SessionTree::from_file(&fp);
            let _ = tx.send(result);
        });
    }

    /// Search for a JSONL file by session ID across search paths.
    fn find_session_file(&self, session_id: &str) -> Option<String> {
        crate::session::find_session_file_in_paths(session_id, &self.search_paths)
    }

    pub fn exit_tree_mode(&mut self) {
        if self.tree.tree_mode_standalone {
            self.should_quit = true;
            return;
        }
        self.tree_mode = false;
        self.tree.session_tree = None;
        self.tree.tree_loading = false;
        self.tree.tree_load_rx = None;
        self.preview_mode = false;
        self.needs_full_redraw = true;
    }

    pub fn on_up_tree(&mut self) {
        if self.tree.tree_cursor > 0 {
            self.tree.tree_cursor -= 1;
            self.adjust_tree_scroll();
            if self.preview_mode {
                self.needs_full_redraw = true;
            }
        }
    }

    pub fn on_down_tree(&mut self) {
        if let Some(ref tree) = self.tree.session_tree {
            if self.tree.tree_cursor < tree.rows.len().saturating_sub(1) {
                self.tree.tree_cursor += 1;
                self.adjust_tree_scroll();
                if self.preview_mode {
                    self.needs_full_redraw = true;
                }
            }
        }
    }

    pub fn on_left_tree(&mut self) {
        // Jump to previous branch point
        if let Some(ref tree) = self.tree.session_tree {
            for i in (0..self.tree.tree_cursor).rev() {
                if tree.rows[i].is_branch_point {
                    self.tree.tree_cursor = i;
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
        if let Some(ref tree) = self.tree.session_tree {
            for i in (self.tree.tree_cursor + 1)..tree.rows.len() {
                if tree.rows[i].is_branch_point {
                    self.tree.tree_cursor = i;
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

        if let Some(ref tree) = self.tree.session_tree {
            if let Some(row) = tree.rows.get(self.tree.tree_cursor) {
                if self.picker_mode {
                    let project = extract_project_from_path(&tree.file_path);
                    self.outcome = Some(AppOutcome::Pick(PickedSession {
                        session_id: tree.session_id.clone(),
                        file_path: tree.file_path.clone(),
                        source: tree.source,
                        project,
                        message_uuid: Some(row.uuid.clone()),
                    }));
                } else {
                    self.outcome = Some(AppOutcome::Resume(ResumeTarget {
                        session_id: tree.session_id.clone(),
                        file_path: tree.file_path.clone(),
                        source: tree.source,
                        uuid: Some(row.uuid.clone()),
                    }));
                }
                self.should_quit = true;
            }
        }
    }

    pub fn on_tab_tree(&mut self) {
        if let Some(ref tree) = self.tree.session_tree {
            if !tree.rows.is_empty() {
                self.preview_mode = !self.preview_mode;
                self.needs_full_redraw = true;
            }
        }
    }

    fn adjust_tree_scroll(&mut self) {
        let visible = self.last_tree_visible_height;
        if self.tree.tree_cursor < self.tree.tree_scroll_offset {
            self.tree.tree_scroll_offset = self.tree.tree_cursor;
        } else if self.tree.tree_cursor >= self.tree.tree_scroll_offset + visible {
            self.tree.tree_scroll_offset = self.tree.tree_cursor.saturating_sub(visible) + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::tui::state::AppOutcome;
    use crate::tui::App;

    #[test]
    fn test_exit_tree_mode_returns_to_search() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.tree_mode = true;
        app.tree.tree_mode_standalone = false;

        app.exit_tree_mode();

        assert!(!app.tree_mode);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_exit_tree_mode_standalone_quits() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.tree_mode = true;
        app.tree.tree_mode_standalone = true;

        app.exit_tree_mode();

        assert!(app.should_quit);
    }

    #[test]
    fn test_on_enter_tree_picker_mode_sets_picked_session() {
        use crate::session::SessionSource;
        use crate::tree::{SessionTree, TreeRow};
        use chrono::Utc;

        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;
        app.tree_mode = true;

        let tree = SessionTree::new_for_test(
            "tree-sess-1".to_string(),
            "/home/user/.claude/projects/-Users-user-projects-coolapp/session.jsonl".to_string(),
            SessionSource::ClaudeCodeCLI,
            vec![TreeRow {
                uuid: "uuid-tree-1".to_string(),
                role: "user".to_string(),
                timestamp: Utc::now(),
                content_preview: "hello world".to_string(),
                graph_symbols: "│ ".to_string(),
                is_on_latest_chain: true,
                is_branch_point: false,
                is_compaction: false,
            }],
        );
        app.tree.session_tree = Some(tree);
        app.tree.tree_cursor = 0;

        app.on_enter_tree();

        assert!(app.should_quit);
        let picked = match &app.outcome {
            Some(AppOutcome::Pick(p)) => p,
            other => panic!("Expected Pick, got {:?}", other),
        };
        assert_eq!(picked.session_id, "tree-sess-1");
        assert_eq!(picked.source, SessionSource::ClaudeCodeCLI);
        assert_eq!(picked.project, "coolapp");
    }
}
