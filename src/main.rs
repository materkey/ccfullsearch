use clap::{Parser, Subcommand};
use crossterm::{
    cursor,
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::prelude::*;
use std::io::{self, stdout};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ccs", about = "Claude Code Session Search", version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Enter tree mode for a session file or ID
    #[arg(long)]
    tree: Option<String>,

    /// Overlay mode: resume sessions as child processes and return to TUI after exit
    #[arg(long)]
    overlay: bool,

    /// Message UUID to resume from (for branch-aware resume, used with --tree)
    #[arg(long, requires = "tree")]
    resume_uuid: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Search across Claude Code sessions
    Search {
        /// Search query
        query: String,
        /// Use regex search
        #[arg(long)]
        regex: bool,
        /// Maximum number of results
        #[arg(long, default_value = "100")]
        limit: usize,
    },
    /// List all Claude Code sessions
    List {
        /// Maximum number of results
        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Pick a session interactively and output its info
    Pick {
        /// Optional initial search query
        query: Option<String>,
        /// Write output to file instead of stdout
        #[arg(long)]
        output: Option<String>,
    },
    #[cfg(not(windows))]
    /// Update ccs to the latest version
    Update,
}

/// Read session_id from the first JSON record in a JSONL file.
/// Returns None if the file can't be read or no session_id is found.
fn read_session_id_from_file(file_path: &str) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(file_path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(50).flatten() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(sid) = ccs::session::extract_session_id(&json) {
                return Some(sid);
            }
        }
    }
    None
}

/// Run the TUI event loop and return the outcome.
///
/// Terminal raw mode and alternate screen are always cleaned up, even if the
/// event loop returns an error (e.g. from `event::poll` or `terminal.draw`).
fn run_tui(
    search_paths: Vec<String>,
    tree_target: Option<String>,
    picker_mode: bool,
    initial_query: Option<String>,
) -> io::Result<ccs::tui::TuiOutcome> {
    enable_raw_mode()?;
    if let Err(e) = execute!(
        stdout(),
        EnterAlternateScreen,
        Clear(ClearType::All),
        cursor::Hide
    ) {
        let _ = disable_raw_mode();
        return Err(e);
    }

    let result = run_tui_inner(search_paths, tree_target, picker_mode, initial_query);

    // Always restore terminal, even on error — best-effort cleanup
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), cursor::Show, LeaveAlternateScreen);

    result
}

/// Inner TUI loop, separated so that `run_tui` can guarantee terminal cleanup.
fn run_tui_inner(
    search_paths: Vec<String>,
    tree_target: Option<String>,
    picker_mode: bool,
    initial_query: Option<String>,
) -> io::Result<ccs::tui::TuiOutcome> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let mut app = ccs::tui::App::new(search_paths);
    app.picker_mode = picker_mode;

    // Pre-fill query if provided
    if let Some(q) = initial_query {
        for c in q.chars() {
            app.on_key(c);
        }
    }

    // Enter tree mode if --tree flag was provided
    if let Some(target) = tree_target {
        app.enter_tree_mode_direct(&target);
    }

    // Main loop
    loop {
        if app.needs_full_redraw {
            terminal.clear()?;
            app.needs_full_redraw = false;
        }

        let completed = terminal.draw(|frame| ccs::tui::render(frame, &app.view()))?;
        let fh = completed.area.height as usize;
        app.last_tree_visible_height = if app.tree_mode {
            fh.saturating_sub(3) // header(2) + help(1)
        } else {
            fh.saturating_sub(7) // header(2) + input(3) + status(1) + help(1)
        };

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                let ctx = app.key_context();
                let action = ccs::tui::dispatch::classify_key(key, &ctx);
                app.handle_action(action);
            }
        }

        app.tick();

        if app.should_quit {
            break;
        }
    }

    Ok(app.into_outcome())
}

fn main() -> io::Result<()> {
    // Set panic hook to restore terminal on unexpected crashes
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
        original_hook(info);
    }));

    let cli = Cli::parse();

    // Handle CLI subcommands
    match cli.command {
        Some(Commands::Search {
            query,
            regex,
            limit,
        }) => {
            ccs::cli::cli_search(&query, &ccs::get_search_paths(), regex, limit);
            return Ok(());
        }
        Some(Commands::List { limit }) => {
            ccs::cli::cli_list(&ccs::get_search_paths(), limit);
            return Ok(());
        }
        Some(Commands::Pick { query, output }) => {
            // Remove any stale output file upfront so that every non-success
            // exit path (TUI init error, write failure, cancel) leaves a clean
            // state for callers that reuse the same --output path.
            if let Some(ref path) = output {
                let _ = std::fs::remove_file(path);
            }
            let search_paths = ccs::get_search_paths();
            let outcome = run_tui(search_paths, None, true, query)?;
            match outcome {
                ccs::tui::TuiOutcome::Pick(picked) => {
                    if let Err(e) = picked.write_output(output.as_deref()) {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                    std::process::exit(0);
                }
                _ => {
                    // Cancelled (Esc/Ctrl-C) — output file was already removed upfront
                    std::process::exit(1);
                }
            }
        }
        #[cfg(not(windows))]
        Some(Commands::Update) => {
            if let Err(e) = ccs::update::run() {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
            return Ok(());
        }
        None => {}
    }

    // TUI mode (normal, non-picker)
    let search_paths = ccs::get_search_paths();
    let overlay = cli.overlay;

    // In overlay mode, wrap TUI in a loop: resume as child process, then return to TUI.
    // Without overlay, resume via exec() (replaces this process, no return).
    let mut tree_target = cli.tree;

    // Direct resume: --tree + --resume-uuid skips TUI and resumes from the specified branch.
    // This is used by the Claude Code skill when the picker already captured a branch selection.
    if let (Some(ref target), Some(ref uuid)) = (&tree_target, &cli.resume_uuid) {
        // Resolve target: if it looks like a session ID (not a path), find the actual file
        let resolved_path = if target.contains('/') || target.ends_with(".jsonl") {
            if !std::path::Path::new(target).exists() {
                eprintln!("Session file not found: {}", target);
                std::process::exit(1);
            }
            target.clone()
        } else {
            ccs::session::find_session_file_in_paths(target, &search_paths).unwrap_or_else(|| {
                eprintln!("Session not found: {}", target);
                std::process::exit(1);
            })
        };
        // Read session_id from file content rather than filename, so that
        // resolve_parent_session can correctly redirect auxiliary/agent files.
        let session_id = read_session_id_from_file(&resolved_path).unwrap_or_else(|| {
            std::path::Path::new(resolved_path.as_str())
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });
        let source = ccs::session::SessionSource::from_path(&resolved_path);
        let file_path = &resolved_path;

        if overlay {
            if let Err(e) =
                ccs::resume::resume_child(&session_id, file_path, source, Some(uuid.as_str()))
            {
                eprintln!("Error resuming session: {}", e);
                std::process::exit(1);
            }
            // Fast-path resume is a one-shot action (skill already picked the branch).
            // Exit so the overlay popup closes and control returns to the caller.
            return Ok(());
        } else {
            if let Err(e) = ccs::resume::resume(&session_id, file_path, source, Some(uuid.as_str()))
            {
                eprintln!("Error resuming session: {}", e);
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    let mut restore_query: Option<String> = None;
    loop {
        let initial_query = restore_query
            .take()
            .or_else(|| tree_target.as_ref().map(|_| String::new()));
        let outcome = run_tui(
            search_paths.clone(),
            tree_target.take(),
            false,
            initial_query,
        )?;

        match outcome {
            ccs::tui::TuiOutcome::Resume {
                session_id,
                file_path,
                source,
                uuid,
                query,
            } => {
                if overlay {
                    // Save query so it's restored when we loop back to TUI
                    if !query.is_empty() {
                        restore_query = Some(query);
                    }
                    if let Err(e) =
                        ccs::resume::resume_child(&session_id, &file_path, source, uuid.as_deref())
                    {
                        eprintln!("Error resuming session: {}", e);
                    }
                    // Loop back to TUI
                    continue;
                } else {
                    // exec() — replaces process, no return on success
                    if let Err(e) =
                        ccs::resume::resume(&session_id, &file_path, source, uuid.as_deref())
                    {
                        eprintln!("Error resuming session: {}", e);
                        std::process::exit(1);
                    }
                    // On Unix exec() replaces the process so we never reach here.
                    // On non-Unix, exec_command spawns and waits, then returns Ok.
                    break;
                }
            }
            _ => {
                // Quit
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parses_overlay_flag() {
        let cli = Cli::parse_from(["ccs", "--overlay"]);
        assert!(cli.overlay);
    }

    #[test]
    fn test_cli_no_overlay_by_default() {
        let cli = Cli::parse_from(["ccs"]);
        assert!(!cli.overlay);
    }

    #[test]
    fn test_cli_overlay_with_tree() {
        let cli = Cli::parse_from(["ccs", "--overlay", "--tree", "some-id"]);
        assert!(cli.overlay);
        assert_eq!(cli.tree.as_deref(), Some("some-id"));
    }

    #[test]
    fn test_cli_resume_uuid_flag() {
        let cli = Cli::parse_from([
            "ccs",
            "--overlay",
            "--tree",
            "/path/to/session.jsonl",
            "--resume-uuid",
            "abc-123",
        ]);
        assert!(cli.overlay);
        assert_eq!(cli.tree.as_deref(), Some("/path/to/session.jsonl"));
        assert_eq!(cli.resume_uuid.as_deref(), Some("abc-123"));
    }

    #[test]
    fn test_cli_resume_uuid_without_tree_is_error() {
        let result = Cli::try_parse_from(["ccs", "--resume-uuid", "abc-123"]);
        assert!(
            result.is_err(),
            "--resume-uuid without --tree should be a parse error"
        );
    }

    /// Simulates the overlay loop decision: Resume outcome + overlay=true
    /// should signal "continue" (return true), while overlay=false should
    /// signal "break after resume" (return false).
    fn should_loop_back(outcome: &ccs::tui::TuiOutcome, overlay: bool) -> bool {
        matches!(outcome, ccs::tui::TuiOutcome::Resume { .. }) && overlay
    }

    #[test]
    fn test_overlay_resume_loops_back() {
        let outcome = ccs::tui::TuiOutcome::Resume {
            session_id: "test-id".to_string(),
            file_path: "/tmp/test.jsonl".to_string(),
            source: ccs::session::SessionSource::ClaudeCodeCLI,
            uuid: None,
            query: String::new(),
        };
        assert!(should_loop_back(&outcome, true));
    }

    #[test]
    fn test_no_overlay_resume_does_not_loop() {
        let outcome = ccs::tui::TuiOutcome::Resume {
            session_id: "test-id".to_string(),
            file_path: "/tmp/test.jsonl".to_string(),
            source: ccs::session::SessionSource::ClaudeCodeCLI,
            uuid: None,
            query: String::new(),
        };
        assert!(!should_loop_back(&outcome, false));
    }

    #[test]
    fn test_quit_outcome_does_not_loop() {
        let outcome = ccs::tui::TuiOutcome::Quit;
        assert!(!should_loop_back(&outcome, true));
        assert!(!should_loop_back(&outcome, false));
    }
}
