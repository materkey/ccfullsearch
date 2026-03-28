use super::path_codec::decode_project_path;
use std::fs;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Ensure a session is registered in Claude CLI's sessions-index.json.
/// Claude CLI only resumes sessions listed in this index.
pub fn ensure_session_in_index(session_id: &str, file_path: &str) {
    let jsonl_path = Path::new(file_path);
    let project_dir = match jsonl_path.parent() {
        Some(d) => d,
        None => return,
    };
    let index_path = project_dir.join("sessions-index.json");

    // Read existing index (or create minimal structure)
    let mut index: serde_json::Value = if index_path.exists() {
        match fs::read_to_string(&index_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| {
                serde_json::json!({"version": 1, "entries": [], "originalPath": ""})
            }),
            Err(_) => return,
        }
    } else {
        return; // No index file — Claude CLI may not use one here
    };

    let entries = match index.get_mut("entries").and_then(|e| e.as_array_mut()) {
        Some(arr) => arr,
        None => return,
    };

    // Check if session already in index
    let already_exists = entries.iter().any(|e| {
        e.get("sessionId")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s == session_id)
    });

    if already_exists {
        return;
    }

    // Read metadata from the JSONL file
    let (first_prompt, message_count, first_ts, last_ts, git_branch) =
        read_session_metadata(file_path);
    let file_mtime = fs::metadata(file_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let project_path = decode_project_path(file_path).unwrap_or_default();

    let entry = serde_json::json!({
        "sessionId": session_id,
        "fullPath": file_path,
        "fileMtime": file_mtime,
        "firstPrompt": first_prompt,
        "summary": "",
        "messageCount": message_count,
        "created": first_ts,
        "modified": last_ts,
        "gitBranch": git_branch,
        "projectPath": project_path,
        "isSidechain": false
    });

    entries.push(entry);

    // Write back
    if let Ok(json_str) = serde_json::to_string_pretty(&index) {
        let _ = fs::write(&index_path, json_str);
    }
}

/// Read session metadata from a JSONL file.
/// Returns (first_prompt, message_count, first_timestamp, last_timestamp, git_branch).
fn read_session_metadata(
    file_path: &str,
) -> (String, usize, String, String, String) {
    use std::io::{BufRead, BufReader};

    let file = match fs::File::open(file_path) {
        Ok(f) => f,
        Err(_) => return (String::new(), 0, String::new(), String::new(), String::new()),
    };
    let reader = BufReader::new(file);

    let mut first_prompt = String::new();
    let mut message_count: usize = 0;
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut git_branch = String::new();

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = match json.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };

        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        message_count += 1;

        let ts = json
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if first_ts.is_empty() && !ts.is_empty() {
            first_ts = ts.clone();
        }
        if !ts.is_empty() {
            last_ts = ts;
        }

        if git_branch.is_empty() {
            if let Some(b) = json
                .get("gitBranch")
                .or_else(|| json.get("branch"))
                .and_then(|v| v.as_str())
            {
                if !b.is_empty() {
                    git_branch = b.to_string();
                }
            }
        }

        // Extract first user prompt
        if msg_type == "user" && first_prompt.is_empty() {
            if let Some(message) = json.get("message") {
                if let Some(content) = message.get("content") {
                    first_prompt = extract_text_content(content);
                }
            }
        }
    }

    (first_prompt, message_count, first_ts, last_ts, git_branch)
}

/// Extract text content from a message content field (string or array of blocks).
fn extract_text_content(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.chars().take(200).collect();
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    return text.chars().take(200).collect();
                }
            }
        }
    }
    String::new()
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

/// Create a synthetic JSONL with a linear parentUuid chain so Claude CLI
/// shows all messages in Rewind. Returns (new_session_id, new_file_path).
///
/// Claude CLI only shows messages in Rewind that are on the parentUuid chain
/// from the last message. Old sessions with branches/subagents break this chain.
/// We rebuild the chain as a flat linear sequence of user/assistant messages.
fn create_linear_session(file_path: &str) -> Result<(String, String), String> {
    use std::io::{BufRead, BufReader, Write};

    let file =
        fs::File::open(file_path).map_err(|e| format!("Failed to open {}: {}", file_path, e))?;
    let reader = BufReader::new(file);

    let new_id = uuid::Uuid::new_v4().to_string();
    let project_dir = Path::new(file_path)
        .parent()
        .ok_or("No parent directory")?;
    let new_path = project_dir.join(format!("{}.jsonl", new_id));

    let mut out =
        fs::File::create(&new_path).map_err(|e| format!("Failed to create file: {}", e))?;

    let mut prev_uuid: Option<String> = None;
    let mut msg_count: usize = 0;

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {}", e))?;
        let mut json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = json
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        // Assign fresh UUID and link to previous
        let new_uuid = uuid::Uuid::new_v4().to_string();
        json["uuid"] = serde_json::Value::String(new_uuid.clone());
        json["sessionId"] = serde_json::Value::String(new_id.clone());
        json["version"] = serde_json::Value::String("2.1.85".to_string());
        json["isSidechain"] = serde_json::Value::Bool(false);

        if let Some(ref parent) = prev_uuid {
            json["parentUuid"] = serde_json::Value::String(parent.clone());
        } else {
            json.as_object_mut()
                .map(|o| o.remove("parentUuid"));
        }

        writeln!(out, "{}", serde_json::to_string(&json).unwrap_or_default())
            .map_err(|e| format!("Write error: {}", e))?;

        prev_uuid = Some(new_uuid);
        msg_count += 1;
    }

    if msg_count == 0 {
        let _ = fs::remove_file(&new_path);
        return Err("No user/assistant messages found".to_string());
    }

    let new_path_str = new_path.to_string_lossy().to_string();
    Ok((new_id, new_path_str))
}

/// Check if a session needs linearization.
/// Returns true if the session has a broken parentUuid chain (branches, subagents)
/// or is missing from the sessions-index.json.
fn needs_linearization(session_id: &str, file_path: &str) -> bool {
    use std::io::{BufRead, BufReader};

    // Check 1: is session in the index?
    let jsonl_path = Path::new(file_path);
    if let Some(project_dir) = jsonl_path.parent() {
        let index_path = project_dir.join("sessions-index.json");
        if index_path.exists() {
            if let Ok(content) = fs::read_to_string(&index_path) {
                if let Ok(index) = serde_json::from_str::<serde_json::Value>(&content) {
                    let in_index = index
                        .get("entries")
                        .and_then(|e| e.as_array())
                        .is_some_and(|entries| {
                            entries.iter().any(|e| {
                                e.get("sessionId")
                                    .and_then(|v| v.as_str())
                                    .is_some_and(|s| s == session_id)
                            })
                        });
                    if !in_index {
                        return true;
                    }
                }
            }
        }
    }

    // Check 2: is the parentUuid chain linear (all messages reachable from tip)?
    let file = match fs::File::open(file_path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let reader = BufReader::new(file);

    let mut uuid_to_parent: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut last_uuid: Option<String> = None;
    let mut msg_uuids: Vec<String> = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let uuid = json.get("uuid").and_then(|v| v.as_str()).map(|s| s.to_string());

        if let Some(ref u) = uuid {
            let parent = json
                .get("parentUuid")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            uuid_to_parent.insert(u.clone(), parent);
            last_uuid = Some(u.clone());

            if msg_type == "user" || msg_type == "assistant" {
                msg_uuids.push(u.clone());
            }
        }
    }

    if msg_uuids.is_empty() {
        return false;
    }

    // Walk chain from tip — count how many message UUIDs are reachable
    let mut on_chain = std::collections::HashSet::new();
    let mut current = last_uuid;
    while let Some(uuid) = current {
        on_chain.insert(uuid.clone());
        current = uuid_to_parent.get(&uuid).and_then(|p| p.clone());
    }

    let reachable_msgs = msg_uuids.iter().filter(|u| on_chain.contains(*u)).count();

    // If less than all messages are on the chain, it has branches
    reachable_msgs < msg_uuids.len()
}

/// Resume a Claude Code CLI session using exec
pub fn resume_cli(session_id: &str, file_path: &str) -> Result<(), String> {
    ensure_project_dir(file_path)?;

    // Only create a linear copy if the session has broken chain or missing from index
    let (resume_id, _resume_path) = if needs_linearization(session_id, file_path) {
        eprintln!("[ccs] Session has branched history, creating linear copy to restore conversation...");
        let (id, path) = create_linear_session(file_path)?;
        ensure_session_in_index(&id, &path);
        (id, path)
    } else {
        ensure_session_in_index(session_id, file_path);
        (session_id.to_string(), file_path.to_string())
    };

    let claude_path =
        which::which("claude").map_err(|_| "Claude binary not found in PATH".to_string())?;

    let project_dir = decode_project_path(file_path);

    if let Some(ref dir) = project_dir {
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

/// Execute a command, replacing the current process on Unix or spawning on Windows.
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
    use tempfile::TempDir;

    #[test]
    fn test_ensure_project_dir_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let new_dir = temp_dir.path().join("subdir").join("session.jsonl");

        let result = ensure_project_dir(new_dir.to_str().unwrap());

        assert!(result.is_ok());
        assert!(temp_dir.path().join("subdir").exists());
    }
}
