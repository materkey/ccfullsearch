use crate::session::{self, SessionSource};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};

/// A node in the session DAG. Represents every uuid-bearing record.
#[derive(Debug, Clone)]
pub struct DagNode {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    #[allow(dead_code)]
    record_type: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub line_index: usize,
    /// Only populated for user/assistant
    pub role: Option<String>,
    /// First ~120 chars of content, sanitized
    pub content_preview: Option<String>,
}

/// A flattened row ready for display in the tree view.
#[derive(Debug, Clone)]
pub struct TreeRow {
    pub uuid: String,
    pub role: String,
    pub timestamp: DateTime<Utc>,
    pub content_preview: String,
    #[allow(dead_code)]
    pub depth: usize,
    pub graph_symbols: String,
    pub is_on_latest_chain: bool,
    pub is_branch_point: bool,
    /// True if this is an auto-compaction (summary) event
    pub is_compaction: bool,
}

/// The full parsed session tree.
pub struct SessionTree {
    /// All uuid-bearing nodes
    nodes: HashMap<String, DagNode>,
    /// parent_uuid -> vec of child uuids (ordered by line_index)
    children: HashMap<String, Vec<String>>,
    /// Root uuids (no parent)
    #[allow(dead_code)]
    roots: Vec<String>,
    /// Set of uuids on the latest chain
    latest_chain: HashSet<String>,
    /// Flattened display rows (only user/assistant messages)
    pub rows: Vec<TreeRow>,
    /// Session metadata
    pub session_id: String,
    pub file_path: String,
    pub source: SessionSource,
}

impl SessionTree {
    /// Parse a JSONL session file into a tree structure.
    pub fn from_file(file_path: &str) -> Result<Self, String> {
        let file = fs::File::open(file_path)
            .map_err(|e| format!("Failed to open {}: {}", file_path, e))?;
        let reader = BufReader::new(file);

        let mut nodes: HashMap<String, DagNode> = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        let mut roots: Vec<String> = Vec::new();
        let mut last_uuid: Option<String> = None;
        let mut session_id = String::new();

        for (line_idx, line_result) in reader.lines().enumerate() {
            let line =
                line_result.map_err(|e| format!("Read error at line {}: {}", line_idx, e))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let json: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let uuid = match session::extract_uuid(&json) {
                Some(u) => u,
                None => continue,
            };

            let parent_uuid = session::extract_parent_uuid(&json);

            let record_type = session::extract_record_type(&json)
                .unwrap_or("")
                .to_string();

            let timestamp = session::extract_timestamp(&json);

            // Extract role and content preview for displayable types
            let (role, content_preview) = if record_type == "user" || record_type == "assistant" {
                let message = json.get("message");
                let role = message
                    .and_then(|m| m.get("role"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let preview = message
                    .and_then(|m| m.get("content"))
                    .map(|c| extract_preview(c, 120));

                (role, preview)
            } else if record_type == "summary" {
                // Auto-compaction event — make it displayable
                let summary_text = json
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(auto-compacted)")
                    .to_string();
                (Some("compaction".to_string()), Some(summary_text))
            } else {
                (None, None)
            };

            // Capture session_id from first available record
            if session_id.is_empty() {
                if let Some(sid) = session::extract_session_id(&json) {
                    session_id = sid;
                }
            }

            // Track parent->child relationships
            match &parent_uuid {
                Some(parent) => {
                    children
                        .entry(parent.clone())
                        .or_default()
                        .push(uuid.clone());
                }
                None => {
                    roots.push(uuid.clone());
                }
            }

            nodes.insert(
                uuid.clone(),
                DagNode {
                    uuid: uuid.clone(),
                    parent_uuid,
                    record_type,
                    timestamp,
                    line_index: line_idx,
                    role,
                    content_preview,
                },
            );
            last_uuid = Some(uuid);
        }

        // Build latest chain
        let latest_chain = build_latest_chain(&nodes, last_uuid.as_deref());

        let source = SessionSource::from_path(file_path);

        let mut tree = SessionTree {
            nodes,
            children,
            roots,
            latest_chain,
            rows: Vec::new(),
            session_id,
            file_path: file_path.to_string(),
            source,
        };

        tree.flatten_to_rows();
        Ok(tree)
    }

    /// Number of branch points (nodes with >1 child in display graph)
    pub fn branch_count(&self) -> usize {
        self.children.values().filter(|kids| kids.len() > 1).count()
    }

    /// Get the full content of a message by reading its JSONL line from file.
    pub fn get_full_content(&self, uuid: &str) -> Option<String> {
        let node = self.nodes.get(uuid)?;
        let file = fs::File::open(&self.file_path).ok()?;
        let reader = BufReader::new(file);

        let line = reader.lines().nth(node.line_index)?.ok()?;
        let json: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        let content_raw = json.get("message")?.get("content")?;
        Some(crate::search::Message::extract_content(content_raw))
    }

    /// Build the display graph (only user/assistant) and flatten via DFS.
    fn flatten_to_rows(&mut self) {
        // Step 1: Build display graph — collapse non-displayable nodes
        let (display_children, display_roots) = self.build_display_graph();

        // Step 2: DFS with column tracking
        let mut rows: Vec<TreeRow> = Vec::new();
        let mut active_columns: Vec<bool> = Vec::new();

        for root in &display_roots {
            let col = find_free_column(&active_columns);
            if col >= active_columns.len() {
                active_columns.push(true);
            } else {
                active_columns[col] = true;
            }
            self.dfs_flatten(
                root,
                col,
                true, // is_last_child of root level
                &display_children,
                &mut active_columns,
                &mut rows,
            );
        }

        self.rows = rows;
    }

    /// Build a display graph that only connects user/assistant nodes,
    /// skipping intermediate progress/system nodes.
    fn build_display_graph(&self) -> (HashMap<String, Vec<String>>, Vec<String>) {
        let mut display_children: HashMap<String, Vec<String>> = HashMap::new();
        let mut display_roots: Vec<String> = Vec::new();

        // For each displayable node, find its displayable parent and register
        // as a child of that parent. If no displayable parent exists, it's a root.
        let mut displayable_parent_cache: HashMap<String, Option<String>> = HashMap::new();

        // Collect all displayable node uuids (as set for O(1) lookup + sorted vec for deterministic iteration)
        let displayable_uuids: HashSet<String> = self
            .nodes
            .values()
            .filter(|n| n.role.is_some() && n.content_preview.is_some())
            .map(|n| n.uuid.clone())
            .collect();

        // Sort by line_index for deterministic processing order
        let mut displayable_sorted: Vec<&String> = displayable_uuids.iter().collect();
        displayable_sorted.sort_by_key(|uuid| {
            self.nodes
                .get(uuid.as_str())
                .map(|n| n.line_index)
                .unwrap_or(0)
        });

        // For each displayable node, walk up parents until we find another displayable node
        for uuid in displayable_sorted {
            let display_parent = self.find_displayable_parent(
                uuid,
                &displayable_uuids,
                &mut displayable_parent_cache,
            );
            match display_parent {
                Some(parent_uuid) => {
                    display_children
                        .entry(parent_uuid)
                        .or_default()
                        .push(uuid.clone());
                }
                None => {
                    display_roots.push(uuid.clone());
                }
            }
        }

        // Sort children by line_index for consistent ordering
        for kids in display_children.values_mut() {
            kids.sort_by_key(|uuid| self.nodes.get(uuid).map(|n| n.line_index).unwrap_or(0));
        }

        // Sort roots by line_index
        display_roots.sort_by_key(|uuid| self.nodes.get(uuid).map(|n| n.line_index).unwrap_or(0));

        (display_children, display_roots)
    }

    /// Walk up parent chain to find the nearest displayable ancestor.
    fn find_displayable_parent(
        &self,
        uuid: &str,
        displayable: &HashSet<String>,
        cache: &mut HashMap<String, Option<String>>,
    ) -> Option<String> {
        let node = self.nodes.get(uuid)?;
        let mut current_parent = node.parent_uuid.clone();

        // Walk up through non-displayable nodes
        let mut visited = HashSet::new();
        while let Some(ref parent_uuid) = current_parent {
            if visited.contains(parent_uuid) {
                break; // Cycle protection
            }
            visited.insert(parent_uuid.clone());

            if let Some(cached) = cache.get(parent_uuid) {
                return cached.clone();
            }

            if displayable.contains(parent_uuid) {
                cache.insert(uuid.to_string(), Some(parent_uuid.clone()));
                return Some(parent_uuid.clone());
            }

            current_parent = self
                .nodes
                .get(parent_uuid)
                .and_then(|n| n.parent_uuid.clone());
        }

        cache.insert(uuid.to_string(), None);
        None
    }

    /// DFS traversal to build flat TreeRow list with graph symbols.
    fn dfs_flatten(
        &self,
        uuid: &str,
        column: usize,
        is_last_child: bool,
        display_children: &HashMap<String, Vec<String>>,
        active_columns: &mut Vec<bool>,
        rows: &mut Vec<TreeRow>,
    ) {
        let node = match self.nodes.get(uuid) {
            Some(n) => n,
            None => return,
        };

        let kids = display_children.get(uuid).cloned().unwrap_or_default();
        let is_branch_point = kids.len() > 1;
        let is_on_latest = self.latest_chain.contains(uuid);

        // Build graph symbols
        let graph = build_graph_symbols(column, active_columns, is_last_child, !kids.is_empty());

        let is_compaction = node.role.as_deref() == Some("compaction")
            || is_context_loss_message(&node.content_preview);

        rows.push(TreeRow {
            uuid: uuid.to_string(),
            role: node.role.clone().unwrap_or_else(|| "?".to_string()),
            timestamp: node.timestamp.unwrap_or_else(Utc::now),
            content_preview: node.content_preview.clone().unwrap_or_default(),
            depth: column,
            graph_symbols: graph,
            is_on_latest_chain: is_on_latest,
            is_branch_point,
            is_compaction,
        });

        if kids.is_empty() {
            // Leaf: free column
            if column < active_columns.len() {
                active_columns[column] = false;
            }
            return;
        }

        // Sort children: latest chain first, then by line_index
        let mut sorted_kids = kids;
        sorted_kids.sort_by(|a, b| {
            let a_latest = self.is_descendant_of_latest(a, display_children);
            let b_latest = self.is_descendant_of_latest(b, display_children);
            // Latest chain first (true > false when reversed)
            b_latest.cmp(&a_latest).then_with(|| {
                let a_idx = self.nodes.get(a).map(|n| n.line_index).unwrap_or(0);
                let b_idx = self.nodes.get(b).map(|n| n.line_index).unwrap_or(0);
                a_idx.cmp(&b_idx)
            })
        });

        let num_kids = sorted_kids.len();
        for (i, child) in sorted_kids.into_iter().enumerate() {
            let is_last = i == num_kids - 1;
            if i == 0 {
                // First child continues on same column
                self.dfs_flatten(
                    &child,
                    column,
                    is_last,
                    display_children,
                    active_columns,
                    rows,
                );
            } else {
                // Allocate new column for branch
                let new_col = find_free_column(active_columns);
                if new_col >= active_columns.len() {
                    active_columns.push(true);
                } else {
                    active_columns[new_col] = true;
                }
                self.dfs_flatten(
                    &child,
                    new_col,
                    is_last,
                    display_children,
                    active_columns,
                    rows,
                );
            }
        }
    }

    /// Check if a node or any of its descendants is on the latest chain.
    fn is_descendant_of_latest(
        &self,
        uuid: &str,
        display_children: &HashMap<String, Vec<String>>,
    ) -> bool {
        if self.latest_chain.contains(uuid) {
            return true;
        }
        if let Some(kids) = display_children.get(uuid) {
            for kid in kids {
                if self.is_descendant_of_latest(kid, display_children) {
                    return true;
                }
            }
        }
        false
    }
}

/// Build the latest chain by walking backwards from the tip (last uuid in file).
fn build_latest_chain(
    nodes: &HashMap<String, DagNode>,
    last_uuid: Option<&str>,
) -> HashSet<String> {
    let mut chain = HashSet::new();
    let Some(tip) = last_uuid else {
        return chain;
    };

    let mut current = Some(tip.to_string());
    while let Some(uuid) = current {
        chain.insert(uuid.clone());
        current = nodes.get(&uuid).and_then(|n| n.parent_uuid.clone());
    }
    chain
}

/// Find the first free (false) column, or return the length (meaning append).
fn find_free_column(active_columns: &[bool]) -> usize {
    // Start from column 1 to keep column 0 for main trunk
    for (i, active) in active_columns.iter().enumerate().skip(1) {
        if !active {
            return i;
        }
    }
    active_columns.len()
}

/// Build the graph gutter string for a row.
fn build_graph_symbols(
    column: usize,
    active_columns: &[bool],
    _is_last_child: bool,
    _has_children: bool,
) -> String {
    let max_col = active_columns.len().max(column + 1);
    let mut result = String::new();

    for col in 0..max_col {
        if col == column {
            result.push_str("* ");
        } else if col < active_columns.len() && active_columns[col] {
            result.push_str("| ");
        } else {
            result.push_str("  ");
        }
    }

    result
}

/// Extract a short preview from message content (first N chars).
fn extract_preview(content: &serde_json::Value, max_chars: usize) -> String {
    let text = if let Some(s) = content.as_str() {
        s.to_string()
    } else if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "text" => {
                    if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                        parts.push(t.to_string());
                    }
                }
                "tool_use" => {
                    if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                        parts.push(format!("[tool: {}]", name));
                    }
                }
                "tool_result" => {
                    parts.push("[tool_result]".to_string());
                }
                _ => {}
            }
        }
        parts.join(" ")
    } else {
        String::new()
    };

    // Sanitize: strip XML tags, remove newlines, multiple spaces
    let stripped = strip_xml_tags(&text);
    let sanitized = stripped
        .replace('\n', " ")
        .replace('\r', "")
        .replace('\t', " ");
    // Collapse multiple spaces
    let mut prev_space = false;
    let collapsed: String = sanitized
        .chars()
        .filter(|c| {
            if *c == ' ' {
                if prev_space {
                    return false;
                }
                prev_space = true;
            } else {
                prev_space = false;
            }
            true
        })
        .collect();

    if collapsed.chars().count() > max_chars {
        collapsed.chars().take(max_chars).collect::<String>() + "..."
    } else {
        collapsed
    }
}

/// Strip XML/HTML-like tags from text, keeping only inner content.
fn strip_xml_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                result.push(' '); // replace tag with space
            }
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

/// Detect messages related to context loss / auto-compaction by content.
/// These are regular user/assistant messages but carry compaction metadata.
fn is_context_loss_message(content_preview: &Option<String>) -> bool {
    let Some(preview) = content_preview else {
        return false;
    };
    let lower = preview.to_lowercase();
    lower.contains("being continued from a previous conversation that ran out of context")
        || lower.contains("/compact")
        || lower.contains("compacted (ctrl+o to see full summary)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Helper: create a simple linear session (no branches)
    fn create_linear_session(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("linear.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi there!"}}]}},"uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Fix the bug"}}]}},"uuid":"u3","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Done fixing"}}]}},"uuid":"u4","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        path
    }

    /// Helper: create a branched session with progress nodes
    fn create_branched_session(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("branched.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Common: progress(p1) -> user(a1) -> assistant(a2)
        writeln!(f, r#"{{"type":"progress","uuid":"p1","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"a1","parentUuid":"p1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi"}}]}},"uuid":"a2","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        // Branch A: system(a3) -> user(a4) -> assistant(a5)
        writeln!(f, r#"{{"type":"system","uuid":"a3","parentUuid":"a2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Branch A msg"}}]}},"uuid":"a4","parentUuid":"a3","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Branch A reply"}}]}},"uuid":"a5","parentUuid":"a4","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        // Branch B: system(b3) -> user(b4) -> assistant(b5)
        writeln!(f, r#"{{"type":"system","uuid":"b3","parentUuid":"a2","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Branch B msg"}}]}},"uuid":"b4","parentUuid":"b3","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Branch B reply"}}]}},"uuid":"b5","parentUuid":"b4","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();
        path
    }

    #[test]
    fn test_linear_session_parse() {
        let dir = TempDir::new().unwrap();
        let path = create_linear_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        assert_eq!(tree.rows.len(), 4);
        assert_eq!(tree.session_id, "s1");
        assert_eq!(tree.branch_count(), 0);
    }

    #[test]
    fn test_linear_session_order() {
        let dir = TempDir::new().unwrap();
        let path = create_linear_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        assert_eq!(tree.rows[0].role, "user");
        assert!(tree.rows[0].content_preview.contains("Hello"));
        assert_eq!(tree.rows[1].role, "assistant");
        assert!(tree.rows[1].content_preview.contains("Hi there"));
        assert_eq!(tree.rows[2].role, "user");
        assert!(tree.rows[2].content_preview.contains("Fix the bug"));
        assert_eq!(tree.rows[3].role, "assistant");
        assert!(tree.rows[3].content_preview.contains("Done fixing"));
    }

    #[test]
    fn test_linear_all_on_latest_chain() {
        let dir = TempDir::new().unwrap();
        let path = create_linear_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        for row in &tree.rows {
            assert!(
                row.is_on_latest_chain,
                "All linear messages should be on latest chain"
            );
        }
    }

    #[test]
    fn test_branched_session_parse() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        // Should have 6 displayable messages: a1, a2, a4, a5, b4, b5
        assert_eq!(tree.rows.len(), 6);
    }

    #[test]
    fn test_branched_session_has_branches() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        // a2 has two displayable children (a4 via a3, and b4 via b3) so it's a branch point
        assert!(
            tree.branch_count() >= 1,
            "Should have at least one branch point"
        );
    }

    #[test]
    fn test_branched_latest_chain() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        // b5 is last line, so latest chain includes: b5, b4, b3, a2, a1, p1
        // In display rows, b4 and b5 should be on latest chain
        // a4 and a5 should NOT be on latest chain

        let find_row = |content: &str| {
            tree.rows
                .iter()
                .find(|r| r.content_preview.contains(content))
                .unwrap()
        };

        assert!(find_row("Branch B msg").is_on_latest_chain);
        assert!(find_row("Branch B reply").is_on_latest_chain);
        assert!(!find_row("Branch A msg").is_on_latest_chain);
        assert!(!find_row("Branch A reply").is_on_latest_chain);
        // Common ancestors
        assert!(find_row("Hello").is_on_latest_chain);
        assert!(find_row("Hi").is_on_latest_chain);
    }

    #[test]
    fn test_branched_latest_chain_first_in_order() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        // After common messages (Hello, Hi), the latest chain branch should come first
        // Row 0: Hello (common)
        // Row 1: Hi (common)
        // Then latest chain branch (B) should come before branch A

        let b_msg_idx = tree
            .rows
            .iter()
            .position(|r| r.content_preview.contains("Branch B msg"))
            .unwrap();
        let a_msg_idx = tree
            .rows
            .iter()
            .position(|r| r.content_preview.contains("Branch A msg"))
            .unwrap();

        assert!(
            b_msg_idx < a_msg_idx,
            "Latest chain (B) should appear before fork (A)"
        );
    }

    #[test]
    fn test_get_full_content() {
        let dir = TempDir::new().unwrap();
        let path = create_linear_session(&dir);
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        let content = tree.get_full_content("u1").unwrap();
        assert_eq!(content, "Hello");
    }

    #[test]
    fn test_compaction_event_visible() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("compact.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi there"}}]}},"uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Discussed greeting","leafUuid":"u2","uuid":"s1sum","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Continue after compact"}}]}},"uuid":"u3","parentUuid":"s1sum","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Sure!"}}]}},"uuid":"u4","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();

        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();

        // Should have 5 rows: u1, u2, summary, u3, u4
        assert_eq!(tree.rows.len(), 5);

        // Find the compaction row
        let compact_row = tree.rows.iter().find(|r| r.is_compaction).unwrap();
        assert_eq!(compact_row.role, "compaction");
        assert!(compact_row.content_preview.contains("Discussed greeting"));

        // Non-compaction rows should not be marked
        let user_rows: Vec<_> = tree.rows.iter().filter(|r| !r.is_compaction).collect();
        assert_eq!(user_rows.len(), 4);
    }

    #[test]
    fn test_compaction_without_uuid_not_displayed() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("compact_no_uuid.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        // Summary without uuid — should be skipped entirely (no uuid = not in DAG)
        writeln!(
            f,
            r#"{{"type":"summary","summary":"Some summary","leafUuid":"u1","sessionId":"s1"}}"#
        )
        .unwrap();

        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();
        // Only the user message should be visible
        assert_eq!(tree.rows.len(), 1);
        assert!(!tree.rows[0].is_compaction);
    }

    #[test]
    fn test_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        fs::write(&path, "").unwrap();
        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();
        assert!(tree.rows.is_empty());
    }

    #[test]
    fn test_extract_preview_plain_string() {
        let content = serde_json::json!("Hello world this is a test message");
        let preview = extract_preview(&content, 20);
        assert_eq!(preview, "Hello world this is ...");
    }

    #[test]
    fn test_extract_preview_array() {
        let content = serde_json::json!([
            {"type": "text", "text": "Part one"},
            {"type": "tool_use", "name": "Read"},
        ]);
        let preview = extract_preview(&content, 100);
        assert!(preview.contains("Part one"));
        assert!(preview.contains("[tool: Read]"));
    }

    #[test]
    fn test_extract_preview_collapses_whitespace() {
        let content = serde_json::json!("Hello\n\n  world\t\ttab");
        let preview = extract_preview(&content, 100);
        assert!(!preview.contains('\n'));
        assert!(!preview.contains('\t'));
    }

    #[test]
    fn test_graph_symbols_single_column() {
        let active = vec![true];
        let symbols = build_graph_symbols(0, &active, true, true);
        assert!(symbols.contains('*'));
    }

    #[test]
    fn test_graph_symbols_multiple_columns() {
        let active = vec![true, true];
        let symbols = build_graph_symbols(1, &active, false, true);
        assert!(symbols.contains('|'));
        assert!(symbols.contains('*'));
    }
}
