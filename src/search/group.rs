use super::RipgrepMatch;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// A group of matches from the same session
#[derive(Debug, Clone)]
pub struct SessionGroup {
    pub session_id: String,
    pub file_path: String,
    pub matches: Vec<RipgrepMatch>,
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

    // Group by session ID
    let mut group_map: HashMap<String, SessionGroup> = HashMap::new();

    for m in results {
        // Skip matches without messages
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
                },
            );
        }
    }

    // Convert to vec and sort matches within each group (newest first)
    let mut groups: Vec<SessionGroup> = group_map.into_values().collect();

    for group in &mut groups {
        group.matches.sort_by(|a, b| {
            let ta = a.message.as_ref().map(|m| m.timestamp);
            let tb = b.message.as_ref().map(|m| m.timestamp);
            tb.cmp(&ta) // Newest first
        });
    }

    // Sort groups by newest message timestamp
    groups.sort_by(|a, b| {
        let ta = a.latest_timestamp();
        let tb = b.latest_timestamp();
        tb.cmp(&ta) // Newest first
    });

    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Message, SessionSource};
    use chrono::TimeZone;

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
        };

        let first = group.first_match();

        assert!(first.is_none());
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
}
