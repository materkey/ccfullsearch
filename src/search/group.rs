use super::RipgrepMatch;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// A group of matches from the same session
#[derive(Debug, Clone)]
pub struct SessionGroup {
    pub session_id: String,
    pub file_path: String,
    pub matches: Vec<RipgrepMatch>,
    pub automation: Option<String>,
    /// Total user+assistant messages in the session file. None = not yet loaded.
    pub message_count: Option<usize>,
    /// Whether the session was compacted (pre-compaction messages absent from file).
    pub message_count_compacted: bool,
}

impl SessionGroup {
    /// Returns the most recent timestamp in the group
    pub fn latest_timestamp(&self) -> Option<DateTime<Utc>> {
        self.matches
            .iter()
            .filter_map(|m| m.message.as_ref())
            .map(|msg| msg.timestamp)
            .max()
    }

    /// Returns the first match in the group
    pub fn first_match(&self) -> Option<&RipgrepMatch> {
        self.matches.first()
    }
}

/// Group matches by session ID, sorted by newest first
pub fn group_by_session(results: Vec<RipgrepMatch>) -> Vec<SessionGroup> {
    if results.is_empty() {
        return vec![];
    }

    let mut group_map: HashMap<String, SessionGroup> = HashMap::new();

    for m in results {
        let Some(ref msg) = m.message else {
            continue;
        };

        let session_id = msg.session_id.clone();

        if let Some(group) = group_map.get_mut(&session_id) {
            group.matches.push(m);
        } else {
            group_map.insert(
                session_id.clone(),
                SessionGroup {
                    session_id,
                    file_path: m.file_path.clone(),
                    matches: vec![m],
                    automation: None,
                    message_count: None,
                    message_count_compacted: false,
                },
            );
        }
    }

    // Convert to vec and sort matches within each group (newest first). Automation is
    // resolved later from the full session file because partial search hits are not enough
    // to classify session origin reliably.
    let mut groups: Vec<SessionGroup> = group_map.into_values().collect();

    for group in &mut groups {
        group.matches.sort_by(|a, b| {
            let ta = a.message.as_ref().map(|m| m.timestamp);
            let tb = b.message.as_ref().map(|m| m.timestamp);
            tb.cmp(&ta) // Newest first
        });
    }

    groups.sort_by(|a, b| {
        let ta = a.latest_timestamp();
        let tb = b.latest_timestamp();
        tb.cmp(&ta) // Newest first
    });

    groups
}

/// Count user + assistant messages in a JSONL session file.
/// Returns (count, compacted) — compacted=true if summary/compact_boundary records found.
/// For compacted sessions, count reflects only post-compaction messages.
pub fn count_session_messages(file_path: &str) -> (usize, bool) {
    use std::io::{BufRead, BufReader};

    let file = match std::fs::File::open(file_path) {
        Ok(f) => f,
        Err(_) => return (0, false),
    };
    let reader = BufReader::new(file);
    let mut count = 0usize;
    let mut compacted = false;

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match json.get("type").and_then(|v| v.as_str()) {
            Some("user") | Some("assistant") => count += 1,
            Some("summary") | Some("compact_boundary") => compacted = true,
            _ => {}
        }
    }
    (count, compacted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Message;
    use crate::session::SessionSource;
    use chrono::TimeZone;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_match(session_id: &str, timestamp_mins: i64) -> RipgrepMatch {
        RipgrepMatch {
            file_path: format!("/path/to/{}.jsonl", session_id),
            message: Some(Message {
                session_id: session_id.to_string(),
                role: "user".to_string(),
                content: "test content".to_string(),
                timestamp: Utc
                    .with_ymd_and_hms(2025, 1, 9, 10, timestamp_mins as u32, 0)
                    .unwrap(),
                branch: None,
                line_number: 1,
                uuid: None,
                parent_uuid: None,
            }),
            source: SessionSource::ClaudeCodeCLI,
        }
    }

    #[test]
    fn test_group_by_session_empty() {
        let results: Vec<RipgrepMatch> = vec![];
        let groups = group_by_session(results);

        assert!(groups.is_empty(), "Should return empty for empty input");
    }

    #[test]
    fn test_group_by_session_single_session() {
        let results = vec![
            make_match("session-1", 0),
            make_match("session-1", 1),
            make_match("session-1", 2),
        ];

        let groups = group_by_session(results);

        assert_eq!(groups.len(), 1, "Should have 1 group");
        assert_eq!(groups[0].session_id, "session-1");
        assert_eq!(groups[0].matches.len(), 3, "Should have 3 matches");
    }

    #[test]
    fn test_group_by_session_multiple_sessions() {
        let results = vec![
            make_match("session-1", 0),
            make_match("session-2", 1),
            make_match("session-1", 2),
            make_match("session-3", 3),
            make_match("session-2", 4),
        ];

        let groups = group_by_session(results);

        assert_eq!(groups.len(), 3, "Should have 3 groups");

        // Find each session and check match count
        let session_counts: HashMap<_, _> = groups
            .iter()
            .map(|g| (g.session_id.clone(), g.matches.len()))
            .collect();

        assert_eq!(session_counts.get("session-1"), Some(&2));
        assert_eq!(session_counts.get("session-2"), Some(&2));
        assert_eq!(session_counts.get("session-3"), Some(&1));
    }

    #[test]
    fn test_group_by_session_sorted_by_newest() {
        let results = vec![
            make_match("old-session", 0),  // oldest
            make_match("new-session", 59), // newest
            make_match("mid-session", 30), // middle
        ];

        let groups = group_by_session(results);

        assert_eq!(groups.len(), 3);
        assert_eq!(
            groups[0].session_id, "new-session",
            "Newest should be first"
        );
        assert_eq!(
            groups[1].session_id, "mid-session",
            "Middle should be second"
        );
        assert_eq!(groups[2].session_id, "old-session", "Oldest should be last");
    }

    #[test]
    fn test_group_by_session_matches_sorted_within_group() {
        let results = vec![
            make_match("session-1", 0),
            make_match("session-1", 30),
            make_match("session-1", 15),
        ];

        let groups = group_by_session(results);

        assert_eq!(groups.len(), 1);
        let matches = &groups[0].matches;
        assert_eq!(matches.len(), 3);

        // Matches within group should be sorted by timestamp (newest first)
        let t0 = matches[0].message.as_ref().unwrap().timestamp;
        let t1 = matches[1].message.as_ref().unwrap().timestamp;
        let t2 = matches[2].message.as_ref().unwrap().timestamp;

        assert!(t0 >= t1, "First should be newest");
        assert!(t1 >= t2, "Second should be before third");
    }

    #[test]
    fn test_latest_timestamp() {
        let group = SessionGroup {
            session_id: "test".to_string(),
            file_path: "/path/to/test.jsonl".to_string(),
            matches: vec![
                make_match("test", 0),
                make_match("test", 30), // latest
                make_match("test", 15),
            ],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        };

        let latest = group.latest_timestamp();

        assert!(latest.is_some());
        let expected = Utc.with_ymd_and_hms(2025, 1, 9, 10, 30, 0).unwrap();
        assert_eq!(latest.unwrap(), expected);
    }

    #[test]
    fn test_first_match() {
        let group = SessionGroup {
            session_id: "test".to_string(),
            file_path: "/path/to/test.jsonl".to_string(),
            matches: vec![make_match("test", 0), make_match("test", 1)],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        };

        let first = group.first_match();

        assert!(first.is_some());
        assert_eq!(
            first.unwrap().message.as_ref().unwrap().timestamp,
            Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap()
        );
    }

    #[test]
    fn test_first_match_empty() {
        let group = SessionGroup {
            session_id: "test".to_string(),
            file_path: "/path/to/test.jsonl".to_string(),
            matches: vec![],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        };

        let first = group.first_match();

        assert!(first.is_none());
    }

    fn make_match_with_content(
        session_id: &str,
        role: &str,
        content: &str,
        timestamp_mins: i64,
    ) -> RipgrepMatch {
        RipgrepMatch {
            file_path: format!("/path/to/{}.jsonl", session_id),
            message: Some(Message {
                session_id: session_id.to_string(),
                role: role.to_string(),
                content: content.to_string(),
                timestamp: Utc
                    .with_ymd_and_hms(2025, 1, 9, 10, timestamp_mins as u32, 0)
                    .unwrap(),
                branch: None,
                line_number: 1,
                uuid: None,
                parent_uuid: None,
            }),
            source: SessionSource::ClaudeCodeCLI,
        }
    }

    #[test]
    fn test_group_leaves_automation_unset_without_session_scan() {
        let results = vec![
            make_match_with_content(
                "rx-session",
                "user",
                "Do task. Output <<<RALPHEX:ALL_TASKS_DONE>>>",
                0,
            ),
            make_match_with_content("rx-session", "assistant", "Working on it.", 1),
        ];

        let groups = group_by_session(results);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].automation, None);
    }

    #[test]
    fn test_group_manual_session_no_automation() {
        let results = vec![
            make_match_with_content("manual-session", "user", "How do I sort a list?", 0),
            make_match_with_content("manual-session", "assistant", "Use sorted()", 1),
        ];

        let groups = group_by_session(results);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].automation, None);
    }

    #[test]
    fn test_group_marker_in_assistant_not_detected() {
        let results = vec![
            make_match_with_content("chat-session", "user", "Tell me about ralphex", 0),
            make_match_with_content(
                "chat-session",
                "assistant",
                "Ralphex uses <<<RALPHEX:ALL_TASKS_DONE>>> signals",
                1,
            ),
        ];

        let groups = group_by_session(results);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].automation, None);
    }

    #[test]
    fn test_group_does_not_scan_session_file_when_hits_miss_marker() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Bootstrap <<<RALPHEX:ALL_TASKS_DONE>>>"}}]}},"sessionId":"auto-session","timestamp":"2025-01-09T10:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Later answer"}}]}},"sessionId":"auto-session","timestamp":"2025-01-09T10:01:00Z"}}"#
        )
        .unwrap();

        let results = vec![RipgrepMatch {
            file_path: path.to_string_lossy().to_string(),
            message: Some(Message {
                session_id: "auto-session".to_string(),
                role: "assistant".to_string(),
                content: "Later answer".to_string(),
                timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 1, 0).unwrap(),
                branch: None,
                line_number: 2,
                uuid: None,
                parent_uuid: None,
            }),
            source: SessionSource::ClaudeCodeCLI,
        }];

        let groups = group_by_session(results);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].automation, None);
    }

    #[test]
    fn test_group_does_not_scan_parent_session_when_hit_is_auxiliary_file() {
        let dir = TempDir::new().unwrap();
        let parent_path = dir.path().join("auto-session.jsonl");
        let aux_path = dir.path().join("agent-abc123.jsonl");

        fs::write(
            &parent_path,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Bootstrap <<<RALPHEX:ALL_TASKS_DONE>>>"}]},"sessionId":"auto-session","timestamp":"2025-01-09T10:00:00Z"}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Parent answer"}]},"sessionId":"auto-session","timestamp":"2025-01-09T10:01:00Z"}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            &aux_path,
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Auxiliary answer"}]},"sessionId":"auto-session","timestamp":"2025-01-09T10:02:00Z"}"#,
                "\n"
            ),
        )
        .unwrap();

        let results = vec![RipgrepMatch {
            file_path: aux_path.to_string_lossy().to_string(),
            message: Some(Message {
                session_id: "auto-session".to_string(),
                role: "assistant".to_string(),
                content: "Auxiliary answer".to_string(),
                timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 2, 0).unwrap(),
                branch: None,
                line_number: 1,
                uuid: None,
                parent_uuid: None,
            }),
            source: SessionSource::ClaudeCodeCLI,
        }];

        let groups = group_by_session(results);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].automation, None);
    }

    #[test]
    fn test_group_skips_none_messages() {
        let results = vec![
            RipgrepMatch {
                file_path: "/path/to/session.jsonl".to_string(),
                message: None, // Should be skipped
                source: SessionSource::ClaudeCodeCLI,
            },
            make_match("session-1", 0),
        ];

        let groups = group_by_session(results);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].matches.len(), 1, "Should skip None messages");
    }

    #[test]
    fn test_count_session_messages_normal() {
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"q1"}},"sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"a1"}},"sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"q2"}},"sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();

        let (count, compacted) = count_session_messages(f.path().to_str().unwrap());
        assert_eq!(count, 3);
        assert!(!compacted);
    }

    #[test]
    fn test_count_session_messages_compacted() {
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"q1"}},"sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"a1"}},"sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"conversation summary","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"q2"}},"sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"a2"}},"sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"progress","uuid":"p1","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();

        let (count, compacted) = count_session_messages(f.path().to_str().unwrap());
        assert_eq!(count, 4);
        assert!(compacted);
    }

    #[test]
    fn test_count_session_messages_nonexistent_file() {
        let (count, compacted) = count_session_messages("/nonexistent/file.jsonl");
        assert_eq!(count, 0);
        assert!(!compacted);
    }
}
