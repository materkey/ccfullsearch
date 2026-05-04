/// Debug logging macro — prints to stderr when `CCS_DEBUG` env var is set.
/// Usage: `ccs_debug!("message: {}", value);`
#[macro_export]
macro_rules! ccs_debug {
    ($($arg:tt)*) => {
        if std::env::var("CCS_DEBUG").is_ok() {
            eprintln!($($arg)*);
        }
    };
}

pub mod ai;
pub mod cli;
pub mod dag;
pub mod recent;
pub mod resume;
pub mod search;
pub mod session;
pub mod tree;
pub mod tui;
pub mod update;

pub use session::{SessionProvider, SessionSource};

#[cfg(test)]
pub(crate) static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn get_search_paths() -> Vec<String> {
    let mut search_paths = Vec::new();

    if let Ok(custom_path) = std::env::var("CCFS_SEARCH_PATH") {
        search_paths.push(custom_path);
    } else if let Some(home) = dirs::home_dir() {
        // Claude Code CLI sessions — respect CLAUDE_CONFIG_DIR env var
        let claude_base = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));

        if let Some(cli_path) = claude_base.join("projects").to_str().map(|s| s.to_string()) {
            search_paths.push(cli_path);
        }

        // Codex rollout sessions — respect CODEX_HOME env var.
        let codex_base = std::env::var("CODEX_HOME")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        for subdir in session::CODEX_SESSION_SUBDIRS {
            let path = codex_base.join(subdir);
            if path.exists() {
                if let Some(p) = path.to_str().map(|s| s.to_string()) {
                    search_paths.push(p);
                }
            }
        }

        // Claude Desktop sessions (macOS)
        let macos_desktop =
            home.join("Library/Application Support/Claude/local-agent-mode-sessions");
        if macos_desktop.exists() {
            if let Some(p) = macos_desktop.to_str().map(|s| s.to_string()) {
                search_paths.push(p);
            }
        }

        // Claude Desktop sessions (Linux)
        let linux_desktop = home.join(".config/Claude/local-agent-mode-sessions");
        if linux_desktop.exists() {
            if let Some(p) = linux_desktop.to_str().map(|s| s.to_string()) {
                search_paths.push(p);
            }
        }

        // Fallback if no paths found (e.g. to_str() failed on non-UTF8 home)
        if search_paths.is_empty() {
            if let Some(p) = home
                .join(".claude/projects")
                .to_str()
                .map(|s| s.to_string())
            {
                search_paths.push(p);
            } else {
                search_paths.push("~/.claude/projects".to_string());
            }
        }
    }

    search_paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_search_paths_respects_claude_config_dir() {
        let _lock = crate::TEST_ENV_MUTEX.lock().unwrap();

        // Save and clear potentially interfering env vars
        let prev_ccfs = env::var("CCFS_SEARCH_PATH").ok();
        let prev_config = env::var("CLAUDE_CONFIG_DIR").ok();
        unsafe { env::remove_var("CCFS_SEARCH_PATH") };

        let tmp = std::env::temp_dir().join("ccfs_test_config_dir");
        unsafe { env::set_var("CLAUDE_CONFIG_DIR", tmp.to_str().unwrap()) };

        let paths = get_search_paths();

        // Should use CLAUDE_CONFIG_DIR as base for projects/
        let expected_suffix = tmp.join("projects");
        assert!(
            paths.iter().any(|p| p == expected_suffix.to_str().unwrap()),
            "Expected path containing {:?}, got {:?}",
            expected_suffix,
            paths
        );

        // Restore env
        unsafe { env::remove_var("CLAUDE_CONFIG_DIR") };
        if let Some(v) = prev_config {
            unsafe { env::set_var("CLAUDE_CONFIG_DIR", v) };
        }
        if let Some(v) = prev_ccfs {
            unsafe { env::set_var("CCFS_SEARCH_PATH", v) };
        }
    }

    #[test]
    fn test_search_paths_default_without_env() {
        let _lock = crate::TEST_ENV_MUTEX.lock().unwrap();

        // Save and clear potentially interfering env vars
        let prev_ccfs = env::var("CCFS_SEARCH_PATH").ok();
        let prev_config = env::var("CLAUDE_CONFIG_DIR").ok();
        unsafe { env::remove_var("CCFS_SEARCH_PATH") };
        unsafe { env::remove_var("CLAUDE_CONFIG_DIR") };

        let paths = get_search_paths();

        // Should contain ~/.claude/projects (the default)
        assert!(
            paths.iter().any(|p| p.ends_with(".claude/projects")),
            "Expected a path ending with .claude/projects, got {:?}",
            paths
        );

        // Restore env
        if let Some(v) = prev_config {
            unsafe { env::set_var("CLAUDE_CONFIG_DIR", v) };
        }
        if let Some(v) = prev_ccfs {
            unsafe { env::set_var("CCFS_SEARCH_PATH", v) };
        }
    }

    #[test]
    fn test_search_paths_includes_codex_home_sessions() {
        let _lock = crate::TEST_ENV_MUTEX.lock().unwrap();

        let prev_ccfs = env::var("CCFS_SEARCH_PATH").ok();
        let prev_config = env::var("CLAUDE_CONFIG_DIR").ok();
        let prev_codex = env::var("CODEX_HOME").ok();
        unsafe { env::remove_var("CCFS_SEARCH_PATH") };
        unsafe { env::remove_var("CLAUDE_CONFIG_DIR") };

        let tmp = tempfile::TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        let archived = tmp.path().join("archived_sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&archived).unwrap();
        unsafe { env::set_var("CODEX_HOME", tmp.path()) };

        let paths = get_search_paths();

        assert!(paths.iter().any(|p| p == sessions.to_str().unwrap()));
        assert!(paths.iter().any(|p| p == archived.to_str().unwrap()));

        unsafe { env::remove_var("CODEX_HOME") };
        if let Some(v) = prev_codex {
            unsafe { env::set_var("CODEX_HOME", v) };
        }
        if let Some(v) = prev_config {
            unsafe { env::set_var("CLAUDE_CONFIG_DIR", v) };
        }
        if let Some(v) = prev_ccfs {
            unsafe { env::set_var("CCFS_SEARCH_PATH", v) };
        }
    }
}
