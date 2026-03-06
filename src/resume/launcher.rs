use super::path_codec::decode_project_path;
use std::fs;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

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

/// Resume a Claude Code CLI session using exec
pub fn resume_cli(session_id: &str, file_path: &str) -> Result<(), String> {
    ensure_project_dir(file_path)?;

    let claude_path =
        which::which("claude").map_err(|_| "Claude binary not found in PATH".to_string())?;

    let project_dir = decode_project_path(file_path);

    if let Some(ref dir) = project_dir {
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create project directory {}: {}", dir, e))?;
        }

        let mut cmd = Command::new(&claude_path);
        cmd.current_dir(dir).args(["--resume", session_id]);
        return exec_command(&mut cmd);
    }

    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp".to_string());

    let mut cmd = Command::new(&claude_path);
    cmd.current_dir(&home_dir).args(["--resume", session_id]);
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
