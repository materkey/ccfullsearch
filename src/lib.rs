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

pub mod agent_search;
pub mod cli;
pub mod config;
pub mod dag;
pub mod project_match;
pub mod recent;
pub mod remote;
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
        if path.exists()
            && let Some(p) = path.to_str().map(|s| s.to_string())
        {
            search_paths.push(p);
        }
    }
}

/// Claude Desktop sessions (macOS and Linux locations).
fn add_desktop_paths(search_paths: &mut Vec<String>, home: &std::path::Path) {
    // Claude Desktop sessions (macOS)
    let macos_desktop = home.join("Library/Application Support/Claude/local-agent-mode-sessions");
    if macos_desktop.exists()
        && let Some(p) = macos_desktop.to_str().map(|s| s.to_string())
    {
        search_paths.push(p);
    }

    // Claude Desktop sessions (Linux)
    let linux_desktop = home.join(".config/Claude/local-agent-mode-sessions");
    if linux_desktop.exists()
        && let Some(p) = linux_desktop.to_str().map(|s| s.to_string())
    {
        search_paths.push(p);
    }
}

/// Opencode SQLite database. The search layer dispatches paths
/// ending in `opencode.db` to a SQL-based scanner instead of ripgrep.
fn add_opencode_path(search_paths: &mut Vec<String>) {
    if let Some(db) = session::opencode::opencode_database_path()
        && let Some(p) = db.to_str().map(|s| s.to_string())
    {
        search_paths.push(p);
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

    // These tests call the parameterized helpers, so they don't touch the
    // process environment. That `get_search_paths` reads the right variables
    // is checked through a child `ccs` in tests/cli_list.rs.

    #[test]
    fn test_build_search_paths_respects_claude_config_dir() {
        let home = tempfile::TempDir::new().unwrap();
        let config_dir = home.path().join("custom-claude");

        let paths = build_search_paths(
            None,
            Some(config_dir.to_str().unwrap().to_string()),
            None,
            Some(home.path().to_path_buf()),
        );

        // Should use CLAUDE_CONFIG_DIR as base for projects/
        let expected = config_dir.join("projects");
        assert!(
            paths.iter().any(|p| p == expected.to_str().unwrap()),
            "Expected {:?} in {:?}",
            expected,
            paths
        );
    }

    #[test]
    fn test_build_search_paths_includes_codex_home_sessions() {
        let home = tempfile::TempDir::new().unwrap();
        let codex = tempfile::TempDir::new().unwrap();
        let sessions = codex.path().join("sessions");
        let archived = codex.path().join("archived_sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&archived).unwrap();

        let paths = build_search_paths(
            None,
            None,
            Some(codex.path().to_str().unwrap().to_string()),
            Some(home.path().to_path_buf()),
        );

        assert!(paths.iter().any(|p| p == sessions.to_str().unwrap()));
        assert!(paths.iter().any(|p| p == archived.to_str().unwrap()));
    }

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
