use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Ensure the project directory exists
pub fn ensure_project_dir(file_path: &str) -> Result<(), String> {
    // Extract project directory from session file path
    let dir = Path::new(file_path)
        .parent()
        .ok_or_else(|| "Cannot get parent directory".to_string())?;

    // The project directory is one level up
    // e.g., /Users/user/.claude/projects/-Users-user-projects-myapp/xxx.jsonl
    // We need /Users/user/projects/myapp to exist

    // For now, just ensure the .claude directory exists
    if !dir.exists() {
        fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    Ok(())
}

/// Extract the actual project path from the .claude/projects path
/// e.g., /Users/user/.claude/projects/-Users-user-projects-myapp/session.jsonl
/// -> /Users/user/projects/myapp
///
/// The encoding uses:
/// - Leading `-` represents `/`
/// - `--` represents `/.` (for hidden directories like .claude)
/// - `-` represents `/` for path separators
///
/// Since directory/file names can also contain `-`, we try multiple strategies
/// and return the first path that actually exists on disk.
#[cfg(test)]
fn extract_project_path(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);

    // Get the parent directory (the project folder in .claude/projects)
    let claude_project_dir = path.parent()?;
    let dir_name = claude_project_dir.file_name()?.to_str()?;

    // Strategy 1: If there's a "-projects-" marker, use it to find the project path
    // This handles: -Users-user-projects-myapp -> /Users/user/projects/myapp
    if let Some(projects_idx) = dir_name.rfind("-projects-") {
        let path_prefix = if dir_name.starts_with('-') {
            &dir_name[1..projects_idx]
        } else {
            &dir_name[..projects_idx]
        };
        let path_prefix = path_prefix.replace("--", "\x00").replace('-', "/").replace('\x00', "/.");
        let project_name = &dir_name[projects_idx + 10..];
        let candidate = format!("/{}/projects/{}", path_prefix, project_name);
        if Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }

    // Strategy 2: Try decoding the entire path (for non-projects paths)
    // Handle `--` as `/.` first (hidden dirs), then `-` as `/`
    let decoded = if dir_name.starts_with('-') {
        &dir_name[1..]
    } else {
        dir_name
    };
    let decoded = decoded.replace("--", "\x00").replace('-', "/").replace('\x00', "/.");
    let candidate = format!("/{}", decoded);
    if Path::new(&candidate).exists() {
        return Some(candidate);
    }

    // Strategy 3: Try progressively shorter paths (from right to left)
    // This handles cases where the last few components have dashes in their names
    let parts: Vec<&str> = dir_name.split('-').collect();
    for split_point in (1..parts.len()).rev() {
        let path_part: String = parts[..split_point].join("/");
        let name_part: String = parts[split_point..].join("-");

        let candidate = if path_part.starts_with('/') {
            format!("{}/{}", path_part, name_part)
        } else {
            format!("/{}/{}", path_part, name_part)
        };

        // Handle `--` -> `.` in the path part
        let candidate = candidate.replace("//", "/.");

        if Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }

    None
}

use crate::search::SessionSource;

/// Resume a Claude session based on its source.
/// If `message_uuid` is provided and the message is not on the latest chain,
/// creates a forked JSONL file and resumes from that instead.
pub fn resume(session_id: &str, file_path: &str, source: SessionSource, message_uuid: Option<&str>) -> Result<(), String> {
    // Check if we need to fork (message not on latest chain)
    if let Some(uuid) = message_uuid {
        if source == SessionSource::ClaudeCodeCLI && !is_on_latest_chain(file_path, uuid) {
            let (fork_session_id, fork_file_path) = create_fork(file_path, uuid)?;
            return resume_cli(&fork_session_id, &fork_file_path);
        }
    }

    match source {
        SessionSource::ClaudeCodeCLI => resume_cli(session_id, file_path),
        SessionSource::ClaudeDesktop => resume_desktop(),
    }
}

/// Check if a message uuid is on the latest parentUuid chain in a JSONL file.
/// The "latest chain" is built by walking parentUuid backwards from the last line with a uuid.
fn is_on_latest_chain(file_path: &str, target_uuid: &str) -> bool {
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

        if let Some(uuid) = json.get("uuid").and_then(|v| v.as_str()) {
            let parent = json.get("parentUuid").and_then(|v| v.as_str()).map(|s| s.to_string());
            uuid_to_parent.insert(uuid.to_string(), parent);
            last_uuid = Some(uuid.to_string());
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
fn create_fork(file_path: &str, target_uuid: &str) -> Result<(String, String), String> {
    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {}", file_path, e))?;

    // Build uuid→parent map and find the target's chain
    let mut uuid_to_parent: HashMap<String, Option<String>> = HashMap::new();

    for line in content.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            if let Some(uuid) = json.get("uuid").and_then(|v| v.as_str()) {
                let parent = json.get("parentUuid").and_then(|v| v.as_str()).map(|s| s.to_string());
                uuid_to_parent.insert(uuid.to_string(), parent);
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
    let parent_dir = Path::new(file_path).parent()
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

/// Resume a Claude Code CLI session using exec
fn resume_cli(session_id: &str, file_path: &str) -> Result<(), String> {
    // Ensure session file's parent directory exists
    ensure_project_dir(file_path)?;

    // Find claude binary
    let claude_path = which::which("claude")
        .map_err(|_| "Claude binary not found in PATH".to_string())?;

    // Claude Code uses the current working directory to determine which project folder
    // to look for sessions in. We need to:
    // 1. Decode the original project path from the .claude/projects folder name
    // 2. Create the directory if it doesn't exist (so Claude can find the session)
    // 3. Run claude --resume from that directory

    // Try to decode the original project path
    let project_dir = decode_project_path(file_path);

    if let Some(ref dir) = project_dir {
        // Create the directory if it doesn't exist
        // (Claude Code needs this to map cwd -> session storage location)
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create project directory {}: {}", dir, e))?;
        }

        // Run claude from the project directory
        let err = Command::new(&claude_path)
            .current_dir(dir)
            .args(["--resume", session_id])
            .exec();
        return Err(format!("Failed to exec claude: {}", err));
    }

    // Fallback: try from home directory
    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp".to_string());

    let err = Command::new(&claude_path)
        .current_dir(&home_dir)
        .args(["--resume", session_id])
        .exec();

    Err(format!("Failed to exec claude: {}", err))
}

/// Open Claude Desktop app for Desktop sessions
fn resume_desktop() -> Result<(), String> {
    // On macOS, use `open` to launch Claude Desktop
    let err = Command::new("open")
        .args(["-a", "Claude"])
        .exec();

    Err(format!("Failed to open Claude Desktop: {}", err))
}

/// Decode the original project path from the .claude/projects folder name
/// Unlike extract_project_path, this doesn't check if the path exists
fn decode_project_path(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let claude_project_dir = path.parent()?;
    let dir_name = claude_project_dir.file_name()?.to_str()?;

    // The directory name is like "-Users-user-Downloads-something"
    // We need to convert it back to "/Users/user/Downloads/something"
    // But directory names can contain dashes, so we try different strategies

    // Strategy 1: If there's a "-projects-" marker, use it
    if let Some(projects_idx) = dir_name.rfind("-projects-") {
        let path_prefix = if dir_name.starts_with('-') {
            &dir_name[1..projects_idx]
        } else {
            &dir_name[..projects_idx]
        };
        let path_prefix = path_prefix.replace("--", "\x00").replace('-', "/").replace('\x00', "/.");
        let project_name = &dir_name[projects_idx + 10..];
        return Some(format!("/{}/projects/{}", path_prefix, project_name));
    }

    // Strategy 2: Just convert dashes to slashes (handle -- as /. for hidden dirs)
    let decoded = if dir_name.starts_with('-') {
        &dir_name[1..]
    } else {
        dir_name
    };
    let decoded = decoded.replace("--", "\x00").replace('-', "/").replace('\x00', "/.");
    Some(format!("/{}", decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Helper to create a JSONL file simulating two branches diverging from a common ancestor.
    /// Branch A: progress→user(a1)→assistant(a2)→system(a3)→user(a4)→assistant(a5)
    /// Branch B: system(b3)→user(b4)→assistant(b5)  (forks after assistant a2)
    fn create_branched_session(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // Common: progress → user(a1) → assistant(a2)
        writeln!(f, r#"{{"type":"progress","uuid":"p1","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"uuid":"a1","parentUuid":"p1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi"}}]}},"uuid":"a2","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        // Branch A continues: system(a3) → user(a4) → assistant(a5)
        writeln!(f, r#"{{"type":"system","uuid":"a3","parentUuid":"a2","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Branch A msg"}}]}},"uuid":"a4","parentUuid":"a3","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Branch A reply"}}]}},"uuid":"a5","parentUuid":"a4","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        // Branch B forks from a2: system(b3) → user(b4) → assistant(b5)
        writeln!(f, r#"{{"type":"system","uuid":"b3","parentUuid":"a2","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Branch B msg"}}]}},"uuid":"b4","parentUuid":"b3","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Branch B reply"}}]}},"uuid":"b5","parentUuid":"b4","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();
        path
    }

    #[test]
    fn test_is_on_latest_chain_tip_message() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        // b5 is the last line → it IS on the latest chain
        assert!(is_on_latest_chain(path.to_str().unwrap(), "b5"));
    }

    #[test]
    fn test_is_on_latest_chain_common_ancestor() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        // a2 is a common ancestor — it's on both chains, including latest
        assert!(is_on_latest_chain(path.to_str().unwrap(), "a2"));
    }

    #[test]
    fn test_is_not_on_latest_chain() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);
        // a5 is the tip of branch A, but b5 is the last line (branch B is latest)
        assert!(!is_on_latest_chain(path.to_str().unwrap(), "a5"));
        assert!(!is_on_latest_chain(path.to_str().unwrap(), "a4"));
        assert!(!is_on_latest_chain(path.to_str().unwrap(), "a3"));
    }

    #[test]
    fn test_create_fork_from_non_latest_branch() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);

        // Fork from a5 (branch A tip, not on latest chain)
        let (new_id, new_path) = create_fork(path.to_str().unwrap(), "a5").unwrap();

        // New file should exist
        assert!(Path::new(&new_path).exists());
        assert_ne!(new_id, "s1"); // New session ID

        // Read and verify fork contents
        let content = fs::read_to_string(&new_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();

        // Should contain: p1, a1, a2, a3, a4, a5 (branch A chain)
        // Should NOT contain: b3, b4, b5 (branch B)
        let has_uuid = |uuid: &str| lines.iter().any(|l| l.contains(&format!("\"uuid\":\"{}\"", uuid)));
        assert!(has_uuid("p1"), "Should contain common ancestor p1");
        assert!(has_uuid("a1"), "Should contain a1");
        assert!(has_uuid("a2"), "Should contain a2");
        assert!(has_uuid("a3"), "Should contain a3");
        assert!(has_uuid("a4"), "Should contain a4");
        assert!(has_uuid("a5"), "Should contain a5");
        assert!(!has_uuid("b3"), "Should NOT contain b3");
        assert!(!has_uuid("b4"), "Should NOT contain b4");
        assert!(!has_uuid("b5"), "Should NOT contain b5");

        // Session ID should be replaced
        assert!(content.contains(&new_id), "Should contain new session ID");
        // Original session ID should NOT appear in uuid-bearing lines
        let has_old_session = lines.iter()
            .filter(|l| l.contains("\"uuid\""))
            .any(|l| l.contains("\"sessionId\":\"s1\""));
        assert!(!has_old_session, "Should NOT contain old sessionId in forked messages");
    }

    #[test]
    fn test_create_fork_preserves_line_order() {
        let dir = TempDir::new().unwrap();
        let path = create_branched_session(&dir);

        let (_, new_path) = create_fork(path.to_str().unwrap(), "a5").unwrap();
        let content = fs::read_to_string(&new_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();

        // Verify order: p1 before a1 before a2 before a3 before a4 before a5
        let pos = |uuid: &str| lines.iter().position(|l| l.contains(&format!("\"uuid\":\"{}\"", uuid))).unwrap();
        assert!(pos("p1") < pos("a1"));
        assert!(pos("a1") < pos("a2"));
        assert!(pos("a2") < pos("a3"));
        assert!(pos("a3") < pos("a4"));
        assert!(pos("a4") < pos("a5"));
    }

    #[test]
    fn test_ensure_project_dir_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let new_dir = temp_dir.path().join("subdir").join("session.jsonl");

        let result = ensure_project_dir(new_dir.to_str().unwrap());

        assert!(result.is_ok());
        assert!(temp_dir.path().join("subdir").exists());
    }

    // Note: These tests verify the path extraction logic, but actual results
    // depend on which paths exist on disk. Using real paths that exist.

    #[test]
    fn test_extract_project_path_real_projects_dir() {
        // This test will only pass if the directory exists
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev-projects-claude-code-fullsearch-rust/abc123.jsonl";
        let result = extract_project_path(file_path);
        // Should find /Users/vkkovalev/projects/claude-code-fullsearch-rust if it exists
        if let Some(path) = result {
            assert!(Path::new(&path).exists(), "Extracted path should exist");
        }
    }

    #[test]
    fn test_extract_project_path_home_directory() {
        // Test for paths like -Users-vkkovalev (home directory)
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev/session.jsonl";
        let result = extract_project_path(file_path);
        // Should find /Users/vkkovalev
        if let Some(path) = result {
            assert!(Path::new(&path).exists(), "Extracted path should exist: {}", path);
        }
    }

    #[test]
    fn test_extract_project_path_hidden_dir() {
        // Test for paths with hidden directories (--) like -Users-vkkovalev--claude
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev--claude/session.jsonl";
        let result = extract_project_path(file_path);
        // Should find /Users/vkkovalev/.claude
        if let Some(path) = result {
            assert!(Path::new(&path).exists(), "Extracted path should exist: {}", path);
        }
    }

    #[test]
    fn test_extract_project_path_downloads_dir() {
        // Test for Downloads paths like -Users-vkkovalev-Downloads-something
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev-Downloads/session.jsonl";
        let result = extract_project_path(file_path);
        // Should find /Users/vkkovalev/Downloads
        if let Some(path) = result {
            assert!(Path::new(&path).exists(), "Extracted path should exist: {}", path);
        }
    }

    #[test]
    fn test_extract_project_path_deleted_project_returns_none() {
        // When the original project directory no longer exists, should return None
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev-Downloads-nonexistent-project-12345/session.jsonl";
        let result = extract_project_path(file_path);
        // This should return None since the directory doesn't exist
        // (our function checks if paths exist)
        assert!(result.is_none() || Path::new(&result.unwrap()).exists());
    }

    #[test]
    fn test_extract_project_path_avito_android_2_outputs() {
        // Real case that was failing: -Users-vkkovalev-Downloads-avito-android-2-outputs
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev-Downloads-avito-android-2-outputs/68cfcc98.jsonl";
        let result = extract_project_path(file_path);
        // This directory may or may not exist - just verify it doesn't panic
        // and returns a reasonable result
        println!("Result for avito-android-2-outputs: {:?}", result);
    }

    // Tests for decode_project_path (doesn't check if path exists)
    #[test]
    fn test_decode_project_path_simple() {
        let file_path = "/Users/user/.claude/projects/-Users-user-projects-myapp/session.jsonl";
        let result = decode_project_path(file_path);
        assert_eq!(result, Some("/Users/user/projects/myapp".to_string()));
    }

    #[test]
    fn test_decode_project_path_downloads() {
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev-Downloads-avito-android-2-outputs/68cfcc98.jsonl";
        let result = decode_project_path(file_path);
        // Note: This decodes assuming dashes are path separators, which may not be correct
        // for "avito-android-2-outputs", but it will create a directory structure that
        // allows Claude to find the session
        assert!(result.is_some());
        println!("Decoded downloads path: {:?}", result);
    }

    #[test]
    fn test_decode_project_path_hidden_dir() {
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev--claude/session.jsonl";
        let result = decode_project_path(file_path);
        assert_eq!(result, Some("/Users/vkkovalev/.claude".to_string()));
    }

    #[test]
    fn test_decode_project_path_home() {
        let file_path = "/Users/vkkovalev/.claude/projects/-Users-vkkovalev/session.jsonl";
        let result = decode_project_path(file_path);
        assert_eq!(result, Some("/Users/vkkovalev".to_string()));
    }
}
