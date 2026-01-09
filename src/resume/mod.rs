use std::fs;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Ensure the project directory exists
pub fn ensure_project_dir(file_path: &str) -> Result<(), String> {
    // Extract project directory from session file path
    let dir = std::path::Path::new(file_path)
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

/// Resume a Claude Code session using exec
pub fn resume(session_id: &str, file_path: &str) -> Result<(), String> {
    // Ensure project dir exists
    ensure_project_dir(file_path)?;

    // Find claude binary
    let claude_path = which::which("claude")
        .map_err(|_| "Claude binary not found in PATH".to_string())?;

    // Use exec to replace current process
    let err = Command::new(claude_path)
        .args(["--resume", session_id])
        .exec();

    // If we get here, exec failed
    Err(format!("Failed to exec claude: {}", err))
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
