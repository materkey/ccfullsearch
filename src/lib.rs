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

/// Test helper: snapshot a set of env vars and restore them on Drop, so a
/// panicking assertion can't leave the process env in a bad state and poison
/// [`TEST_ENV_MUTEX`] for subsequent tests.
#[cfg(test)]
pub(crate) struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

#[cfg(test)]
impl EnvGuard {
    pub(crate) fn new(keys: &[&'static str]) -> Self {
        let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        Self { saved }
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            // SAFETY: tests run single-threaded behind TEST_ENV_MUTEX, so no
            // other thread is reading the environment during restore.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

pub fn get_search_paths() -> Vec<String> {
    build_search_paths(
        std::env::var("CCFS_SEARCH_PATH").ok(),
        std::env::var("CLAUDE_CONFIG_DIR").ok(),
        std::env::var("CODEX_HOME").ok(),
        dirs::home_dir(),
    )
}

/// Assemble search paths from the env overrides and per-source defaults.
/// Env values are passed as parameters so tests don't touch the process env.
fn build_search_paths(
    custom_path: Option<String>,
    claude_config_dir: Option<String>,
    codex_home: Option<String>,
    home: Option<std::path::PathBuf>,
) -> Vec<String> {
    let mut search_paths = Vec::new();

    if let Some(custom) = custom_path {
        search_paths.push(custom);
    } else if let Some(home) = home {
        add_claude_cli_path(&mut search_paths, claude_config_dir, &home);
        add_codex_paths(&mut search_paths, codex_home, &home);
        add_desktop_paths(&mut search_paths, &home);
        add_opencode_path(&mut search_paths);

        // Fallback if no paths found (e.g. to_str() failed on non-UTF8 home)
        if search_paths.is_empty() {
            add_fallback_path(&mut search_paths, &home);
        }
    }

    search_paths
}

/// Claude Code CLI sessions — respect CLAUDE_CONFIG_DIR env var.
fn add_claude_cli_path(
    search_paths: &mut Vec<String>,
    claude_config_dir: Option<String>,
    home: &std::path::Path,
) {
    let claude_base = claude_config_dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));

    if let Some(cli_path) = claude_base.join("projects").to_str().map(|s| s.to_string()) {
        search_paths.push(cli_path);
    }
}

/// Codex rollout sessions — respect CODEX_HOME env var.
fn add_codex_paths(
    search_paths: &mut Vec<String>,
    codex_home: Option<String>,
    home: &std::path::Path,
) {
    let codex_base = codex_home
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
}

/// Claude Desktop sessions (macOS and Linux locations).
fn add_desktop_paths(search_paths: &mut Vec<String>, home: &std::path::Path) {
    // Claude Desktop sessions (macOS)
    let macos_desktop = home.join("Library/Application Support/Claude/local-agent-mode-sessions");
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
}

/// Opencode SQLite database. The search layer dispatches paths
/// ending in `opencode.db` to a SQL-based scanner instead of ripgrep.
fn add_opencode_path(search_paths: &mut Vec<String>) {
    if let Some(db) = session::opencode::opencode_database_path() {
        if let Some(p) = db.to_str().map(|s| s.to_string()) {
            search_paths.push(p);
        }
    }
}

/// Fallback to `~/.claude/projects` when no other paths were found.
fn add_fallback_path(search_paths: &mut Vec<String>, home: &std::path::Path) {
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

    // The tests below call the parameterized helpers directly, so they don't
    // touch the process environment and don't need TEST_ENV_MUTEX.

    #[test]
    fn test_build_search_paths_custom_path_wins() {
        let paths = build_search_paths(
            Some("/custom/override".to_string()),
            Some("/ignored".to_string()),
            None,
            dirs::home_dir(),
        );
        assert_eq!(paths, vec!["/custom/override".to_string()]);
    }

    #[test]
    fn test_build_search_paths_without_home_is_empty() {
        let paths = build_search_paths(None, None, None, None);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_build_search_paths_includes_claude_projects_for_home() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = build_search_paths(None, None, None, Some(tmp.path().to_path_buf()));
        let expected = tmp.path().join(".claude/projects");
        assert!(
            paths.iter().any(|p| p == expected.to_str().unwrap()),
            "Expected {:?} in {:?}",
            expected,
            paths
        );
    }

    #[test]
    fn test_add_claude_cli_path_uses_config_dir() {
        let mut paths = Vec::new();
        add_claude_cli_path(
            &mut paths,
            Some("/opt/claude-config".to_string()),
            std::path::Path::new("/home/u"),
        );
        assert_eq!(paths, vec!["/opt/claude-config/projects".to_string()]);
    }

    #[test]
    fn test_add_claude_cli_path_defaults_to_home() {
        let mut paths = Vec::new();
        add_claude_cli_path(&mut paths, None, std::path::Path::new("/home/u"));
        assert_eq!(paths, vec!["/home/u/.claude/projects".to_string()]);
    }

    #[test]
    fn test_add_codex_paths_only_existing_subdirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let codex = tmp.path().join("codex-home");
        std::fs::create_dir_all(codex.join("sessions")).unwrap();
        // archived_sessions intentionally absent
        let mut paths = Vec::new();
        add_codex_paths(
            &mut paths,
            Some(codex.to_str().unwrap().to_string()),
            tmp.path(),
        );
        assert_eq!(
            paths,
            vec![codex.join("sessions").to_str().unwrap().to_string()]
        );
    }

    #[test]
    fn test_add_codex_paths_defaults_to_home_codex() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".codex/sessions")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".codex/archived_sessions")).unwrap();
        let mut paths = Vec::new();
        add_codex_paths(&mut paths, None, tmp.path());
        assert_eq!(paths.len(), 2, "got {:?}", paths);
        assert!(paths[0].ends_with("/.codex/sessions"));
        assert!(paths[1].ends_with("/.codex/archived_sessions"));
    }

    #[test]
    fn test_add_desktop_paths_pushes_existing_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(
            tmp.path()
                .join("Library/Application Support/Claude/local-agent-mode-sessions"),
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join(".config/Claude/local-agent-mode-sessions"))
            .unwrap();
        let mut paths = Vec::new();
        add_desktop_paths(&mut paths, tmp.path());
        assert_eq!(paths.len(), 2, "got {:?}", paths);
        assert!(paths[0].contains("Library/Application Support"));
        assert!(paths[1].contains(".config/Claude"));
    }

    #[test]
    fn test_add_desktop_paths_skips_missing_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut paths = Vec::new();
        add_desktop_paths(&mut paths, tmp.path());
        assert!(paths.is_empty(), "got {:?}", paths);
    }

    #[test]
    fn test_add_fallback_path() {
        let mut paths = Vec::new();
        add_fallback_path(&mut paths, std::path::Path::new("/home/u"));
        assert_eq!(paths, vec!["/home/u/.claude/projects".to_string()]);
    }
}
