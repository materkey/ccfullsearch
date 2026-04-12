use crate::dag::{DisplayFilter, SessionDag, TipStrategy};
use crate::session;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::session::is_sidechain;

/// Check if a message uuid is on the latest parentUuid chain in a JSONL file.
///
/// Returns `true` if:
/// - The uuid is on the latest chain, OR
/// - The uuid doesn't exist in the file at all (e.g. it came from a subagent file
///   that was resolved to this parent file — forking would be wrong since the uuid
///   has no meaning in this file's DAG)
/// - The chain can't be determined
pub fn is_on_latest_chain(file_path: &str, target_uuid: &str) -> bool {
    let dag = match SessionDag::from_file(Path::new(file_path), DisplayFilter::Standard) {
        Ok(d) => d,
        Err(_) => return true,
    };
    // If the uuid doesn't exist in this file at all, don't treat it as "off-chain"
    if dag.get(target_uuid).is_none() {
        return true;
    }
    let tip = match dag.tip(TipStrategy::LastAppended) {
        Some(t) => t,
        None => return true,
    };
    dag.chain_from(tip).contains(target_uuid)
}

/// Build the set of uuids on the latest chain (from the terminal message backwards).
pub fn build_chain_from_tip(file_path: &str) -> Option<HashSet<String>> {
    let dag = SessionDag::from_file(Path::new(file_path), DisplayFilter::Standard).ok()?;
    let tip = dag.tip(TipStrategy::LastAppended)?;
    Some(dag.chain_from(tip))
}

/// Return the UUID of the terminal displayable record in the file.
pub fn latest_tip_uuid(file_path: &str) -> Option<String> {
    let dag = SessionDag::from_file(Path::new(file_path), DisplayFilter::Standard).ok()?;
    dag.tip(TipStrategy::LastAppended).map(|s| s.to_string())
}

/// Create a forked JSONL file containing only messages from the branch
/// that includes the target uuid. Returns (new_session_id, new_file_path).
pub fn create_fork(file_path: &str, target_uuid: &str) -> Result<(String, String), String> {
    // Build DAG and get the branch chain
    let dag = SessionDag::from_file(Path::new(file_path), DisplayFilter::Standard)
        .map_err(|e| format!("Failed to build DAG: {}", e))?;
    let branch_uuids = dag.chain_from(target_uuid);

    // Read file and filter lines for the fork
    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {}", file_path, e))?;

    let new_session_id = uuid::Uuid::new_v4().to_string();
    let mut forked_lines = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut json: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if is_sidechain(&json) {
            continue;
        }
        if let Some(uuid) = session::extract_uuid(&json) {
            if branch_uuids.contains(uuid.as_str()) {
                replace_session_id_in_value(&mut json, &new_session_id);
                if let Ok(s) = serde_json::to_string(&json) {
                    forked_lines.push(s);
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

/// Replace sessionId/session_id in an already-parsed JSON value.
fn replace_session_id_in_value(json: &mut serde_json::Value, new_id: &str) {
    if json.get("sessionId").is_some() {
        json["sessionId"] = serde_json::Value::String(new_id.to_string());
    }
    if json.get("session_id").is_some() {
        json["session_id"] = serde_json::Value::String(new_id.to_string());
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

    #[test]
    fn test_build_chain_ignores_sidechain_records() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Normal chain: u1 -> u2 -> u3
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","uuid":"u3","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        // Sidechain record at end — should NOT become tip
        writeln!(f, r#"{{"type":"assistant","uuid":"sc1","parentUuid":"u2","isSidechain":true,"sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();

        let chain = build_chain_from_tip(path.to_str().unwrap()).unwrap();
        let tip = latest_tip_uuid(path.to_str().unwrap()).unwrap();

        assert_eq!(tip, "u3", "Tip should be u3, not sidechain sc1");
        assert!(chain.contains("u3"));
        assert!(chain.contains("u2"));
        assert!(chain.contains("u1"));
        assert!(!chain.contains("sc1"), "Sidechain should not be in chain");
    }

    #[test]
    fn test_create_fork_ignores_sidechain_records() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Chain: u1 -> u2 -> u3
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","uuid":"u3","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        // Sidechain branching from u2
        writeln!(f, r#"{{"type":"assistant","uuid":"sc1","parentUuid":"u2","isSidechain":true,"sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();

        let (_, new_path) = create_fork(path.to_str().unwrap(), "u3").unwrap();
        let content = fs::read_to_string(&new_path).unwrap();

        assert!(content.contains("\"uuid\":\"u1\""));
        assert!(content.contains("\"uuid\":\"u2\""));
        assert!(content.contains("\"uuid\":\"u3\""));
        assert!(
            !content.contains("\"uuid\":\"sc1\""),
            "Sidechain record should not be in fork"
        );
    }

    #[test]
    fn test_build_chain_resets_on_compact_boundary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Pre-boundary chain: u1 -> u2
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        // Compact boundary — should reset all state
        writeln!(f, r#"{{"type":"system","subtype":"compact_boundary","uuid":"cb1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        // Post-boundary chain: u3 -> u4
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u4","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();

        let chain = build_chain_from_tip(path.to_str().unwrap()).unwrap();
        let tip = latest_tip_uuid(path.to_str().unwrap()).unwrap();

        assert_eq!(tip, "u4", "Tip should be post-boundary u4");
        assert!(chain.contains("u4"));
        assert!(chain.contains("u3"));
        assert!(
            !chain.contains("u2"),
            "Pre-boundary u2 should not be in chain"
        );
        assert!(
            !chain.contains("u1"),
            "Pre-boundary u1 should not be in chain"
        );
    }

    #[test]
    fn test_create_fork_handles_compact_boundary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Pre-boundary chain
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        // Compact boundary
        writeln!(f, r#"{{"type":"system","subtype":"compact_boundary","uuid":"cb1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        // Post-boundary chain: u3 -> u4 -> u5 (branch A), u3 -> u6 (branch B)
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u4","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","uuid":"u5","parentUuid":"u4","sessionId":"s1","timestamp":"2025-01-01T00:06:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","uuid":"u6","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:07:00Z"}}"#).unwrap();

        // Fork from u5 (branch A) — should only include post-boundary records on that branch
        let (_, new_path) = create_fork(path.to_str().unwrap(), "u5").unwrap();
        let content = fs::read_to_string(&new_path).unwrap();

        assert!(
            !content.contains("\"uuid\":\"u1\""),
            "Pre-boundary u1 should not be in fork"
        );
        assert!(
            !content.contains("\"uuid\":\"u2\""),
            "Pre-boundary u2 should not be in fork"
        );
        assert!(
            !content.contains("compact_boundary"),
            "Boundary itself should not be in fork"
        );
        assert!(
            content.contains("\"uuid\":\"u3\""),
            "Post-boundary root u3 should be in fork"
        );
        assert!(
            content.contains("\"uuid\":\"u4\""),
            "Branch A u4 should be in fork"
        );
        assert!(
            content.contains("\"uuid\":\"u5\""),
            "Branch A u5 should be in fork"
        );
        assert!(
            !content.contains("\"uuid\":\"u6\""),
            "Branch B u6 should not be in fork"
        );
    }

    #[test]
    fn test_create_fork_skips_metadata_without_uuid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Normal chain: u1 -> u2
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        // Metadata lines without uuid — should be skipped in fork
        writeln!(
            f,
            r#"{{"type":"summary","sessionId":"s1","summary":"A conversation about testing"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","sessionId":"s1","title":"Test Session"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"tag","sessionId":"s1","tag":"important"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"file-history-snapshot","sessionId":"s1","files":[]}}"#
        )
        .unwrap();

        let (_, new_path) = create_fork(path.to_str().unwrap(), "u2").unwrap();
        let content = fs::read_to_string(&new_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();

        // Should contain the chain messages
        assert!(content.contains("\"uuid\":\"u1\""));
        assert!(content.contains("\"uuid\":\"u2\""));

        // Should NOT contain any metadata lines
        assert!(
            !content.contains("\"type\":\"summary\""),
            "Summary metadata should be skipped"
        );
        assert!(
            !content.contains("\"type\":\"custom-title\""),
            "Custom-title metadata should be skipped"
        );
        assert!(
            !content.contains("\"type\":\"tag\""),
            "Tag metadata should be skipped"
        );
        assert!(
            !content.contains("\"type\":\"file-history-snapshot\""),
            "File-history-snapshot metadata should be skipped"
        );

        // Only 2 lines should remain (u1 and u2)
        assert_eq!(
            lines.len(),
            2,
            "Fork should only contain uuid-bearing chain messages"
        );
    }

    #[test]
    fn test_build_chain_bridges_compact_boundary_via_logical_parent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Pre-boundary chain: u1 -> u2
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        // Compact boundary with logicalParentUuid bridging to u2
        writeln!(f, r#"{{"type":"system","subtype":"compact_boundary","uuid":"cb1","logicalParentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        // Post-boundary chain referencing cb1
        writeln!(f, r#"{{"type":"user","uuid":"u3","parentUuid":"cb1","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u4","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();

        let chain = build_chain_from_tip(path.to_str().unwrap()).unwrap();
        let tip = latest_tip_uuid(path.to_str().unwrap()).unwrap();

        assert_eq!(tip, "u4", "Tip should be post-boundary u4");
        assert!(chain.contains("u4"));
        assert!(chain.contains("u3"));
        assert!(
            chain.contains("cb1"),
            "Boundary cb1 should be in chain (bridge)"
        );
        assert!(
            chain.contains("u2"),
            "Pre-boundary u2 should be reachable via logicalParentUuid bridge"
        );
        assert!(
            chain.contains("u1"),
            "Pre-boundary u1 should be reachable via bridge"
        );
    }

    #[test]
    fn test_create_fork_bridges_compact_boundary_via_logical_parent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Pre-boundary chain: u1 -> u2
        writeln!(
            f,
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        // Compact boundary with logicalParentUuid bridging to u2
        writeln!(f, r#"{{"type":"system","subtype":"compact_boundary","uuid":"cb1","logicalParentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        // Post-boundary: u3 -> u4 (latest chain), u3 -> u5 (off-latest branch)
        writeln!(f, r#"{{"type":"user","uuid":"u3","parentUuid":"cb1","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","uuid":"u4","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","uuid":"u5","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:06:00Z"}}"#).unwrap();

        // Fork from u4 (branch that spans compact_boundary via logicalParentUuid)
        let (_, new_path) = create_fork(path.to_str().unwrap(), "u4").unwrap();
        let content = fs::read_to_string(&new_path).unwrap();

        // Should include the full chain: u4 -> u3 -> cb1 -> u2 -> u1
        assert!(
            content.contains("\"uuid\":\"u1\""),
            "Pre-boundary u1 should be in fork (reachable via logicalParentUuid bridge)"
        );
        assert!(
            content.contains("\"uuid\":\"u2\""),
            "Pre-boundary u2 should be in fork (reachable via logicalParentUuid bridge)"
        );
        assert!(
            content.contains("\"uuid\":\"cb1\""),
            "Boundary cb1 should be in fork (part of chain)"
        );
        assert!(
            content.contains("\"uuid\":\"u3\""),
            "Post-boundary u3 should be in fork"
        );
        assert!(
            content.contains("\"uuid\":\"u4\""),
            "Target u4 should be in fork"
        );
        assert!(
            !content.contains("\"uuid\":\"u5\""),
            "Off-branch u5 should NOT be in fork"
        );
    }

    #[test]
    fn test_latest_chain_and_tip_ignore_invalid_jsonl_lines() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":"truncated""#).unwrap();

        let chain = build_chain_from_tip(path.to_str().unwrap()).unwrap();

        assert_eq!(
            latest_tip_uuid(path.to_str().unwrap()),
            Some("b5".to_string())
        );
        assert!(chain.contains("b5"));
        assert!(chain.contains("b4"));
        assert!(chain.contains("b3"));
        assert!(chain.contains("a2"));
        assert!(!chain.contains("a5"));
    }

    #[test]
    fn test_is_on_latest_chain_unknown_uuid_returns_true() {
        // UUID from a subagent file doesn't exist in the parent session file.
        // is_on_latest_chain should return true (don't fork) rather than false.
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);

        // "nonexistent-uuid" is not in the file at all
        assert!(
            is_on_latest_chain(path.to_str().unwrap(), "nonexistent-uuid"),
            "Unknown UUID should be treated as on-chain to avoid spurious forks"
        );
    }

    #[test]
    fn test_is_on_latest_chain_off_branch_uuid_returns_false() {
        // UUID that IS in the file but on a non-latest branch should return false.
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);

        // a5 is on branch A (not latest — branch B with b5 is latest)
        assert!(
            !is_on_latest_chain(path.to_str().unwrap(), "a5"),
            "UUID on non-latest branch should return false"
        );
    }
}
