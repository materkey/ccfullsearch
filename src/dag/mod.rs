use crate::session::{self, record::SessionRecord};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Strategy for selecting the tip (terminal node) of the conversation DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipStrategy {
    /// Pick the last displayable UUID (by file order) that has no displayable children.
    /// Fallback: last UUID seen (any type).
    /// Used by: resume/fork.rs, recent.rs
    LastAppended,
    /// Pick the displayable terminal UUID with the maximum timestamp.
    /// Fallback: last node by line_index.
    /// Used by: tree/mod.rs
    MaxTimestamp,
}

/// Controls which record types count as "displayable" for tip selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayFilter {
    /// user + assistant + compaction are displayable.
    Standard,
    /// Only user + assistant are displayable (no compaction).
    MessagesOnly,
}

/// A single entry in the session DAG.
#[derive(Debug, Clone)]
pub struct DagEntry {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub is_displayable: bool,
    pub line_index: usize,
}

/// Unified DAG engine for Claude session files.
///
/// Replaces the three duplicated DAG implementations in:
/// - `resume/fork.rs` (DagInfo + parse_dag + find_tip + build_chain)
/// - `recent.rs` (build_latest_chain)
/// - `tree/mod.rs` (build_latest_chain)
///
/// Sidechain records are excluded during construction.
pub struct SessionDag {
    entries: HashMap<String, DagEntry>,
    displayable_order: Vec<String>,
    last_uuid: Option<String>,
}

impl SessionDag {
    /// Build a DAG from a JSONL session file.
    /// Skips sidechain records. Uses session:: extractors for field access.
    pub fn from_file(path: &Path, filter: DisplayFilter) -> Result<Self, String> {
        let file =
            File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
        let reader = BufReader::new(file);

        let mut entries = HashMap::new();
        let mut displayable_order = Vec::new();
        let mut last_uuid = None;

        for (line_index, line_result) in reader.lines().enumerate() {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => continue,
            };

            // Fast pre-filter: skip lines without "uuid" to avoid JSON parsing overhead
            if !line.contains("\"uuid\"") {
                continue;
            }

            let json: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if session::is_sidechain(&json) {
                continue;
            }

            let Some(uuid) = session::extract_uuid(&json) else {
                continue;
            };
            let parent_uuid = session::extract_parent_uuid_or_logical(&json);

            let record_type = session::extract_record_type(&json);
            let is_displayable = is_displayable_type(record_type, filter);
            let timestamp = session::extract_timestamp(&json);

            if is_displayable {
                displayable_order.push(uuid.clone());
            }

            entries.insert(
                uuid.clone(),
                DagEntry {
                    uuid: uuid.clone(),
                    parent_uuid,
                    timestamp,
                    is_displayable,
                    line_index,
                },
            );

            last_uuid = Some(uuid);
        }

        Ok(SessionDag {
            entries,
            displayable_order,
            last_uuid,
        })
    }

    /// Build a DAG from pre-parsed SessionRecords.
    /// Each item is (record, line_index, optional_timestamp).
    /// Skips sidechain records and records without a DAG uuid.
    pub fn from_records<I>(records: I, filter: DisplayFilter) -> Self
    where
        I: Iterator<Item = (SessionRecord, usize, Option<DateTime<Utc>>)>,
    {
        let mut entries = HashMap::new();
        let mut displayable_order = Vec::new();
        let mut last_uuid = None;

        for (record, line_index, timestamp) in records {
            if record.is_sidechain() {
                continue;
            }

            let Some(uuid) = record.dag_uuid().map(|s| s.to_string()) else {
                continue;
            };
            let parent_uuid = record.dag_parent_uuid().map(|s| s.to_string());

            let is_displayable = is_record_displayable(&record, filter);

            if is_displayable {
                displayable_order.push(uuid.clone());
            }

            entries.insert(
                uuid.clone(),
                DagEntry {
                    uuid: uuid.clone(),
                    parent_uuid,
                    timestamp,
                    is_displayable,
                    line_index,
                },
            );

            last_uuid = Some(uuid);
        }

        SessionDag {
            entries,
            displayable_order,
            last_uuid,
        }
    }

    /// Select the tip (terminal node) of the conversation using the given strategy.
    pub fn tip(&self, strategy: TipStrategy) -> Option<&str> {
        let displayable_parent_set = self.displayable_parent_set();

        match strategy {
            TipStrategy::LastAppended => {
                // Scan displayable UUIDs in reverse (last-seen first), pick
                // the first one with no displayable children. Non-displayable
                // attachment/system records are DAG nodes, but they should not
                // make the preceding user/assistant message stop being the
                // resumable conversation tip.
                self.displayable_order
                    .iter()
                    .rev()
                    .find(|uuid| !displayable_parent_set.contains(*uuid))
                    .map(|s| s.as_str())
                    .or(self.last_uuid.as_deref())
            }
            TipStrategy::MaxTimestamp => {
                // Among displayable terminal nodes, pick the one with max timestamp.
                let tip = self
                    .displayable_order
                    .iter()
                    .filter(|uuid| !displayable_parent_set.contains(*uuid))
                    .filter_map(|uuid| self.entries.get(uuid))
                    .filter(|e| e.timestamp.is_some())
                    .max_by_key(|e| e.timestamp)
                    .map(|e| e.uuid.as_str());

                // Fallback: last node by line_index (any type)
                tip.or_else(|| {
                    self.entries
                        .values()
                        .max_by_key(|e| e.line_index)
                        .map(|e| e.uuid.as_str())
                })
            }
        }
    }

    /// Walk the chain from `tip` back to the root, returning all UUIDs on the path.
    /// Cycle-safe: stops if a UUID is visited twice.
    pub fn chain_from(&self, tip: &str) -> HashSet<String> {
        let mut chain = HashSet::new();
        let mut current = Some(tip.to_string());
        while let Some(uuid) = current {
            if !chain.insert(uuid.clone()) {
                break; // cycle detected
            }
            current = self.entries.get(&uuid).and_then(|e| e.parent_uuid.clone());
        }
        chain
    }

    /// Returns the number of entries in the DAG.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the DAG has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of displayable entries.
    pub fn displayable_count(&self) -> usize {
        self.displayable_order.len()
    }

    /// Get an entry by UUID.
    pub fn get(&self, uuid: &str) -> Option<&DagEntry> {
        self.entries.get(uuid)
    }

    /// Build the set of displayable nodes that have a displayable child in the
    /// collapsed display graph. Non-displayable bridge nodes are walked through.
    fn displayable_parent_set(&self) -> HashSet<String> {
        let displayable: HashSet<&str> =
            self.displayable_order.iter().map(String::as_str).collect();
        let mut parents = HashSet::new();

        for uuid in &self.displayable_order {
            let mut current_parent = self.entries.get(uuid).and_then(|e| e.parent_uuid.clone());
            let mut visited = HashSet::new();

            while let Some(parent_uuid) = current_parent {
                if !visited.insert(parent_uuid.clone()) {
                    break;
                }

                if displayable.contains(parent_uuid.as_str()) {
                    parents.insert(parent_uuid);
                    break;
                }

                current_parent = self
                    .entries
                    .get(&parent_uuid)
                    .and_then(|e| e.parent_uuid.clone());
            }
        }

        parents
    }
}

/// Check if a raw record type string is displayable under the given filter.
fn is_displayable_type(record_type: Option<&str>, filter: DisplayFilter) -> bool {
    match filter {
        DisplayFilter::Standard => {
            matches!(
                record_type,
                Some("user" | "assistant" | "compaction" | "summary")
            )
        }
        DisplayFilter::MessagesOnly => {
            matches!(record_type, Some("user" | "assistant"))
        }
    }
}

/// Check if a parsed SessionRecord is displayable under the given filter.
fn is_record_displayable(record: &SessionRecord, filter: DisplayFilter) -> bool {
    match record {
        SessionRecord::Message { .. } => true,
        SessionRecord::Summary { .. } => filter == DisplayFilter::Standard,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::record::{ContentBlock, MessageRole};

    // --- Helper to build records for from_records tests ---

    fn user_record(
        uuid: &str,
        parent: Option<&str>,
        ts: Option<&str>,
    ) -> (SessionRecord, usize, Option<DateTime<Utc>>) {
        let record = SessionRecord::Message {
            role: MessageRole::User,
            content_blocks: vec![ContentBlock::Text("test".into())],
            uuid: Some(uuid.to_string()),
            parent_uuid: parent.map(|s| s.to_string()),
            is_sidechain: false,
        };
        let timestamp = ts
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        (record, 0, timestamp)
    }

    fn assistant_record(
        uuid: &str,
        parent: Option<&str>,
        ts: Option<&str>,
        line_index: usize,
    ) -> (SessionRecord, usize, Option<DateTime<Utc>>) {
        let record = SessionRecord::Message {
            role: MessageRole::Assistant,
            content_blocks: vec![ContentBlock::Text("response".into())],
            uuid: Some(uuid.to_string()),
            parent_uuid: parent.map(|s| s.to_string()),
            is_sidechain: false,
        };
        let timestamp = ts
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        (record, line_index, timestamp)
    }

    fn compaction_record(
        uuid: &str,
        parent: Option<&str>,
    ) -> (SessionRecord, usize, Option<DateTime<Utc>>) {
        let record = SessionRecord::Summary {
            text: "compacted".to_string(),
            is_compaction: true,
            uuid: Some(uuid.to_string()),
            parent_uuid: parent.map(|s| s.to_string()),
            leaf_uuid: None,
            is_sidechain: false,
        };
        (record, 0, None)
    }

    fn sidechain_record(
        uuid: &str,
        parent: Option<&str>,
    ) -> (SessionRecord, usize, Option<DateTime<Utc>>) {
        let record = SessionRecord::Message {
            role: MessageRole::User,
            content_blocks: vec![],
            uuid: Some(uuid.to_string()),
            parent_uuid: parent.map(|s| s.to_string()),
            is_sidechain: true,
        };
        (record, 0, None)
    }

    fn compact_boundary_record(
        uuid: &str,
        parent: Option<&str>,
        logical_parent: Option<&str>,
    ) -> (SessionRecord, usize, Option<DateTime<Utc>>) {
        let record = SessionRecord::CompactBoundary {
            uuid: Some(uuid.to_string()),
            parent_uuid: parent.map(|s| s.to_string()),
            logical_parent_uuid: logical_parent.map(|s| s.to_string()),
            is_sidechain: false,
        };
        (record, 0, None)
    }

    fn summary_record(
        uuid: &str,
        parent: Option<&str>,
    ) -> (SessionRecord, usize, Option<DateTime<Utc>>) {
        let record = SessionRecord::Summary {
            text: "summary".to_string(),
            is_compaction: false,
            uuid: Some(uuid.to_string()),
            parent_uuid: parent.map(|s| s.to_string()),
            leaf_uuid: None,
            is_sidechain: false,
        };
        (record, 0, None)
    }

    fn metadata_record(
        uuid: &str,
        parent: Option<&str>,
    ) -> (SessionRecord, usize, Option<DateTime<Utc>>) {
        let record = SessionRecord::Metadata {
            uuid: Some(uuid.to_string()),
            parent_uuid: parent.map(|s| s.to_string()),
            is_sidechain: false,
        };
        (record, 0, None)
    }

    // --- TipStrategy::LastAppended tests ---

    #[test]
    fn test_tip_last_appended_linear() {
        // Linear chain: u1 -> u2 -> u3 -> u4
        // Tip should be u4 (last displayable, no children)
        let records = vec![
            user_record("u1", None, None),
            assistant_record("u2", Some("u1"), None, 1),
            user_record("u3", Some("u2"), None),
            assistant_record("u4", Some("u3"), None, 3),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("u4"));
    }

    #[test]
    fn test_tip_last_appended_ignores_non_displayable_tail() {
        // Real Claude Code files often append attachment/system records after
        // the last user/assistant message. They are DAG children, but they are
        // not valid conversation rows to resume from.
        let records = vec![
            user_record("u1", None, None),
            assistant_record("u2", Some("u1"), None, 1),
            metadata_record("sys1", Some("u2")),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);

        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("u2"));
        assert_eq!(dag.tip(TipStrategy::MaxTimestamp), Some("u2"));
    }

    #[test]
    fn test_tip_last_appended_branched() {
        // Branch: u1 -> a1 -> a2 (branch A)
        //         u1 -> b1 -> b2 (branch B, appended later)
        // Tip should be b2 (last appended displayable terminal)
        let records = vec![
            user_record("u1", None, None),
            assistant_record("a1", Some("u1"), None, 1),
            user_record("a2", Some("a1"), None),
            user_record("b1", Some("u1"), None),
            assistant_record("b2", Some("b1"), None, 4),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("b2"));
    }

    #[test]
    fn test_tip_last_appended_sidechains_excluded() {
        // u1 -> u2 -> sidechain_u3
        // Sidechain records are skipped; tip should be u2
        let records = vec![
            user_record("u1", None, None),
            assistant_record("u2", Some("u1"), None, 1),
            sidechain_record("sc1", Some("u2")),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("u2"));
        assert_eq!(dag.len(), 2); // sidechain not stored
    }

    #[test]
    fn test_tip_last_appended_compact_boundary_bridging() {
        // Pre-boundary: u1 -> u2
        // Boundary: cb1 (logicalParent=u2)
        // Post-boundary: u3 (parent=cb1) -> u4 (parent=u3)
        // Tip should be u4, and chain should include all: u4, u3, cb1, u2, u1
        let records = vec![
            user_record("u1", None, None),
            assistant_record("u2", Some("u1"), None, 1),
            compact_boundary_record("cb1", None, Some("u2")),
            user_record("u3", Some("cb1"), None),
            assistant_record("u4", Some("u3"), None, 4),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("u4"));

        let chain = dag.chain_from("u4");
        assert!(chain.contains("u4"));
        assert!(chain.contains("u3"));
        assert!(chain.contains("cb1"));
        assert!(chain.contains("u2"));
        assert!(chain.contains("u1"));
    }

    #[test]
    fn test_tip_last_appended_compact_boundary_no_logical_parent() {
        // Pre-boundary: u1 -> u2
        // Boundary: cb1 (no logicalParentUuid)
        // Post-boundary: u3 (parent=cb1) -> u4
        // Chain from u4 should stop at cb1 (no link to pre-boundary)
        let records = vec![
            user_record("u1", None, None),
            assistant_record("u2", Some("u1"), None, 1),
            compact_boundary_record("cb1", None, None),
            user_record("u3", Some("cb1"), None),
            assistant_record("u4", Some("u3"), None, 4),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);

        let chain = dag.chain_from("u4");
        assert!(chain.contains("u4"));
        assert!(chain.contains("u3"));
        assert!(chain.contains("cb1"));
        assert!(!chain.contains("u2")); // pre-boundary unreachable
        assert!(!chain.contains("u1"));
    }

    #[test]
    fn test_tip_last_appended_fallback_to_last_uuid() {
        // Only non-displayable records (compact_boundary, summary)
        // Fallback should pick last_uuid
        let records = vec![
            compact_boundary_record("cb1", None, None),
            summary_record("s1", Some("cb1")),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("s1"));
    }

    #[test]
    fn test_tip_last_appended_empty_dag() {
        let dag = SessionDag::from_records(std::iter::empty(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::LastAppended), None);
        assert!(dag.is_empty());
    }

    // --- TipStrategy::MaxTimestamp tests ---

    #[test]
    fn test_tip_max_timestamp_basic() {
        // u1 (t=10:00) -> u2 (t=10:01) -> u3 (t=10:02, latest timestamp)
        let records = vec![
            user_record("u1", None, Some("2025-06-01T10:00:00Z")),
            assistant_record("u2", Some("u1"), Some("2025-06-01T10:01:00Z"), 1),
            user_record("u3", Some("u2"), Some("2025-06-01T10:02:00Z")),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::MaxTimestamp), Some("u3"));
    }

    #[test]
    fn test_tip_max_timestamp_branched_picks_latest() {
        // Branch A: u1 -> a1 (t=10:01) -> a2 (t=10:02)
        // Branch B: u1 -> b1 (t=10:03) -> b2 (t=10:04, latest!)
        // MaxTimestamp should pick b2
        let records = vec![
            user_record("u1", None, Some("2025-06-01T10:00:00Z")),
            assistant_record("a1", Some("u1"), Some("2025-06-01T10:01:00Z"), 1),
            user_record("a2", Some("a1"), Some("2025-06-01T10:02:00Z")),
            user_record("b1", Some("u1"), Some("2025-06-01T10:03:00Z")),
            assistant_record("b2", Some("b1"), Some("2025-06-01T10:04:00Z"), 4),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::MaxTimestamp), Some("b2"));
    }

    #[test]
    fn test_tip_max_timestamp_clock_skew() {
        // Branch A appended first with later timestamps (clock skew)
        // Branch B appended later with earlier timestamps
        // MaxTimestamp should still pick from branch A (higher timestamp)
        let records = vec![
            user_record("u1", None, Some("2025-06-01T10:00:00Z")),
            assistant_record("a1", Some("u1"), Some("2025-06-01T10:05:00Z"), 1),
            user_record("a2", Some("a1"), Some("2025-06-01T10:06:00Z")),
            user_record("b1", Some("u1"), Some("2025-06-01T10:01:00Z")),
            assistant_record("b2", Some("b1"), Some("2025-06-01T10:02:00Z"), 4),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        // a2 has timestamp 10:06 > b2's 10:02
        assert_eq!(dag.tip(TipStrategy::MaxTimestamp), Some("a2"));
    }

    #[test]
    fn test_tip_max_timestamp_equal_timestamps() {
        // Two terminal nodes with equal timestamps
        // Behavior: max_by_key picks one deterministically (last in iteration order for HashMap)
        let records = vec![
            user_record("u1", None, Some("2025-06-01T10:00:00Z")),
            assistant_record("a1", Some("u1"), Some("2025-06-01T10:01:00Z"), 1),
            assistant_record("b1", Some("u1"), Some("2025-06-01T10:01:00Z"), 2),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        let tip = dag.tip(TipStrategy::MaxTimestamp);
        // Both a1 and b1 are valid — just verify one is picked
        assert!(tip == Some("a1") || tip == Some("b1"));
    }

    #[test]
    fn test_tip_max_timestamp_no_timestamps_fallback() {
        // All records have no timestamps — fallback to max line_index
        let mut r1 = user_record("u1", None, None);
        r1.1 = 0;
        let mut r2 = assistant_record("u2", Some("u1"), None, 1);
        r2.1 = 1;
        let mut r3 = user_record("u3", Some("u2"), None);
        r3.1 = 2;

        let dag = SessionDag::from_records(vec![r1, r2, r3].into_iter(), DisplayFilter::Standard);
        let tip = dag.tip(TipStrategy::MaxTimestamp);
        // Fallback: max line_index = u3 at index 2
        assert_eq!(tip, Some("u3"));
    }

    #[test]
    fn test_tip_max_timestamp_sidechains_excluded() {
        // u1 -> u2, sidechain sc1 (with later timestamp)
        // Sidechain excluded, tip should be u2
        let records = vec![
            user_record("u1", None, Some("2025-06-01T10:00:00Z")),
            assistant_record("u2", Some("u1"), Some("2025-06-01T10:01:00Z"), 1),
            sidechain_record("sc1", Some("u2")),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.tip(TipStrategy::MaxTimestamp), Some("u2"));
    }

    // --- chain_from tests ---

    #[test]
    fn test_chain_from_linear() {
        // u1 -> u2 -> u3
        let records = vec![
            user_record("u1", None, None),
            assistant_record("u2", Some("u1"), None, 1),
            user_record("u3", Some("u2"), None),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        let chain = dag.chain_from("u3");
        assert_eq!(chain.len(), 3);
        assert!(chain.contains("u1"));
        assert!(chain.contains("u2"));
        assert!(chain.contains("u3"));
    }

    #[test]
    fn test_chain_from_branched_only_one_path() {
        // u1 -> a1 -> a2
        // u1 -> b1 -> b2
        // chain_from("b2") should NOT include a1, a2
        let records = vec![
            user_record("u1", None, None),
            assistant_record("a1", Some("u1"), None, 1),
            user_record("a2", Some("a1"), None),
            user_record("b1", Some("u1"), None),
            assistant_record("b2", Some("b1"), None, 4),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        let chain = dag.chain_from("b2");
        assert_eq!(chain.len(), 3);
        assert!(chain.contains("b2"));
        assert!(chain.contains("b1"));
        assert!(chain.contains("u1"));
        assert!(!chain.contains("a1"));
        assert!(!chain.contains("a2"));
    }

    #[test]
    fn test_chain_from_cycle_guard() {
        // Construct a cycle: u1 -> u2 -> u1 (via direct entry manipulation)
        let records = vec![
            user_record("u1", Some("u2"), None), // parent=u2 (creates cycle)
            assistant_record("u2", Some("u1"), None, 1),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        let chain = dag.chain_from("u2");
        // Should terminate despite cycle, containing both nodes
        assert!(chain.contains("u1"));
        assert!(chain.contains("u2"));
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn test_chain_from_unknown_tip() {
        // Tip UUID not in DAG — should return set with just the tip
        let records = vec![user_record("u1", None, None)];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        let chain = dag.chain_from("nonexistent");
        assert_eq!(chain.len(), 1);
        assert!(chain.contains("nonexistent"));
    }

    // --- DisplayFilter tests ---

    #[test]
    fn test_display_filter_standard_includes_compaction() {
        let records = vec![
            user_record("u1", None, None),
            compaction_record("c1", Some("u1")),
            assistant_record("u2", Some("c1"), None, 2),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.displayable_count(), 3); // u1, c1, u2
    }

    #[test]
    fn test_display_filter_messages_only_excludes_compaction() {
        let records = vec![
            user_record("u1", None, None),
            compaction_record("c1", Some("u1")),
            assistant_record("u2", Some("c1"), None, 2),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::MessagesOnly);
        assert_eq!(dag.displayable_count(), 2); // u1, u2 only
        assert_eq!(dag.len(), 3); // c1 still in entries (has uuid)
    }

    #[test]
    fn test_summary_non_compaction_displayable_under_standard() {
        let records = vec![
            user_record("u1", None, None),
            summary_record("s1", Some("u1")),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.displayable_count(), 2); // u1 + s1 (summaries are displayable)
        assert_eq!(dag.len(), 2);
    }

    #[test]
    fn test_compact_boundary_not_displayable_but_in_entries() {
        let records = vec![
            user_record("u1", None, None),
            compact_boundary_record("cb1", None, Some("u1")),
            user_record("u2", Some("cb1"), None),
        ];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert_eq!(dag.displayable_count(), 2); // u1, u2
        assert_eq!(dag.len(), 3); // cb1 stored for chain walking
    }

    // --- from_file tests with fixture files ---

    #[test]
    fn test_from_file_linear_session() {
        let path = Path::new("tests/fixtures/linear_session.jsonl");
        let dag = SessionDag::from_file(path, DisplayFilter::Standard).unwrap();
        assert_eq!(dag.len(), 4); // u1, u2, u3, u4
        assert_eq!(dag.displayable_count(), 4);
        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("u4"));
        assert_eq!(dag.tip(TipStrategy::MaxTimestamp), Some("u4"));

        let chain = dag.chain_from("u4");
        assert_eq!(chain.len(), 4);
    }

    #[test]
    fn test_from_file_branched_session() {
        let path = Path::new("tests/fixtures/branched_session.jsonl");
        let dag = SessionDag::from_file(path, DisplayFilter::Standard).unwrap();
        // root, a1, a2, a3, b2, b3 = 6 records
        assert_eq!(dag.len(), 6);

        // LastAppended: b3 is last appended terminal
        assert_eq!(dag.tip(TipStrategy::LastAppended), Some("b3"));

        // MaxTimestamp: b3 has latest timestamp (10:05)
        assert_eq!(dag.tip(TipStrategy::MaxTimestamp), Some("b3"));

        // chain from b3 = b3 -> b2 -> a1 -> root
        let chain_b = dag.chain_from("b3");
        assert_eq!(chain_b.len(), 4);
        assert!(chain_b.contains("b3"));
        assert!(chain_b.contains("b2"));
        assert!(chain_b.contains("a1"));
        assert!(chain_b.contains("root"));
        assert!(!chain_b.contains("a2"));
        assert!(!chain_b.contains("a3"));

        // chain from a3 = a3 -> a2 -> a1 -> root
        let chain_a = dag.chain_from("a3");
        assert_eq!(chain_a.len(), 4);
        assert!(chain_a.contains("a3"));
        assert!(chain_a.contains("a2"));
    }

    #[test]
    fn test_from_file_compaction_session() {
        let path = Path::new("tests/fixtures/compaction_session.jsonl");
        let dag = SessionDag::from_file(path, DisplayFilter::Standard).unwrap();
        // c1, c2, c4, c5 are user/assistant; summary has no uuid typically
        // The summary in fixture has no uuid field, so it's skipped
        assert!(dag.len() >= 4);

        let tip = dag.tip(TipStrategy::LastAppended).unwrap();
        assert_eq!(tip, "c5");

        let chain = dag.chain_from("c5");
        assert!(chain.contains("c5"));
        assert!(chain.contains("c4"));
        assert!(chain.contains("c2"));
        assert!(chain.contains("c1"));
    }

    #[test]
    fn test_from_file_nonexistent() {
        let result = SessionDag::from_file(
            Path::new("/nonexistent/path.jsonl"),
            DisplayFilter::Standard,
        );
        assert!(result.is_err());
    }

    // --- Utility method tests ---

    #[test]
    fn test_len_and_is_empty() {
        let empty = SessionDag::from_records(std::iter::empty(), DisplayFilter::Standard);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let records = vec![user_record("u1", None, None)];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        assert!(!dag.is_empty());
        assert_eq!(dag.len(), 1);
    }

    #[test]
    fn test_get_entry() {
        let records = vec![user_record("u1", None, Some("2025-06-01T10:00:00Z"))];
        let dag = SessionDag::from_records(records.into_iter(), DisplayFilter::Standard);
        let entry = dag.get("u1").unwrap();
        assert_eq!(entry.uuid, "u1");
        assert!(entry.timestamp.is_some());
        assert!(entry.is_displayable);
        assert!(dag.get("nonexistent").is_none());
    }
}
