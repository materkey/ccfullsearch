use crate::tree::SessionTree;
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
                                    if audit.exists()
                                        && Self::file_contains_session_id(&audit, session_id)
                                    {
                                        return Some(audit.to_string_lossy().to_string());
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
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        let reader = BufReader::new(file);
        for line in reader.lines().take(5).flatten() {
            if line.contains(session_id) {
                return true;
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
}

#[cfg(test)]
mod tests {
    use crate::tui::App;

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
