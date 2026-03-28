use super::path_codec::decode_project_path;
use crate::search::Message;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

const SESSIONS_INDEX_FILE: &str = "sessions-index.json";
const SYNTHETIC_VERSION: &str = "2.1.85";

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
    let mut last_uuid: Option<String> = None;
    let mut msg_uuid_count: usize = 0;

    for line in reader.lines().map_while(Result::ok) {
        let json: serde_json::Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Track UUID chain (all record types)
        if let Some(uuid) = json.get("uuid").and_then(|v| v.as_str()) {
            let parent = json
                .get("parentUuid")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            uuid_to_parent.insert(uuid.to_string(), parent);
            last_uuid = Some(uuid.to_string());
        }

        let msg_type = match json.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };

        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        message_count += 1;

        if json.get("uuid").is_some() {
            msg_uuid_count += 1;
        }

        let ts = json
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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

    let reachable = uuid_to_parent
        .keys()
        .filter(|u| on_chain.contains(*u))
        .count();
    let is_linear = msg_uuid_count == 0 || reachable >= uuid_to_parent.len();

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
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| {
            serde_json::json!({"version": 1, "entries": [], "originalPath": ""})
        }),
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

        let new_uuid = uuid::Uuid::new_v4().to_string();
        json["uuid"] = serde_json::Value::String(new_uuid.clone());
        json["sessionId"] = serde_json::Value::String(new_id.clone());
        json["version"] = serde_json::Value::String(SYNTHETIC_VERSION.to_string());
        json["isSidechain"] = serde_json::Value::Bool(false);

        if let Some(ref parent) = prev_uuid {
            json["parentUuid"] = serde_json::Value::String(parent.clone());
        } else {
            json.as_object_mut().map(|o| o.remove("parentUuid"));
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

    Ok((new_id, new_path.to_string_lossy().to_string()))
}

/// Resume a Claude Code CLI session using exec.
pub fn resume_cli(session_id: &str, file_path: &str) -> Result<(), String> {
    ensure_project_dir(file_path)?;

    let analysis = analyze_session(file_path);
    let project_dir = Path::new(file_path).parent();

    let needs_linearization = analysis
        .as_ref()
        .map(|a| !a.is_linear)
        .unwrap_or(false)
        || project_dir
            .map(|d| !is_session_in_index(d, session_id))
            .unwrap_or(false);

    let resume_id = if needs_linearization {
        eprintln!(
            "[ccs] Session has branched history, creating linear copy to restore conversation..."
        );
        let (id, path) = create_linear_session(file_path)?;
        let linear_analysis = analyze_session(&path);
        if let Some(ref a) = linear_analysis {
            ensure_session_in_index(&id, &path, a);
        }
        id
    } else {
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
