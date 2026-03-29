use super::path_codec::decode_project_path;
use crate::search::Message;
use crate::session;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

const SESSIONS_INDEX_FILE: &str = "sessions-index.json";
const SYNTHETIC_SOURCE_PATH_FIELD: &str = "ccsSyntheticSourcePath";

/// Session metadata extracted from a single JSONL parse pass.
struct SessionAnalysis {
    first_prompt: String,
    message_count: usize,
    first_ts: String,
    last_ts: String,
    git_branch: String,
    /// Whether all user/assistant messages are on the latest parentUuid chain.
    is_linear: bool,
}

/// Parse a JSONL session file once, extracting all metadata needed for resume.
fn analyze_session(file_path: &str) -> Option<SessionAnalysis> {
    let file = fs::File::open(file_path).ok()?;
    let reader = BufReader::new(file);

    let mut first_prompt = String::new();
    let mut message_count: usize = 0;
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut git_branch = String::new();

    let mut uuid_to_parent: HashMap<String, Option<String>> = HashMap::new();
    let mut message_uuids = HashSet::new();
    let mut last_uuid: Option<String> = None;

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(uuid) = session::extract_uuid(&json) {
            let parent = session::extract_parent_uuid(&json);
            uuid_to_parent.insert(uuid.clone(), parent);
            last_uuid = Some(uuid);
        }

        let msg_type = match json.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };

        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        message_count += 1;
        if let Some(uuid) = session::extract_uuid(&json) {
            message_uuids.insert(uuid);
        }

        let ts = json.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        if first_ts.is_empty() && !ts.is_empty() {
            first_ts = ts.to_string();
        }
        if !ts.is_empty() {
            last_ts = ts.to_string();
        }

        if git_branch.is_empty() {
            if let Some(b) = json
                .get("gitBranch")
                .or_else(|| json.get("branch"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                git_branch = b.to_string();
            }
        }

        if msg_type == "user" && first_prompt.is_empty() {
            if let Some(content) = json.get("message").and_then(|m| m.get("content")) {
                let full = Message::extract_content(content);
                first_prompt = full.chars().take(200).collect();
            }
        }
    }

    // Walk chain from tip to count reachable message UUIDs
    let mut on_chain = HashSet::new();
    let mut current = last_uuid;
    while let Some(uuid) = current {
        on_chain.insert(uuid.clone());
        current = uuid_to_parent.get(&uuid).and_then(|p| p.clone());
    }

    let reachable_messages = message_uuids
        .iter()
        .filter(|uuid| on_chain.contains(*uuid))
        .count();
    let is_linear = message_uuids.is_empty() || reachable_messages == message_uuids.len();

    Some(SessionAnalysis {
        first_prompt,
        message_count,
        first_ts,
        last_ts,
        git_branch,
        is_linear,
    })
}

/// Check if session_id exists in the sessions-index.json at the given project dir.
fn is_session_in_index(project_dir: &Path, session_id: &str) -> bool {
    let index_path = project_dir.join(SESSIONS_INDEX_FILE);
    let content = match fs::read_to_string(&index_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let index: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    index
        .get("entries")
        .and_then(|e| e.as_array())
        .is_some_and(|entries| {
            entries.iter().any(|e| {
                e.get("sessionId")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s == session_id)
            })
        })
}

/// Register a session in Claude CLI's sessions-index.json (if index exists and session is missing).
fn ensure_session_in_index(session_id: &str, file_path: &str, analysis: &SessionAnalysis) {
    let project_dir = match Path::new(file_path).parent() {
        Some(d) => d,
        None => return,
    };
    let index_path = project_dir.join(SESSIONS_INDEX_FILE);

    let mut index: serde_json::Value = match fs::read_to_string(&index_path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(
            |_| serde_json::json!({"version": 1, "entries": [], "originalPath": ""}),
        ),
        Err(_) => return,
    };

    let entries = match index.get_mut("entries").and_then(|e| e.as_array_mut()) {
        Some(arr) => arr,
        None => return,
    };

    let already_exists = entries.iter().any(|e| {
        e.get("sessionId")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s == session_id)
    });
    if already_exists {
        return;
    }

    let file_mtime = fs::metadata(file_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let project_path = decode_project_path(file_path).unwrap_or_default();

    entries.push(serde_json::json!({
        "sessionId": session_id,
        "fullPath": file_path,
        "fileMtime": file_mtime,
        "firstPrompt": analysis.first_prompt,
        "summary": "",
        "messageCount": analysis.message_count,
        "created": analysis.first_ts,
        "modified": analysis.last_ts,
        "gitBranch": analysis.git_branch,
        "projectPath": project_path,
        "isSidechain": false
    }));

    if let Ok(json_str) = serde_json::to_string_pretty(&index) {
        let _ = fs::write(&index_path, json_str);
    }
}

fn cleanup_legacy_synthetic_sessions(
    project_dir: &Path,
    keep_file_path: &Path,
    source_file_path: &Path,
) {
    let mut removed_paths: HashSet<String> = HashSet::new();

    let entries = match fs::read_dir(project_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep_file_path {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if !is_synthetic_linear_session_file(&path) {
            continue;
        }
        if !disposable_synthetic_session_matches_source(&path, source_file_path) {
            continue;
        }

        if fs::remove_file(&path).is_ok() {
            removed_paths.insert(path.to_string_lossy().to_string());
        }
    }

    if removed_paths.is_empty() {
        return;
    }

    let index_path = project_dir.join(SESSIONS_INDEX_FILE);
    let mut index: serde_json::Value = match fs::read_to_string(&index_path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(index) => index,
            Err(_) => return,
        },
        Err(_) => return,
    };

    let Some(entries) = index.get_mut("entries").and_then(|e| e.as_array_mut()) else {
        return;
    };

    entries.retain(|entry| {
        let full_path = entry.get("fullPath").and_then(|v| v.as_str()).unwrap_or("");
        !removed_paths.contains(full_path)
    });

    if let Ok(json_str) = serde_json::to_string_pretty(&index) {
        let _ = fs::write(index_path, json_str);
    }
}

fn disposable_synthetic_session_matches_source(path: &Path, source_file_path: &Path) -> bool {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let reader = BufReader::new(file);
    let mut saw_matching_synthetic_record = false;
    let mut saw_non_synthetic_record = false;

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session::is_synthetic_linear_record(&json) {
            if json
                .get(SYNTHETIC_SOURCE_PATH_FIELD)
                .and_then(|value| value.as_str())
                .is_some_and(|path| Path::new(path) == source_file_path)
            {
                saw_matching_synthetic_record = true;
            }
            continue;
        }

        saw_non_synthetic_record = true;
    }

    saw_matching_synthetic_record && !saw_non_synthetic_record
}

/// Ensure the project directory exists
pub fn ensure_project_dir(file_path: &str) -> Result<(), String> {
    let dir = Path::new(file_path)
        .parent()
        .ok_or_else(|| "Cannot get parent directory".to_string())?;

    if !dir.exists() {
        fs::create_dir_all(dir).map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    Ok(())
}

/// Create a synthetic JSONL with a linear parentUuid chain.
/// Claude CLI only shows messages in Rewind that are on the parentUuid chain
/// from the last message. We rebuild as a flat sequence of user/assistant messages.
fn create_linear_session(file_path: &str) -> Result<(String, String), String> {
    let file =
        fs::File::open(file_path).map_err(|e| format!("Failed to open {}: {}", file_path, e))?;
    let source_fingerprint = synthetic_source_fingerprint(&file);
    let reader = BufReader::new(file);
    let latest_chain = super::fork::build_chain_from_tip(file_path);
    let latest_tip = super::fork::latest_tip_uuid(file_path);

    let project_dir = Path::new(file_path).parent().ok_or("No parent directory")?;
    let synthetic_key = format!(
        "ccs-linear:{}:{}:{}",
        file_path,
        latest_tip.as_deref().unwrap_or("no-tip"),
        source_fingerprint,
    );
    let new_id = stable_synthetic_session_id(&synthetic_key);
    let new_path = project_dir.join(format!("{}.jsonl", new_id));

    if new_path.exists() && is_synthetic_linear_session_file(&new_path) {
        return Ok((new_id, new_path.to_string_lossy().to_string()));
    }

    let mut out =
        fs::File::create(&new_path).map_err(|e| format!("Failed to create file: {}", e))?;

    let mut prev_uuid: Option<String> = None;
    let mut rewritten_uuids: HashMap<String, String> = HashMap::new();
    let mut msg_count: usize = 0;

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {}", e))?;
        let mut json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(chain) = latest_chain.as_ref() {
            if let Some(uuid) = session::extract_uuid(&json) {
                if !chain.contains(&uuid) {
                    continue;
                }
            } else if let Some(parent_uuid) = session::extract_parent_uuid(&json) {
                if !chain.contains(&parent_uuid) {
                    continue;
                }
            } else if let Some(leaf_uuid) = session::extract_leaf_uuid(&json) {
                if !chain.contains(&leaf_uuid) {
                    continue;
                }
            }
        }

        let msg_type = json
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let is_message = msg_type == "user" || msg_type == "assistant";
        let original_uuid = session::extract_uuid(&json);
        let had_uuid = original_uuid.is_some();
        let had_parent_uuid = session::extract_parent_uuid(&json).is_some();
        let original_leaf_uuid = session::extract_leaf_uuid(&json);
        let synthetic_parent = prev_uuid.clone();

        if json.get("sessionId").is_some() {
            json["sessionId"] = serde_json::Value::String(new_id.clone());
        }
        if json.get("session_id").is_some() {
            json["session_id"] = serde_json::Value::String(new_id.clone());
        }
        if json.get("isSidechain").is_some() || is_message {
            json["isSidechain"] = serde_json::Value::Bool(false);
        }

        if had_uuid {
            // Rewrite every UUID-bearing record into a single synthetic chain.
            let new_uuid = uuid::Uuid::new_v4().to_string();
            json["uuid"] = serde_json::Value::String(new_uuid.clone());
            if let Some(original_uuid) = original_uuid {
                rewritten_uuids.insert(original_uuid, new_uuid.clone());
            }

            if let Some(ref parent) = synthetic_parent {
                json["parentUuid"] = serde_json::Value::String(parent.clone());
            } else {
                json.as_object_mut().map(|o| o.remove("parentUuid"));
            }

            prev_uuid = Some(new_uuid);
        } else if had_parent_uuid {
            if let Some(ref parent) = synthetic_parent {
                json["parentUuid"] = serde_json::Value::String(parent.clone());
            } else {
                json.as_object_mut().map(|o| o.remove("parentUuid"));
            }
        }

        if let Some(original_leaf_uuid) = original_leaf_uuid {
            if let Some(rewritten_leaf_uuid) = rewritten_uuids.get(&original_leaf_uuid) {
                json["leafUuid"] = serde_json::Value::String(rewritten_leaf_uuid.clone());
            } else if let Some(ref parent) = synthetic_parent {
                json["leafUuid"] = serde_json::Value::String(parent.clone());
            } else {
                json.as_object_mut().map(|o| o.remove("leafUuid"));
            }
        }

        session::mark_synthetic_linear_record(&mut json);
        json[SYNTHETIC_SOURCE_PATH_FIELD] = serde_json::Value::String(file_path.to_string());
        writeln!(out, "{}", serde_json::to_string(&json).unwrap_or_default())
            .map_err(|e| format!("Write error: {}", e))?;

        if is_message {
            msg_count += 1;
        }
    }

    if msg_count == 0 {
        let _ = fs::remove_file(&new_path);
        return Err("No user/assistant messages found".to_string());
    }

    Ok((new_id, new_path.to_string_lossy().to_string()))
}

fn synthetic_source_fingerprint(file: &fs::File) -> String {
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return "unknown-source".to_string(),
    };

    let len = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    format!("{len}:{modified}")
}

fn is_synthetic_linear_session_file(path: &Path) -> bool {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if session::is_synthetic_linear_record(&json) {
            return true;
        }
    }

    false
}

fn stable_synthetic_session_id(key: &str) -> String {
    let h1 = fnv1a64(key.as_bytes(), 0xcbf29ce484222325);
    let h2 = fnv1a64(key.as_bytes(), 0x84222325cbf29ce4);

    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (h1 >> 32) as u32,
        ((h1 >> 16) & 0xffff) as u16,
        (h1 & 0xffff) as u16,
        (h2 >> 48) as u16,
        h2 & 0x0000_ffff_ffff_ffff,
    )
}

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Resume a Claude Code CLI session using exec.
pub fn resume_cli(session_id: &str, file_path: &str) -> Result<(), String> {
    ensure_project_dir(file_path)?;

    let analysis = analyze_session(file_path);
    let project_dir = Path::new(file_path).parent();

    let is_branched = analysis.as_ref().map(|a| !a.is_linear).unwrap_or(false);
    let in_index = project_dir
        .map(|d| is_session_in_index(d, session_id))
        .unwrap_or(false);

    let resume_id = if is_branched {
        // Session has actual branching — create a linear copy
        eprintln!("[ccs] Session has branched history, creating linear copy to resume...");
        let (id, path) = create_linear_session(file_path)?;
        cleanup_legacy_synthetic_sessions(
            project_dir.ok_or("No parent directory")?,
            Path::new(&path),
            Path::new(file_path),
        );
        let linear_analysis = analyze_session(&path);
        if let Some(ref a) = linear_analysis {
            ensure_session_in_index(&id, &path, a);
        }
        id
    } else {
        // Linear session — just register in index if needed
        if !in_index {
            eprintln!("[ccs] Registering session in index...");
        }
        if let Some(ref a) = analysis {
            ensure_session_in_index(session_id, file_path, a);
        }
        session_id.to_string()
    };

    let claude_path =
        which::which("claude").map_err(|_| "Claude binary not found in PATH".to_string())?;

    let decoded_project_dir = decode_project_path(file_path);

    if let Some(ref dir) = decoded_project_dir {
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create project directory {}: {}", dir, e))?;
        }

        let mut cmd = Command::new(&claude_path);
        cmd.current_dir(dir).args(["--resume", &resume_id]);
        return exec_command(&mut cmd);
    }

    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp".to_string());

    let mut cmd = Command::new(&claude_path);
    cmd.current_dir(&home_dir).args(["--resume", &resume_id]);
    exec_command(&mut cmd)
}

/// Open Claude Desktop app for Desktop sessions
pub fn resume_desktop() -> Result<(), String> {
    let mut cmd = Command::new("open");
    cmd.args(["-a", "Claude"]);
    exec_command(&mut cmd)
}

#[cfg(unix)]
fn exec_command(cmd: &mut Command) -> Result<(), String> {
    let err = cmd.exec();
    Err(format!("Failed to exec: {}", err))
}

#[cfg(not(unix))]
fn exec_command(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("Failed to spawn: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Process exited with {}", status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_ensure_project_dir_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let new_dir = temp_dir.path().join("subdir").join("session.jsonl");

        let result = ensure_project_dir(new_dir.to_str().unwrap());

        assert!(result.is_ok());
        assert!(temp_dir.path().join("subdir").exists());
    }

    #[test]
    fn test_analyze_session_treats_interleaved_metadata_as_linear() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("linear_with_metadata.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Checkpoint","uuid":"m1","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Continue"}},"uuid":"u2","parentUuid":"m1","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Done"}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();

        let analysis = analyze_session(path.to_str().unwrap()).unwrap();
        assert!(analysis.is_linear);
    }

    #[test]
    fn test_analyze_session_still_detects_real_branch_with_metadata_nodes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_with_metadata.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"m1","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch A"}},"uuid":"u2","parentUuid":"m1","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"A"}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"m2","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch B"}},"uuid":"u3","parentUuid":"m2","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"B"}},"uuid":"a3","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();

        let analysis = analyze_session(path.to_str().unwrap()).unwrap();
        assert!(!analysis.is_linear);
    }

    #[test]
    fn test_create_linear_session_excludes_abandoned_branch_messages() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_for_resume.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"branch-a-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch A question"}},"uuid":"u2","parentUuid":"branch-a-root","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Branch A answer"}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"branch-b-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch B question"}},"uuid":"u3","parentUuid":"branch-b-root","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Branch B answer"}},"uuid":"a3","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();

        let (_new_id, linear_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let content = fs::read_to_string(&linear_path).unwrap();

        assert!(content.contains("Hello"));
        assert!(content.contains("Hi"));
        assert!(content.contains("Branch B question"));
        assert!(content.contains("Branch B answer"));
        assert!(!content.contains("Branch A question"));
        assert!(!content.contains("Branch A answer"));
    }

    #[test]
    fn test_create_linear_session_marks_records_as_synthetic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_for_synthetic_marker.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"branch-a-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Old branch"}},"uuid":"a2","parentUuid":"branch-a-root","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"branch-b-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Latest branch"}},"uuid":"a3","parentUuid":"branch-b-root","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();

        let (_new_id, linear_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let content = fs::read_to_string(&linear_path).unwrap();

        for line in content.lines().filter(|line| !line.is_empty()) {
            let json: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(session::is_synthetic_linear_record(&json));
        }
    }

    #[test]
    fn test_create_linear_session_rewrites_metadata_uuids_into_linear_chain() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_with_trailing_metadata.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"old-branch-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Old branch"}},"uuid":"a2","parentUuid":"old-branch-root","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"latest-branch-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Latest question"}},"uuid":"u2","parentUuid":"latest-branch-root","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Latest answer"}},"uuid":"a3","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Trailing metadata tip","uuid":"summary-tip","parentUuid":"a3","sessionId":"s1","timestamp":"2025-01-01T00:06:00Z"}}"#).unwrap();

        let (new_id, linear_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let content = fs::read_to_string(&linear_path).unwrap();

        let original_uuids = [
            "p0",
            "u1",
            "a1",
            "latest-branch-root",
            "u2",
            "a3",
            "summary-tip",
        ];
        let mut prev_uuid: Option<String> = None;
        let mut saw_trailing_summary = false;

        for line in content.lines().filter(|line| !line.is_empty()) {
            let json: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(
                session::extract_session_id(&json),
                Some(new_id.clone()),
                "every kept record should point at the synthetic session"
            );

            if json.get("type").and_then(|v| v.as_str()) == Some("summary") {
                saw_trailing_summary = true;
            }

            if let Some(uuid) = session::extract_uuid(&json) {
                assert!(
                    !original_uuids.contains(&uuid.as_str()),
                    "synthetic session must not reuse original UUIDs"
                );

                let parent = session::extract_parent_uuid(&json);
                assert_eq!(
                    parent, prev_uuid,
                    "uuid-bearing records should form a flat parent chain"
                );
                prev_uuid = Some(uuid);
            }
        }

        assert!(
            saw_trailing_summary,
            "latest-branch summary should be preserved"
        );
    }

    #[test]
    fn test_create_linear_session_excludes_leaf_uuid_only_summary_from_abandoned_branch() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_with_compaction_summaries.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();

        writeln!(f, r#"{{"type":"system","uuid":"old-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Old branch answer"}},"uuid":"old-a1","parentUuid":"old-root","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Old branch compaction summary","leafUuid":"old-a1","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();

        writeln!(f, r#"{{"type":"system","uuid":"latest-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Latest branch question"}},"uuid":"u2","parentUuid":"latest-root","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Latest branch answer"}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();

        let (_new_id, linear_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let content = fs::read_to_string(&linear_path).unwrap();

        assert!(content.contains("Latest branch question"));
        assert!(content.contains("Latest branch answer"));
        assert!(
            !content.contains("Old branch compaction summary"),
            "leafUuid-only summaries from abandoned branches must be pruned"
        );
    }

    #[test]
    fn test_create_linear_session_rewrites_leaf_uuid_only_summary_links() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_with_leaf_uuid_summary.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"old-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Old branch answer"}},"uuid":"old-a1","parentUuid":"old-root","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();

        writeln!(f, r#"{{"type":"system","uuid":"latest-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Latest branch question"}},"uuid":"u2","parentUuid":"latest-root","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Latest branch answer"}},"uuid":"latest-a1","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Latest branch compaction summary","leafUuid":"latest-a1","sessionId":"s1","timestamp":"2025-01-01T00:06:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Continue after compaction"}},"uuid":"u3","parentUuid":"latest-a1","sessionId":"s1","timestamp":"2025-01-01T00:06:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Final answer"}},"uuid":"a3","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:07:00Z"}}"#).unwrap();

        let (_new_id, linear_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let content = fs::read_to_string(&linear_path).unwrap();

        let mut rewritten_latest_answer_uuid: Option<String> = None;
        let mut rewritten_summary_leaf_uuid: Option<String> = None;

        for line in content.lines().filter(|line| !line.is_empty()) {
            let json: serde_json::Value = serde_json::from_str(line).unwrap();

            if json.get("type").and_then(|v| v.as_str()) == Some("assistant")
                && json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .map(Message::extract_content)
                    .as_deref()
                    == Some("Latest branch answer")
            {
                rewritten_latest_answer_uuid = session::extract_uuid(&json);
            }

            if json.get("type").and_then(|v| v.as_str()) == Some("summary")
                && json.get("summary").and_then(|v| v.as_str())
                    == Some("Latest branch compaction summary")
            {
                rewritten_summary_leaf_uuid = session::extract_leaf_uuid(&json);
            }
        }

        assert!(
            rewritten_latest_answer_uuid.is_some(),
            "latest branch answer should remain in the synthetic session"
        );
        assert!(
            rewritten_summary_leaf_uuid.is_some(),
            "kept compaction summary should still carry a leafUuid"
        );
        assert_eq!(rewritten_summary_leaf_uuid, rewritten_latest_answer_uuid);
        assert_ne!(
            rewritten_summary_leaf_uuid.as_deref(),
            Some("latest-a1"),
            "leafUuid should point at the rewritten synthetic UUID, not the original branch UUID"
        );
    }

    #[test]
    fn test_create_linear_session_reuses_same_synthetic_file_for_same_tip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_for_reuse.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Old branch"}},"uuid":"u2","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Old answer"}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Latest branch"}},"uuid":"u3","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Latest answer"}},"uuid":"a3","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();

        let (first_id, first_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let (second_id, second_path) = create_linear_session(path.to_str().unwrap()).unwrap();

        assert_eq!(first_id, second_id);
        assert_eq!(first_path, second_path);

        let jsonl_files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
            .collect();
        assert_eq!(
            jsonl_files.len(),
            2,
            "re-entering the same branched session should reuse the same synthetic file"
        );
    }

    #[test]
    fn test_create_linear_session_regenerates_when_summary_is_appended_without_new_tip_uuid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_for_append_regeneration.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"old-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Old answer"}},"uuid":"old-a1","parentUuid":"old-root","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"latest-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Latest branch"}},"uuid":"u2","parentUuid":"latest-root","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Latest answer"}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();
        drop(f);

        let (first_id, first_path) = create_linear_session(path.to_str().unwrap()).unwrap();

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Latest branch compaction summary","leafUuid":"a2","sessionId":"s1","timestamp":"2025-01-01T00:06:00Z"}}"#).unwrap();
        drop(f);

        let (second_id, second_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let regenerated_content = fs::read_to_string(&second_path).unwrap();

        assert_ne!(first_id, second_id);
        assert_ne!(first_path, second_path);
        assert!(
            regenerated_content.contains("Latest branch compaction summary"),
            "appended summary-only metadata should force regeneration of the synthetic session"
        );
    }

    #[test]
    fn test_create_linear_session_excludes_abandoned_branch_messages_with_invalid_tail_line() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("branched_with_invalid_tail.jsonl");
        let mut f = fs::File::create(&path).unwrap();

        writeln!(f, r#"{{"type":"progress","uuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"uuid":"u1","parentUuid":"p0","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Hi"}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"branch-a-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch A question"}},"uuid":"u2","parentUuid":"branch-a-root","sessionId":"s1","timestamp":"2025-01-01T00:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Branch A answer"}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2025-01-01T00:05:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"system","uuid":"branch-b-root","parentUuid":"a1","sessionId":"s1","timestamp":"2025-01-01T00:03:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Branch B question"}},"uuid":"u3","parentUuid":"branch-b-root","sessionId":"s1","timestamp":"2025-01-01T00:04:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Branch B answer"}},"uuid":"a3","parentUuid":"u3","sessionId":"s1","timestamp":"2025-01-01T00:05:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":"truncated""#).unwrap();

        let (_new_id, linear_path) = create_linear_session(path.to_str().unwrap()).unwrap();
        let content = fs::read_to_string(&linear_path).unwrap();

        assert!(content.contains("Hello"));
        assert!(content.contains("Hi"));
        assert!(content.contains("Branch B question"));
        assert!(content.contains("Branch B answer"));
        assert!(!content.contains("Branch A question"));
        assert!(!content.contains("Branch A answer"));
    }

    #[test]
    fn test_cleanup_legacy_synthetic_sessions_removes_old_files_and_index_entries() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path();
        let source_path = project_dir.join("source.jsonl");
        let keep_path = project_dir.join("keep.jsonl");
        let legacy_path = project_dir.join("legacy.jsonl");
        let real_path = project_dir.join("real.jsonl");

        fs::write(
            &keep_path,
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"keep"}},"sessionId":"keep","timestamp":"2025-01-01T00:00:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
                source_path.to_string_lossy()
            ),
        )
        .unwrap();
        fs::write(
            &legacy_path,
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":"legacy"}},"sessionId":"legacy","timestamp":"2025-01-01T00:00:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
                source_path.to_string_lossy()
            ),
        )
        .unwrap();
        fs::write(
            &real_path,
            r#"{"type":"user","message":{"role":"user","content":"real"},"sessionId":"real","timestamp":"2025-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        let index_path = project_dir.join(SESSIONS_INDEX_FILE);
        fs::write(
            &index_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "entries": [
                    {"sessionId": "keep", "fullPath": keep_path.to_string_lossy()},
                    {"sessionId": "legacy", "fullPath": legacy_path.to_string_lossy()},
                    {"sessionId": "real", "fullPath": real_path.to_string_lossy()}
                ],
                "originalPath": ""
            }))
            .unwrap(),
        )
        .unwrap();

        cleanup_legacy_synthetic_sessions(project_dir, &keep_path, &source_path);

        assert!(keep_path.exists(), "current synthetic file should be kept");
        assert!(
            !legacy_path.exists(),
            "legacy synthetic file should be removed"
        );
        assert!(
            real_path.exists(),
            "real session file should not be removed"
        );

        let index: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(index_path).unwrap()).unwrap();
        let entries = index["entries"].as_array().unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry["sessionId"] == "keep"));
        assert!(entries.iter().any(|entry| entry["sessionId"] == "real"));
        assert!(!entries.iter().any(|entry| entry["sessionId"] == "legacy"));
    }

    #[test]
    fn test_cleanup_legacy_synthetic_sessions_keeps_other_synthetic_sources() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path();
        let source_a = project_dir.join("source-a.jsonl");
        let source_b = project_dir.join("source-b.jsonl");
        let keep_path = project_dir.join("keep.jsonl");
        let stale_same_source_path = project_dir.join("stale-same-source.jsonl");
        let other_source_path = project_dir.join("other-source.jsonl");

        fs::write(
            &keep_path,
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"keep"}},"sessionId":"keep","timestamp":"2025-01-01T00:00:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
                source_a.to_string_lossy()
            ),
        )
        .unwrap();
        fs::write(
            &stale_same_source_path,
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":"legacy"}},"sessionId":"legacy-a","timestamp":"2025-01-01T00:00:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
                source_a.to_string_lossy()
            ),
        )
        .unwrap();
        fs::write(
            &other_source_path,
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":"other"}},"sessionId":"legacy-b","timestamp":"2025-01-01T00:00:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
                source_b.to_string_lossy()
            ),
        )
        .unwrap();

        let index_path = project_dir.join(SESSIONS_INDEX_FILE);
        fs::write(
            &index_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "entries": [
                    {"sessionId": "keep", "fullPath": keep_path.to_string_lossy()},
                    {"sessionId": "legacy-a", "fullPath": stale_same_source_path.to_string_lossy()},
                    {"sessionId": "legacy-b", "fullPath": other_source_path.to_string_lossy()}
                ],
                "originalPath": ""
            }))
            .unwrap(),
        )
        .unwrap();

        cleanup_legacy_synthetic_sessions(project_dir, &keep_path, &source_a);

        assert!(keep_path.exists(), "current synthetic file should be kept");
        assert!(
            !stale_same_source_path.exists(),
            "older synthetic copies for the same source should be removed"
        );
        assert!(
            other_source_path.exists(),
            "synthetic sessions from other resumed branches must be preserved"
        );

        let index: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(index_path).unwrap()).unwrap();
        let entries = index["entries"].as_array().unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry["sessionId"] == "keep"));
        assert!(entries.iter().any(|entry| entry["sessionId"] == "legacy-b"));
        assert!(!entries.iter().any(|entry| entry["sessionId"] == "legacy-a"));
    }

    #[test]
    fn test_cleanup_legacy_synthetic_sessions_keeps_resumed_synthetic_sessions_with_real_replies() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path();
        let source_path = project_dir.join("source.jsonl");
        let keep_path = project_dir.join("keep.jsonl");
        let resumed_path = project_dir.join("resumed.jsonl");

        fs::write(
            &keep_path,
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":"keep"}},"sessionId":"keep","timestamp":"2025-01-01T00:00:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
                source_path.to_string_lossy()
            ),
        )
        .unwrap();

        let mut resumed = fs::File::create(&resumed_path).unwrap();
        writeln!(
            resumed,
            r#"{{"type":"user","message":{{"role":"user","content":"bootstrap"}},"sessionId":"resumed","timestamp":"2025-01-01T00:00:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
            source_path.to_string_lossy()
        )
        .unwrap();
        writeln!(
            resumed,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"bootstrap reply"}},"sessionId":"resumed","timestamp":"2025-01-01T00:01:00Z","ccsSyntheticLinear":true,"ccsSyntheticSourcePath":"{}"}}"#,
            source_path.to_string_lossy()
        )
        .unwrap();
        writeln!(
            resumed,
            r#"{{"type":"user","message":{{"role":"user","content":"real follow-up"}},"sessionId":"resumed","timestamp":"2025-01-01T00:02:00Z"}}"#
        )
        .unwrap();
        writeln!(
            resumed,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"real answer"}},"sessionId":"resumed","timestamp":"2025-01-01T00:03:00Z"}}"#
        )
        .unwrap();

        let index_path = project_dir.join(SESSIONS_INDEX_FILE);
        fs::write(
            &index_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "entries": [
                    {"sessionId": "keep", "fullPath": keep_path.to_string_lossy()},
                    {"sessionId": "resumed", "fullPath": resumed_path.to_string_lossy()}
                ],
                "originalPath": ""
            }))
            .unwrap(),
        )
        .unwrap();

        cleanup_legacy_synthetic_sessions(project_dir, &keep_path, &source_path);

        assert!(keep_path.exists(), "current synthetic file should be kept");
        assert!(
            resumed_path.exists(),
            "resumed synthetic sessions with real follow-up records must be preserved"
        );

        let index: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(index_path).unwrap()).unwrap();
        let entries = index["entries"].as_array().unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry["sessionId"] == "keep"));
        assert!(entries.iter().any(|entry| entry["sessionId"] == "resumed"));
    }
}
