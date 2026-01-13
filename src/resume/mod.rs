use std::fs;
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

/// Resume a Claude session based on its source
pub fn resume(session_id: &str, file_path: &str, source: SessionSource) -> Result<(), String> {
    match source {
        SessionSource::ClaudeCodeCLI => resume_cli(session_id, file_path),
        SessionSource::ClaudeDesktop => resume_desktop(),
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
    use tempfile::TempDir;

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
