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
        /// Output full message content instead of a snippet around the match
        #[arg(long)]
        full_content: bool,
    },
    /// List all Claude Code sessions
    List {
        /// Maximum number of results
        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Show messages around a search hit (drill-down for `ccs search` results)
    Show {
        /// Session file path (file_path from search output)
        file_path: String,
        /// 1-based JSONL line of the target message (line_number from search output)
        #[arg(long)]
        line: Option<usize>,
        /// Target message UUID (message_uuid from search output)
        #[arg(long)]
        uuid: Option<String>,
        /// Messages of context before and after the target
        #[arg(long, default_value = "3")]
        context: usize,
        /// Maximum characters of content per message
        #[arg(long, default_value = "2000")]
        max_chars: usize,
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

/// A `--tree` target is either a filesystem path or a bare session ID.
fn is_path_like(target: &str) -> bool {
    target.contains('/') || target.ends_with(".jsonl")
}

/// Resolve a `--tree` target to a session file path: paths are used as-is
/// (after an existence check), bare session IDs are searched for.
fn resolve_tree_target(target: &str, search_paths: &[String]) -> Result<String, String> {
    if is_path_like(target) {
        if !std::path::Path::new(target).exists() {
            return Err(format!("Session file not found: {}", target));
        }
        return Ok(target.to_string());
    }
    ccs::session::find_session_file_in_paths(target, search_paths)
        .ok_or_else(|| format!("Session not found: {}", target))
}

fn resolve_tree_target_or_exit(target: &str, search_paths: &[String]) -> String {
    resolve_tree_target(target, search_paths).unwrap_or_else(|msg| {
        eprintln!("{}", msg);
        std::process::exit(1);
    })
}

/// Filename stem of a session file, or "unknown" when it has none.
fn file_stem_or_unknown(file_path: &str) -> String {
    std::path::Path::new(file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Session ID for resume. Read from file content rather than filename, so
/// that resolve_parent_session can correctly redirect auxiliary/agent files;
/// falls back to the filename stem.
fn session_id_for_resume(file_path: &str) -> String {
    read_session_id_from_file(file_path).unwrap_or_else(|| file_stem_or_unknown(file_path))
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

    let mut app = init_app(search_paths, tree_target, picker_mode, initial_query);

    let result = run_event_loop(&mut terminal, &mut app);
    result.map(|()| app.into_outcome())
}

/// Build the initial App state: picker mode, pre-filled query, tree mode.
fn init_app(
    search_paths: Vec<String>,
    tree_target: Option<String>,
    picker_mode: bool,
    initial_query: Option<String>,
) -> ccs::tui::App {
    let mut app = ccs::tui::App::new(search_paths);
    app.picker_mode = picker_mode;

    prefill_query(&mut app, initial_query);

    // Enter tree mode if --tree flag was provided
    if let Some(target) = tree_target {
        app.enter_tree_mode_direct(&target);
    }

    app
}

/// Type an initial query into the app as if the user had entered it.
fn prefill_query(app: &mut ccs::tui::App, initial_query: Option<String>) {
    if let Some(q) = initial_query {
        for c in q.chars() {
            app.on_key(c);
        }
    }
}

/// Main draw/input/tick loop; runs until the app requests quit.
fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut ccs::tui::App,
) -> io::Result<()> {
    while !app.should_quit {
        tick_once(terminal, app)?;
    }
    Ok(())
}

/// One iteration of the event loop: draw, handle input, poll background work.
fn tick_once(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut ccs::tui::App,
) -> io::Result<()> {
    draw_frame(terminal, app)?;
    poll_and_handle_key(app)?;
    app.tick();
    Ok(())
}

/// Clear the terminal when a full redraw was requested, then render a frame.
fn draw_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut ccs::tui::App,
) -> io::Result<()> {
    if app.needs_full_redraw {
        terminal.clear()?;
        app.needs_full_redraw = false;
    }
    render_frame(terminal, app)
}

/// Render one frame and record the visible list height for scroll math.
fn render_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut ccs::tui::App,
) -> io::Result<()> {
    let completed = terminal.draw(|frame| ccs::tui::render(frame, &app.view()))?;
    app.last_tree_visible_height = visible_height(app.tree_mode, completed.area.height as usize);
    Ok(())
}

/// Rows available for list content: frame height minus fixed chrome.
fn visible_height(tree_mode: bool, frame_height: usize) -> usize {
    if tree_mode {
        frame_height.saturating_sub(3) // header(2) + help(1)
    } else {
        frame_height.saturating_sub(7) // header(2) + input(3) + status(1) + help(1)
    }
}

/// Poll for a terminal event with a 100 ms timeout.
fn poll_event() -> io::Result<Option<Event>> {
    if event::poll(Duration::from_millis(100))? {
        return event::read().map(Some);
    }
    Ok(None)
}

/// Poll for one terminal event and dispatch it to the app.
fn poll_and_handle_key(app: &mut ccs::tui::App) -> io::Result<()> {
    if let Some(event) = poll_event()? {
        handle_event(app, event);
    }
    Ok(())
}

/// Dispatch a key-press event through `classify_key` to the app; non-press
/// key events (release/repeat) and non-key events are ignored.
fn handle_event(app: &mut ccs::tui::App, event: Event) {
    if let Event::Key(key) = event {
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctx = app.key_context();
        let action = ccs::tui::dispatch::classify_key(key, &ctx);
        app.handle_action(action);
    }
}

fn main() -> io::Result<()> {
    install_panic_hook();

    let cli = Cli::parse();

    match cli.command {
        Some(cmd) => run_subcommand(cmd),
        None => run_tui_mode(cli.tree, cli.resume_uuid, cli.overlay),
    }
}

/// Set panic hook to restore terminal on unexpected crashes.
fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
        original_hook(info);
    }));
}

/// Handle CLI subcommands (non-TUI entry points, plus the pick picker).
fn run_subcommand(cmd: Commands) -> io::Result<()> {
    match cmd {
        Commands::Search {
            query,
            regex,
            limit,
            full_content,
        } => {
            ccs::cli::cli_search(&query, &ccs::get_search_paths(), regex, limit, full_content);
            Ok(())
        }
        Commands::List { limit } => {
            ccs::cli::cli_list(&ccs::get_search_paths(), limit);
            Ok(())
        }
        Commands::Show {
            file_path,
            line,
            uuid,
            context,
            max_chars,
        } => {
            ccs::cli::cli_show(&file_path, line, uuid.as_deref(), context, max_chars);
            Ok(())
        }
        Commands::Pick { query, output } => run_pick(query, output),
        #[cfg(not(windows))]
        Commands::Update => run_update(),
    }
}

/// Handle `ccs pick`: run the TUI in picker mode and exit with the pick result.
fn run_pick(query: Option<String>, output: Option<String>) -> io::Result<()> {
    // Remove any stale output file upfront so that every non-success
    // exit path (TUI init error, write failure, cancel) leaves a clean
    // state for callers that reuse the same --output path.
    remove_stale_output(output.as_deref());
    let outcome = run_tui(ccs::get_search_paths(), None, true, query)?;
    std::process::exit(pick_exit_code(outcome, output.as_deref()));
}

fn remove_stale_output(path: Option<&str>) {
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}

/// Exit code for `ccs pick`: 0 when a session was picked and written, 1 on
/// write failure or cancel (Esc/Ctrl-C — output file was already removed upfront).
fn pick_exit_code(outcome: ccs::tui::TuiOutcome, output: Option<&str>) -> i32 {
    if let ccs::tui::TuiOutcome::Pick(picked) = outcome {
        if let Err(e) = picked.write_output(output) {
            eprintln!("Error: {}", e);
            return 1;
        }
        return 0;
    }
    1
}

#[cfg(not(windows))]
fn run_update() -> io::Result<()> {
    if let Err(e) = ccs::update::run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
    Ok(())
}

/// TUI mode (no subcommand): direct resume when --tree + --resume-uuid are
/// given, otherwise the interactive TUI loop.
fn run_tui_mode(
    tree_target: Option<String>,
    resume_uuid: Option<String>,
    overlay: bool,
) -> io::Result<()> {
    let search_paths = ccs::get_search_paths();

    // Direct resume: --tree + --resume-uuid skips TUI and resumes from the specified branch.
    // This is used by the Claude Code skill when the picker already captured a branch selection.
    if let (Some(target), Some(uuid)) = (&tree_target, &resume_uuid) {
        return run_direct_resume(target, uuid, &search_paths, overlay);
    }

    run_tui_loop(search_paths, tree_target, overlay)
}

/// Resume a session directly (no TUI) from the given `--tree` target and
/// message UUID. In overlay mode this is a one-shot action (the skill already
/// picked the branch): resume as child, then exit so the overlay popup closes
/// and control returns to the caller.
fn run_direct_resume(
    target: &str,
    uuid: &str,
    search_paths: &[String],
    overlay: bool,
) -> io::Result<()> {
    let file_path = resolve_tree_target_or_exit(target, search_paths);
    let session_id = session_id_for_resume(&file_path);
    let source = ccs::session::SessionSource::from_path(&file_path);

    let result = if overlay {
        ccs::resume::resume_child(&session_id, &file_path, source, Some(uuid))
    } else {
        ccs::resume::resume(&session_id, &file_path, source, Some(uuid))
    };
    if let Err(e) = result {
        eprintln!("Error resuming session: {}", e);
        std::process::exit(1);
    }
    Ok(())
}

/// Normal TUI mode. In overlay mode, wrap TUI in a loop: resume as child
/// process, then return to TUI. Without overlay, resume via exec() (replaces
/// this process, no return).
fn run_tui_loop(
    search_paths: Vec<String>,
    mut tree_target: Option<String>,
    overlay: bool,
) -> io::Result<()> {
    let mut restore_query: Option<String> = None;
    let mut keep_running = true;
    while keep_running {
        keep_running = run_tui_once(&search_paths, &mut tree_target, overlay, &mut restore_query)?;
    }
    Ok(())
}

/// Run one TUI pass and handle its outcome. Returns `true` when the loop
/// should return to the TUI (overlay resume), `false` to stop.
fn run_tui_once(
    search_paths: &[String],
    tree_target: &mut Option<String>,
    overlay: bool,
    restore_query: &mut Option<String>,
) -> io::Result<bool> {
    let initial_query = restore_query
        .take()
        .or_else(|| tree_target.as_ref().map(|_| String::new()));
    let outcome = run_tui(
        search_paths.to_vec(),
        tree_target.take(),
        false,
        initial_query,
    )?;
    Ok(handle_outcome(outcome, overlay, restore_query))
}

/// Decide what to do after the TUI exits. Returns `true` to loop back into
/// the TUI (overlay resume), `false` to stop (quit, or exec()-style resume).
fn handle_outcome(
    outcome: ccs::tui::TuiOutcome,
    overlay: bool,
    restore_query: &mut Option<String>,
) -> bool {
    match outcome {
        ccs::tui::TuiOutcome::Resume {
            session_id,
            file_path,
            source,
            uuid,
            query,
        } => resume_outcome(
            &session_id,
            &file_path,
            source,
            uuid.as_deref(),
            query,
            overlay,
            restore_query,
        ),
        _ => {
            // Quit
            false
        }
    }
}

/// Perform the resume side effect for a `Resume` outcome and report whether
/// the TUI loop should continue (overlay mode only).
fn resume_outcome(
    session_id: &str,
    file_path: &str,
    source: ccs::session::SessionSource,
    uuid: Option<&str>,
    query: String,
    overlay: bool,
    restore_query: &mut Option<String>,
) -> bool {
    if !overlay {
        resume_exec(session_id, file_path, source, uuid);
        return false;
    }
    // Save query so it's restored when we loop back to TUI
    *restore_query = saved_query(query);
    resume_in_overlay(session_id, file_path, source, uuid);
    true
}

/// Query to restore on the next TUI pass: empty queries are not preserved.
fn saved_query(query: String) -> Option<String> {
    if query.is_empty() {
        None
    } else {
        Some(query)
    }
}

/// Resume a session as a child process (overlay mode) — errors are reported
/// but the TUI loop continues.
fn resume_in_overlay(
    session_id: &str,
    file_path: &str,
    source: ccs::session::SessionSource,
    uuid: Option<&str>,
) {
    if let Err(e) = ccs::resume::resume_child(session_id, file_path, source, uuid) {
        eprintln!("Error resuming session: {}", e);
    }
}

/// Resume a session via exec(). On Unix exec() replaces the process so this
/// never returns on success. On non-Unix, exec_command spawns and waits,
/// then returns.
fn resume_exec(
    session_id: &str,
    file_path: &str,
    source: ccs::session::SessionSource,
    uuid: Option<&str>,
) {
    if let Err(e) = ccs::resume::resume(session_id, file_path, source, uuid) {
        eprintln!("Error resuming session: {}", e);
        std::process::exit(1);
    }
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
            source: ccs::session::SessionSource::CLI,
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
            source: ccs::session::SessionSource::CLI,
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

    #[test]
    fn test_handle_outcome_quit_stops_loop() {
        let mut restore_query = None;
        assert!(!handle_outcome(
            ccs::tui::TuiOutcome::Quit,
            true,
            &mut restore_query
        ));
        assert!(restore_query.is_none());
    }

    #[test]
    fn test_visible_height_tree_mode() {
        assert_eq!(visible_height(true, 20), 17);
        assert_eq!(visible_height(true, 2), 0);
    }

    #[test]
    fn test_visible_height_search_mode() {
        assert_eq!(visible_height(false, 20), 13);
        assert_eq!(visible_height(false, 5), 0);
    }

    #[test]
    fn test_is_path_like() {
        assert!(is_path_like("/abs/path"));
        assert!(is_path_like("rel/path"));
        assert!(is_path_like("session.jsonl"));
        assert!(!is_path_like("abc-123"));
    }

    #[test]
    fn test_saved_query_empty_is_none() {
        assert_eq!(saved_query(String::new()), None);
    }

    #[test]
    fn test_saved_query_non_empty_is_preserved() {
        assert_eq!(saved_query("hello".to_string()), Some("hello".to_string()));
    }

    #[test]
    fn test_remove_stale_output_removes_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("out.txt");
        std::fs::write(&path, "stale").unwrap();
        remove_stale_output(Some(path.to_str().unwrap()));
        assert!(!path.exists());
    }

    #[test]
    fn test_remove_stale_output_none_is_noop() {
        remove_stale_output(None);
    }

    fn write_session_file(dir: &std::path::Path, name: &str, lines: &[&str]) -> String {
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n")).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn test_read_session_id_from_file_cli_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_session_file(
            dir.path(),
            "session.jsonl",
            &[r#"{"sessionId":"abc-123","type":"user"}"#],
        );
        assert_eq!(
            read_session_id_from_file(&path),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn test_read_session_id_skips_lines_without_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_session_file(
            dir.path(),
            "session.jsonl",
            &[
                "not json at all",
                r#"{"type":"summary"}"#,
                r#"{"session_id":"desktop-42"}"#,
            ],
        );
        assert_eq!(
            read_session_id_from_file(&path),
            Some("desktop-42".to_string())
        );
    }

    #[test]
    fn test_read_session_id_missing_file_is_none() {
        assert_eq!(
            read_session_id_from_file("/nonexistent/dir/session.jsonl"),
            None
        );
    }

    #[test]
    fn test_read_session_id_no_session_id_is_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_session_file(dir.path(), "session.jsonl", &[r#"{"type":"summary"}"#]);
        assert_eq!(read_session_id_from_file(&path), None);
    }

    #[test]
    fn test_file_stem_or_unknown() {
        assert_eq!(file_stem_or_unknown("/a/b/sess-1.jsonl"), "sess-1");
        assert_eq!(file_stem_or_unknown(""), "unknown");
    }

    #[test]
    fn test_session_id_for_resume_prefers_file_content() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_session_file(dir.path(), "outer.jsonl", &[r#"{"sessionId":"inner-id"}"#]);
        assert_eq!(session_id_for_resume(&path), "inner-id");
    }

    #[test]
    fn test_session_id_for_resume_falls_back_to_stem() {
        assert_eq!(
            session_id_for_resume("/nonexistent/dir/fallback-id.jsonl"),
            "fallback-id"
        );
    }

    #[test]
    fn test_resolve_tree_target_existing_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_session_file(dir.path(), "sess.jsonl", &[r#"{"sessionId":"x"}"#]);
        assert_eq!(resolve_tree_target(&path, &[]), Ok(path));
    }

    #[test]
    fn test_resolve_tree_target_missing_path() {
        let err = resolve_tree_target("/nonexistent/dir/sess.jsonl", &[]).unwrap_err();
        assert!(err.contains("Session file not found"), "got: {}", err);
    }

    #[test]
    fn test_resolve_tree_target_by_session_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_session_file(dir.path(), "sess-42.jsonl", &[r#"{"sessionId":"sess-42"}"#]);
        let search_paths = vec![dir.path().to_str().unwrap().to_string()];
        assert_eq!(resolve_tree_target("sess-42", &search_paths), Ok(path));
    }

    #[test]
    fn test_resolve_tree_target_unknown_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let search_paths = vec![dir.path().to_str().unwrap().to_string()];
        let err = resolve_tree_target("nope", &search_paths).unwrap_err();
        assert!(err.contains("Session not found"), "got: {}", err);
    }

    fn sample_picked_session() -> ccs::tui::PickedSession {
        ccs::tui::PickedSession {
            session_id: "sess-1".to_string(),
            file_path: "/tmp/sess-1.jsonl".to_string(),
            source: ccs::session::SessionSource::CLI,
            project: "my-project".to_string(),
            message_uuid: None,
        }
    }

    #[test]
    fn test_pick_exit_code_writes_output_and_returns_zero() {
        let dir = tempfile::TempDir::new().unwrap();
        let out = dir.path().join("picked.txt");
        let outcome = ccs::tui::TuiOutcome::Pick(sample_picked_session());
        assert_eq!(pick_exit_code(outcome, out.to_str()), 0);
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("session_id: sess-1"));
    }

    #[test]
    fn test_pick_exit_code_write_failure_returns_one() {
        let outcome = ccs::tui::TuiOutcome::Pick(sample_picked_session());
        assert_eq!(
            pick_exit_code(outcome, Some("/nonexistent/dir/picked.txt")),
            1
        );
    }

    #[test]
    fn test_pick_exit_code_quit_returns_one() {
        assert_eq!(pick_exit_code(ccs::tui::TuiOutcome::Quit, None), 1);
    }
}
