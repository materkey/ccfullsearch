use super::path_codec::decode_project_path;
use crate::search::Message;
use std::fs;
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

const SESSIONS_INDEX_FILE: &str = "sessions-index.json";

/// Session metadata extracted from a single JSONL parse pass.
struct SessionAnalysis {
    first_prompt: String,
    message_count: usize,
    first_ts: String,
    last_ts: String,
    git_branch: String,
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

    Some(SessionAnalysis {
        first_prompt,
        message_count,
        first_ts,
        last_ts,
        git_branch,
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

/// Ensure the project directory and session index are set up for resume.
/// Returns the session ID to pass to `claude --resume`.
///
/// Cross-project resume works because we:
/// 1. Decode the project path from the JSONL file location
/// 2. Set cwd to that project directory (in build_resume_command)
/// 3. Ensure the session is registered in sessions-index.json
///
/// So `claude --resume <session-id>` finds it in the correct project dir.
pub(super) fn prepare_resume(session_id: &str, file_path: &str) -> Result<String, String> {
    ensure_project_dir(file_path)?;

    let analysis = analyze_session(file_path);
    let project_dir = Path::new(file_path).parent();
    let in_index = project_dir
        .map(|d| is_session_in_index(d, session_id))
        .unwrap_or(false);

    if !in_index {
        if let Some(ref a) = analysis {
            ensure_session_in_index(session_id, file_path, a);
        }
    }

    Ok(session_id.to_string())
}

/// Build the resume command arguments. Returns (working_dir, resume_arg).
/// Extracted for testability.
pub(super) fn build_resume_command(
    session_id: &str,
    file_path: &str,
) -> Result<(String, String), String> {
    let resume_arg = prepare_resume(session_id, file_path)?;

    let decoded_project_dir = decode_project_path(file_path);

    let working_dir = if let Some(ref dir) = decoded_project_dir {
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create project directory {}: {}", dir, e))?;
        }
        dir.clone()
    } else {
        dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/tmp".to_string())
    };

    Ok((working_dir, resume_arg))
}

/// Resume a Claude Code CLI session using exec.
pub fn resume_cli(session_id: &str, file_path: &str) -> Result<(), String> {
    let (working_dir, resume_arg) = build_resume_command(session_id, file_path)?;

    let claude_path =
        which::which("claude").map_err(|_| "Claude binary not found in PATH".to_string())?;

    if std::env::var("CCS_DEBUG").is_ok() {
        eprintln!(
            "[ccs:resume_cli] claude={} cwd={} --resume {}",
            claude_path.display(),
            working_dir,
            resume_arg
        );
    }

    let mut cmd = Command::new(&claude_path);
    cmd.current_dir(&working_dir)
        .args(["--resume", &resume_arg]);
    exec_command(&mut cmd)
}

/// Resume a Claude Code CLI session as a child process (returns when claude exits).
/// Unlike `resume_cli()` which uses exec() and replaces the current process,
/// this spawns claude as a child and waits for it to finish, allowing the caller
/// to continue afterwards (e.g., return to TUI in overlay mode).
pub fn resume_cli_child(session_id: &str, file_path: &str) -> Result<(), String> {
    let (working_dir, resume_arg) = build_resume_command(session_id, file_path)?;

    let claude_path =
        which::which("claude").map_err(|_| "Claude binary not found in PATH".to_string())?;

    if std::env::var("CCS_DEBUG").is_ok() {
        eprintln!(
            "[ccs:resume_cli_child] claude={} cwd={} --resume {}",
            claude_path.display(),
            working_dir,
            resume_arg
        );
    }

    // Any exit code is acceptable — Claude CLI exits non-zero on Ctrl-C (130),
    // /exit, or other normal termination paths. In overlay mode we just need to
    // know that the process finished so we can return to the TUI.
    Command::new(&claude_path)
        .current_dir(&working_dir)
        .args(["--resume", &resume_arg])
        .status()
        .map_err(|e| format!("Failed to spawn claude: {}", e))?;

    Ok(())
}

/// Open Claude Desktop app for Desktop sessions
pub fn resume_desktop() -> Result<(), String> {
    let mut cmd = Command::new("open");
    cmd.args(["-a", "Claude"]);
    exec_command(&mut cmd)
}

/// Open Claude Desktop app as a child process and wait for it to close.
/// Uses `open -W` so the TUI regains control only after the user closes Claude Desktop.
pub fn resume_desktop_child() -> Result<(), String> {
    // Ignore exit code: `open -W` can return non-zero for benign reasons
    // (e.g., app already running). Same approach as resume_cli_child().
    Command::new("open")
        .args(["-W", "-a", "Claude"])
        .status()
        .map_err(|e| format!("Failed to open Claude Desktop: {}", e))?;

    Ok(())
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
    fn test_prepare_resume_returns_session_id() {
        let dir = TempDir::new().unwrap();
        let session_id = "abc-123";
        let session_file = dir.path().join(format!("{}.jsonl", session_id));
        {
            let mut f = fs::File::create(&session_file).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"sessionId":"{}","timestamp":"2025-01-01T00:00:00Z"}}"#, session_id).unwrap();
        }

        let result = prepare_resume(session_id, session_file.to_str().unwrap()).unwrap();

        // Should return session ID (Claude CLI doesn't accept file paths for --resume)
        assert_eq!(result, session_id);
    }

    #[test]
    fn test_build_resume_command_returns_working_dir_and_session_id() {
        // Place session file inside a .claude/projects/<encoded-dir>/ structure
        // so decode_project_path can resolve the working directory
        let dir = TempDir::new().unwrap();
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-tmp-myproject");
        fs::create_dir_all(&project_dir).unwrap();

        let session_id = "test-build-cmd";
        let session_file = project_dir.join(format!("{}.jsonl", session_id));
        {
            let mut f = fs::File::create(&session_file).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"sessionId":"{}","timestamp":"2025-01-01T00:00:00Z"}}"#, session_id).unwrap();
        }

        let (working_dir, resume_arg) =
            build_resume_command(session_id, session_file.to_str().unwrap()).unwrap();

        assert_eq!(resume_arg, session_id);
        // working_dir should be a valid directory
        assert!(
            Path::new(&working_dir).exists(),
            "working_dir should exist: {}",
            working_dir
        );
    }

    #[test]
    fn test_resume_cli_child_fails_without_claude_binary() {
        // resume_cli_child should fail gracefully when claude is not in PATH
        let dir = TempDir::new().unwrap();
        let project_dir = dir
            .path()
            .join(".claude")
            .join("projects")
            .join("-tmp-testproj");
        fs::create_dir_all(&project_dir).unwrap();

        let session_id = "child-test";
        let session_file = project_dir.join(format!("{}.jsonl", session_id));
        {
            let mut f = fs::File::create(&session_file).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"sessionId":"{}","timestamp":"2025-01-01T00:00:00Z"}}"#, session_id).unwrap();
        }

        // Override PATH to ensure claude is not found
        let original_path = std::env::var("PATH").unwrap_or_default();
        unsafe { std::env::set_var("PATH", dir.path()) };

        let result = resume_cli_child(session_id, session_file.to_str().unwrap());

        unsafe { std::env::set_var("PATH", &original_path) };

        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("not found"),
            "Should report claude binary not found"
        );
    }
}
