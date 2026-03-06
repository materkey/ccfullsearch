use crate::session;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Check if a message uuid is on the latest parentUuid chain in a JSONL file.
/// The "latest chain" is built by walking parentUuid backwards from the last line with a uuid.
pub fn is_on_latest_chain(file_path: &str, target_uuid: &str) -> bool {
    let chain = match build_chain_from_tip(file_path) {
        Some(c) => c,
        None => return true, // Can't determine chain, assume it's on latest
    };
    chain.contains(target_uuid)
}

/// Build the set of uuids on the latest chain (from the last message backwards).
pub fn build_chain_from_tip(file_path: &str) -> Option<HashSet<String>> {
    let file = fs::File::open(file_path).ok()?;
    let reader = BufReader::new(file);

    let mut uuid_to_parent: HashMap<String, Option<String>> = HashMap::new();
    let mut last_uuid: Option<String> = None;

    for line in reader.lines() {
        let line = line.ok()?;
        let json: serde_json::Value = serde_json::from_str(line.trim()).ok()?;

        if let Some(uuid) = session::extract_uuid(&json) {
            let parent = session::extract_parent_uuid(&json);
            uuid_to_parent.insert(uuid.clone(), parent);
            last_uuid = Some(uuid);
        }
    }

    let tip = last_uuid?;
    let mut chain = HashSet::new();
    let mut current = Some(tip);

    while let Some(uuid) = current {
        chain.insert(uuid.clone());
        current = uuid_to_parent.get(&uuid).and_then(|p| p.clone());
    }

    Some(chain)
}

/// Create a forked JSONL file containing only messages from the branch
/// that includes the target uuid. Returns (new_session_id, new_file_path).
pub fn create_fork(file_path: &str, target_uuid: &str) -> Result<(String, String), String> {
    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {}", file_path, e))?;

    // Build uuid→parent map and find the target's chain
    let mut uuid_to_parent: HashMap<String, Option<String>> = HashMap::new();

    for line in content.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(uuid) = session::extract_uuid(&json) {
                let parent = session::extract_parent_uuid(&json);
                uuid_to_parent.insert(uuid, parent);
            }
        }
    }

    // Walk from target_uuid backwards to build the branch chain
    let mut branch_uuids = HashSet::new();
    let mut current = Some(target_uuid.to_string());
    while let Some(uuid) = current {
        branch_uuids.insert(uuid.clone());
        current = uuid_to_parent.get(&uuid).and_then(|p| p.clone());
    }

    // Generate new session ID
    let new_session_id = uuid::Uuid::new_v4().to_string();

    // Filter lines: include lines whose uuid is in the branch, or lines without uuid (metadata)
    let mut forked_lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let line_uuid = json.get("uuid").and_then(|v| v.as_str());

            match line_uuid {
                Some(uuid) if branch_uuids.contains(uuid) => {
                    // Replace sessionId/session_id with new session ID
                    let updated = replace_session_id(trimmed, &new_session_id);
                    forked_lines.push(updated);
                }
                Some(_) => {
                    // UUID not in branch — skip
                }
                None => {
                    // No UUID (file-history-snapshot, etc.) — include as-is
                    forked_lines.push(trimmed.to_string());
                }
            }
        }
    }

    // Write forked file in the same directory
    let parent_dir = Path::new(file_path)
        .parent()
        .ok_or_else(|| "Cannot get parent directory".to_string())?;
    let new_file_path = parent_dir.join(format!("{}.jsonl", new_session_id));
    let new_file_path_str = new_file_path.to_string_lossy().to_string();

    let output = forked_lines.join("\n") + "\n";
    fs::write(&new_file_path, output)
        .map_err(|e| format!("Failed to write fork {}: {}", new_file_path_str, e))?;

    Ok((new_session_id, new_file_path_str))
}

/// Replace sessionId or session_id value in a JSON line
fn replace_session_id(line: &str, new_id: &str) -> String {
    if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(line) {
        if json.get("sessionId").is_some() {
            json["sessionId"] = serde_json::Value::String(new_id.to_string());
        }
        if json.get("session_id").is_some() {
            json["session_id"] = serde_json::Value::String(new_id.to_string());
        }
        serde_json::to_string(&json).unwrap_or_else(|_| line.to_string())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Helper to create a JSONL file simulating two branches diverging from a common ancestor.
    fn create_branched_session(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"progress","uuid":"p1","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"a1","parentUuid":"p1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi"}}]}},"uuid":"a2","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"a3","parentUuid":"a2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Branch A msg"}}]}},"uuid":"a4","parentUuid":"a3","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Branch A reply"}}]}},"uuid":"a5","parentUuid":"a4","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"b3","parentUuid":"a2","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Branch B msg"}}]}},"uuid":"b4","parentUuid":"b3","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Branch B reply"}}]}},"uuid":"b5","parentUuid":"b4","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();
        path
    }

    #[test]
    fn test_is_on_latest_chain_tip_message() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        assert!(is_on_latest_chain(path.to_str().unwrap(), "b5"));
    }

    #[test]
    fn test_is_on_latest_chain_common_ancestor() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        assert!(is_on_latest_chain(path.to_str().unwrap(), "a2"));
    }

    #[test]
    fn test_is_not_on_latest_chain() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        assert!(!is_on_latest_chain(path.to_str().unwrap(), "a5"));
        assert!(!is_on_latest_chain(path.to_str().unwrap(), "a4"));
        assert!(!is_on_latest_chain(path.to_str().unwrap(), "a3"));
    }

    #[test]
    fn test_create_fork_from_non_latest_branch() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);

        let (new_id, new_path) = create_fork(path.to_str().unwrap(), "a5").unwrap();

        assert!(Path::new(&new_path).exists());
        assert_ne!(new_id, "s1");

        let content = fs::read_to_string(&new_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();

        let has_uuid = |uuid: &str| {
            lines
                .iter()
                .any(|l| l.contains(&format!("\"uuid\":\"{}\"", uuid)))
        };
        assert!(has_uuid("p1"), "Should contain common ancestor p1");
        assert!(has_uuid("a1"), "Should contain a1");
        assert!(has_uuid("a2"), "Should contain a2");
        assert!(has_uuid("a3"), "Should contain a3");
        assert!(has_uuid("a4"), "Should contain a4");
        assert!(has_uuid("a5"), "Should contain a5");
        assert!(!has_uuid("b3"), "Should NOT contain b3");
        assert!(!has_uuid("b4"), "Should NOT contain b4");
        assert!(!has_uuid("b5"), "Should NOT contain b5");

        assert!(content.contains(&new_id), "Should contain new session ID");
        let has_old_session = lines
            .iter()
            .filter(|l| l.contains("\"uuid\""))
            .any(|l| l.contains("\"sessionId\":\"s1\""));
        assert!(
            !has_old_session,
            "Should NOT contain old sessionId in forked messages"
        );
    }

    #[test]
    fn test_create_fork_preserves_line_order() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);

        let (_, new_path) = create_fork(path.to_str().unwrap(), "a5").unwrap();
        let content = fs::read_to_string(&new_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();

        let pos = |uuid: &str| {
            lines
                .iter()
                .position(|l| l.contains(&format!("\"uuid\":\"{}\"", uuid)))
                .unwrap()
        };
        assert!(pos("p1") < pos("a1"));
        assert!(pos("a1") < pos("a2"));
        assert!(pos("a2") < pos("a3"));
        assert!(pos("a3") < pos("a4"));
        assert!(pos("a4") < pos("a5"));
    }
}
