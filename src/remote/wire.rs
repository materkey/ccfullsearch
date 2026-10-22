//! The `ccs serve` protocol: one JSON object per line. These structs are the
//! contract between two ccs versions on two machines, kept apart from the
//! internal types so an internal change does not silently change the wire.
//! Changing any of them means bumping [`PROTOCOL_VERSION`].

use super::Host;
use crate::recent::RecentSession;
use crate::search::{Message, RipgrepMatch, SessionGroup};
use crate::session::record::MessageRole;
use crate::session::SessionSource;
use crate::tree::{SessionTree, TreeRow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const PROTOCOL_VERSION: u32 = 1;

/// First line `ccs serve` writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireHello {
    pub r#type: String,
    pub protocol: u32,
    pub ccs_version: String,
}

impl WireHello {
    pub fn current() -> Self {
        Self {
            r#type: "hello".to_string(),
            protocol: PROTOCOL_VERSION,
            ccs_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRequest {
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCancelParams {
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCancel {
    pub method: String,
    pub params: WireCancelParams,
}

impl WireCancel {
    pub fn new(id: u64) -> Self {
        Self {
            method: "cancel".to_string(),
            params: WireCancelParams { id },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireResponse {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchParams {
    pub query: String,
    pub regex: bool,
    /// Project relative to the serving machine's home (absolute when outside it).
    pub project: Option<String>,
    pub full_content: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentParams {
    pub limit: usize,
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShowParams {
    pub file_path: String,
    pub line: Option<usize>,
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TreeParams {
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireMatch {
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub text_content: String,
    pub content_truncated: bool,
    pub timestamp: DateTime<Utc>,
    pub branch: Option<String>,
    pub file_path: Option<String>,
    pub line_number: usize,
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub on_latest_chain: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireGroup {
    pub session_id: String,
    pub file_path: String,
    pub project: String,
    pub automation: Option<String>,
    pub message_count: Option<usize>,
    pub message_count_compacted: bool,
    pub matches: Vec<WireMatch>,
    pub matches_total: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSearch {
    pub groups: Vec<WireGroup>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRecent {
    pub session_id: String,
    pub file_path: String,
    pub project: String,
    pub timestamp: DateTime<Utc>,
    pub summary: String,
    pub automation: Option<String>,
    pub branch: Option<String>,
    pub message_count: Option<usize>,
    /// `"user"` or `"assistant"`.
    pub preview_role: String,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireShow {
    pub content: String,
    pub text_content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTreeRow {
    pub uuid: String,
    pub role: String,
    pub timestamp: DateTime<Utc>,
    pub content_preview: String,
    pub graph_symbols: String,
    pub is_on_latest_chain: bool,
    pub is_branch_point: bool,
    pub is_compaction: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTree {
    pub session_id: String,
    pub file_path: String,
    pub tip_uuid: Option<String>,
    pub branch_count: usize,
    pub rows: Vec<WireTreeRow>,
}

impl WireMatch {
    fn into_match(self, group_file_path: &str) -> (RipgrepMatch, Option<String>) {
        let latest = self.on_latest_chain.then(|| self.uuid.clone()).flatten();
        let file_path = self
            .file_path
            .clone()
            .unwrap_or_else(|| group_file_path.to_string());
        let source = SessionSource::from_path(&file_path);
        let message = Message {
            session_id: self.session_id,
            role: self.role,
            content: self.content,
            text_content: self.text_content,
            timestamp: self.timestamp,
            branch: self.branch,
            file_path: self.file_path,
            line_number: self.line_number,
            uuid: self.uuid,
            parent_uuid: self.parent_uuid,
        };
        let m = RipgrepMatch {
            file_path,
            message: Some(message),
            source,
            content_truncated: self.content_truncated,
        };
        (m, latest)
    }
}

impl WireGroup {
    /// The group as the picker holds it, plus the uuids the host marked as
    /// on the latest chain (the fork marker's input).
    pub fn into_group(self, host: Host) -> (SessionGroup, HashSet<String>) {
        let mut chain = HashSet::new();
        let file_path = self.file_path;
        let matches = self
            .matches
            .into_iter()
            .map(|m| {
                let (m, latest) = m.into_match(&file_path);
                chain.extend(latest);
                m
            })
            .collect();
        let group = SessionGroup {
            session_id: self.session_id,
            file_path,
            matches,
            automation: self.automation,
            message_count: self.message_count,
            message_count_compacted: self.message_count_compacted,
            host,
            project_label: Some(self.project),
            matches_total: Some(self.matches_total),
        };
        (group, chain)
    }
}

impl WireRecent {
    pub fn from_recent(session: &RecentSession) -> Self {
        Self {
            session_id: session.session_id.clone(),
            file_path: session.file_path.clone(),
            project: session.project.clone(),
            timestamp: session.timestamp,
            summary: session.summary.clone(),
            automation: session.automation.clone(),
            branch: session.branch.clone(),
            message_count: session.message_count,
            preview_role: match session.preview_role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            }
            .to_string(),
            cwd: session.cwd.clone(),
        }
    }

    pub fn into_recent(self, host: Host) -> RecentSession {
        RecentSession {
            source: SessionSource::from_path(&self.file_path),
            session_id: self.session_id,
            file_path: self.file_path,
            project: self.project,
            timestamp: self.timestamp,
            summary: self.summary,
            automation: self.automation,
            branch: self.branch,
            message_count: self.message_count,
            preview_role: if self.preview_role == "assistant" {
                MessageRole::Assistant
            } else {
                MessageRole::User
            },
            cwd: self.cwd,
            host,
        }
    }
}

impl From<&TreeRow> for WireTreeRow {
    fn from(row: &TreeRow) -> Self {
        Self {
            uuid: row.uuid.clone(),
            role: row.role.clone(),
            timestamp: row.timestamp,
            content_preview: row.content_preview.clone(),
            graph_symbols: row.graph_symbols.clone(),
            is_on_latest_chain: row.is_on_latest_chain,
            is_branch_point: row.is_branch_point,
            is_compaction: row.is_compaction,
        }
    }
}

impl From<WireTreeRow> for TreeRow {
    fn from(row: WireTreeRow) -> Self {
        Self {
            uuid: row.uuid,
            role: row.role,
            timestamp: row.timestamp,
            content_preview: row.content_preview,
            graph_symbols: row.graph_symbols,
            is_on_latest_chain: row.is_on_latest_chain,
            is_branch_point: row.is_branch_point,
            is_compaction: row.is_compaction,
        }
    }
}

impl WireTree {
    pub fn from_tree(tree: &SessionTree) -> Self {
        Self {
            session_id: tree.session_id.clone(),
            file_path: tree.file_path.clone(),
            tip_uuid: tree.tip_uuid.clone(),
            branch_count: tree.branch_count(),
            rows: tree.rows.iter().map(WireTreeRow::from).collect(),
        }
    }

    pub fn into_tree(self, host: Host) -> SessionTree {
        SessionTree::from_rows(
            host,
            self.session_id,
            self.file_path,
            self.tip_uuid,
            self.branch_count,
            self.rows.into_iter().map(TreeRow::from).collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap()
    }

    fn wire_match(uuid: &str, on_latest_chain: bool) -> WireMatch {
        WireMatch {
            session_id: "s1".into(),
            role: "user".into(),
            content: "needle".into(),
            text_content: "needle".into(),
            content_truncated: true,
            timestamp: ts(),
            branch: Some("main".into()),
            file_path: Some("/home/m/.claude/projects/-p/s1.jsonl".into()),
            line_number: 3,
            uuid: Some(uuid.into()),
            parent_uuid: None,
            on_latest_chain,
        }
    }

    fn wire_group() -> WireGroup {
        WireGroup {
            session_id: "s1".into(),
            file_path: "/home/m/.claude/projects/-p/s1.jsonl".into(),
            project: "p".into(),
            automation: None,
            message_count: Some(10),
            message_count_compacted: false,
            matches: vec![wire_match("u1", true), wire_match("u2", false)],
            matches_total: 2,
        }
    }

    fn golden_values() -> serde_json::Value {
        serde_json::json!({
            "hello": WireHello::current(),
            "request": WireRequest { id: 7, method: "search".into(), params: serde_json::to_value(SearchParams { query: "q".into(), regex: false, project: Some("projects/x".into()), full_content: false, limit: Some(5) }).unwrap() },
            "cancel": WireCancel::new(7),
            "response_ok": WireResponse { id: 7, result: Some(serde_json::json!({})), error: None },
            "response_err": WireResponse { id: 8, result: None, error: Some("boom".into()) },
            "recent_params": RecentParams { limit: 100, project: None },
            "show_params": ShowParams { file_path: "/f".into(), line: Some(3), uuid: None },
            "tree_params": TreeParams { file_path: "/f".into() },
            "search": WireSearch { groups: vec![wire_group()], truncated: false },
            "recent": WireRecent { session_id: "s1".into(), file_path: "/f".into(), project: "p".into(), timestamp: ts(), summary: "sum".into(), automation: Some("ralphex".into()), branch: None, message_count: Some(4), preview_role: "user".into(), cwd: Some("/home/m/p".into()) },
            "show": WireShow { content: "full".into(), text_content: "full".into() },
            "tree": WireTree { session_id: "s1".into(), file_path: "/f".into(), tip_uuid: Some("u2".into()), branch_count: 1, rows: vec![WireTreeRow { uuid: "u1".into(), role: "user".into(), timestamp: ts(), content_preview: "hi".into(), graph_symbols: "* ".into(), is_on_latest_chain: true, is_branch_point: false, is_compaction: false }] },
        })
    }

    // Changing tests/fixtures/wire_golden.json means the wire changed:
    // bump PROTOCOL_VERSION in the same commit.
    #[test]
    fn wire_shapes_when_serialized_then_match_golden() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/wire_golden.json"
        );
        if std::env::var_os("CCS_BLESS_GOLDEN").is_some() {
            let mut blessed = golden_values();
            blessed["hello"]["ccs_version"] = serde_json::json!("<version>");
            std::fs::write(path, serde_json::to_string_pretty(&blessed).unwrap() + "\n").unwrap();
        }
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut actual = golden_values();
        actual["hello"]["ccs_version"] = serde_json::json!("<version>");
        assert_eq!(actual, golden);
    }

    #[test]
    fn wire_group_when_missing_field_then_deserialize_fails() {
        let mut value = serde_json::to_value(wire_group()).unwrap();
        value.as_object_mut().unwrap().remove("matches_total");
        assert!(serde_json::from_value::<WireGroup>(value).is_err());
    }

    #[test]
    fn wire_group_into_group_when_flags_set_then_latest_chain_has_flagged_uuids() {
        let (group, chain) = wire_group().into_group(Host::Remote("Z".into()));
        assert_eq!(chain, HashSet::from(["u1".to_string()]));
        assert_eq!(group.host, Host::Remote("Z".into()));
        assert_eq!(group.project_label.as_deref(), Some("p"));
        assert_eq!(group.matches_total, Some(2));
        assert!(group.matches[0].content_truncated);
        assert_eq!(group.matches[0].message.as_ref().unwrap().line_number, 3);
    }

    #[test]
    fn wire_tree_into_tree_when_remote_then_rows_tip_and_branch_count_kept() {
        let tree = golden_tree().into_tree(Host::Remote("Z".into()));
        assert_eq!(tree.rows.len(), 1);
        assert_eq!(tree.tip_uuid.as_deref(), Some("u2"));
        assert_eq!(tree.branch_count(), 1);
        assert_eq!(tree.host, Host::Remote("Z".into()));
    }

    fn golden_tree() -> WireTree {
        serde_json::from_value(golden_values()["tree"].clone()).unwrap()
    }
}
