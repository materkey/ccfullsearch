pub mod cli;
pub mod resume;
pub mod search;
pub mod session;
pub mod tree;
pub mod tui;

pub fn get_search_paths() -> Vec<String> {
    let mut search_paths = Vec::new();

    if let Ok(custom_path) = std::env::var("CCFS_SEARCH_PATH") {
        search_paths.push(custom_path);
    } else {
        // Claude Code CLI sessions
        if let Some(cli_path) = dirs::home_dir()
            .map(|h| h.join(".claude").join("projects"))
            .and_then(|p| p.to_str().map(|s| s.to_string()))
        {
            search_paths.push(cli_path);
        }

        // Claude Desktop sessions (macOS)
        if let Some(desktop_path) = dirs::home_dir()
            .map(|h| h.join("Library/Application Support/Claude/local-agent-mode-sessions"))
            .and_then(|p| p.to_str().map(|s| s.to_string()))
        {
            search_paths.push(desktop_path);
        }

        // Fallback if no paths found
        if search_paths.is_empty() {
            search_paths.push("~/.claude/projects".to_string());
        }
    }

    search_paths
}
