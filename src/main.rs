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
use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};

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
        /// Hosts to search: all, local, remote or a host name (repeatable)
        #[arg(long, default_values_t = [ccs::cli::CLI_DEFAULT_HOSTS.to_string()])]
        host: Vec<String>,
    },
    /// List all Claude Code sessions
    List {
        /// Maximum number of results
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Hosts to list: all, local, remote or a host name (repeatable)
        #[arg(long, default_values_t = [ccs::cli::CLI_DEFAULT_HOSTS.to_string()])]
        host: Vec<String>,
    },
    /// Show messages around a search hit (drill-down for `ccs search` results),
    /// or the end of a session when no --line or --uuid is given
    Show {
        /// Session file path (file_path from search output) or session id
        target: String,
        /// 1-based JSONL line of the target message (line_number from search output)
        #[arg(long)]
        line: Option<usize>,
        /// Target message UUID (message_uuid from search output)
        #[arg(long)]
        uuid: Option<String>,
        /// Last N messages of the session's current line (default 10 when no --line/--uuid)
        #[arg(long, conflicts_with_all = ["line", "uuid", "context"])]
        tail: Option<usize>,
        /// Messages of context before and after the target
        #[arg(long, default_value = "3")]
        context: usize,
        /// Maximum characters of content per message
        #[arg(long, default_value = "2000")]
        max_chars: usize,
        /// Host the file is on (the host field of search output); local by default
        #[arg(long, default_value = "local")]
        host: String,
    },
    /// Pick a session interactively and output its info
    Pick {
        /// Optional initial search query
        query: Option<String>,
        /// Write output to file instead of stdout
        #[arg(long)]
        output: Option<String>,
        /// Hosts to pick from: all, local, remote or a host name (repeatable)
        #[arg(long, default_values_t = [ccs::cli::CLI_DEFAULT_HOSTS.to_string()])]
        host: Vec<String>,
    },
    #[cfg(not(windows))]
    /// Update ccs to the latest version
    Update,
    /// Resume a session file on this machine, or on a remote host with --host
    Resume {
        /// Session file path (file_path from search or pick output)
        #[arg(long)]
        file: String,
        /// Message UUID to resume from (branch-aware resume)
        #[arg(long)]
        uuid: Option<String>,
        /// Session id; read from the file when absent (needed for Opencode `<db>#<id>` paths)
        #[arg(long)]
        session_id: Option<String>,
        /// Remote host from config.toml [[hosts]] / CCS_HOSTS; the session runs there over ssh
        #[arg(long)]
        host: Option<String>,
    },
    /// Answer remote-host requests on stdin/stdout (run by another ccs over ssh)
    #[command(hide = true)]
    Serve {
        /// Speak the line-delimited JSON protocol on stdin/stdout
        #[arg(long)]
        stdio: bool,
    },
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

// A `--tree` target is either a filesystem path or a bare session ID, the
// same rule as `ccs show`.
use ccs::cli::is_path_like;

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
fn run_tui(app: &mut ccs::tui::App) -> io::Result<ccs::tui::TuiOutcome> {
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

    let result = run_tui_inner(app);

    // Always restore terminal, even on error — best-effort cleanup
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), cursor::Show, LeaveAlternateScreen);

    result
}

/// Inner TUI loop, separated so that `run_tui` can guarantee terminal cleanup.
fn run_tui_inner(app: &mut ccs::tui::App) -> io::Result<ccs::tui::TuiOutcome> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let result = run_event_loop(&mut terminal, app);
    result.map(|()| app.take_outcome())
}

/// Build the initial App state: picker mode, pre-filled query, tree mode.
fn init_app(
    search_paths: Vec<String>,
    tree_target: Option<String>,
    picker_mode: bool,
    initial_query: Option<String>,
) -> ccs::tui::App {
    let selection = ccs::remote::HostSelection {
        local: true,
        remote: remote_hosts(),
    };
    init_app_with_hosts(
        search_paths,
        tree_target,
        picker_mode,
        initial_query,
        selection,
    )
}

fn init_app_with_hosts(
    search_paths: Vec<String>,
    tree_target: Option<String>,
    picker_mode: bool,
    initial_query: Option<String>,
    selection: ccs::remote::HostSelection,
) -> ccs::tui::App {
    let mut app = ccs::tui::App::new(search_paths).with_remote_hosts(selection.remote);
    if !selection.local {
        app.hosts.filter = ccs::tui::hosts_state::HostFilter::Remote;
    }
    app.picker_mode = picker_mode;

    prefill_query(&mut app, initial_query);

    // Enter tree mode if --tree flag was provided
    if let Some(target) = tree_target {
        app.enter_tree_mode_direct(&target);
    }

    app
}

/// Remote hosts from `[[hosts]]` in `config.toml` / `CCS_HOSTS`. A broken config is reported
/// and leaves the picker local-only rather than refusing to start.
fn remote_hosts() -> Vec<ccs::remote::config::HostConfig> {
    ccs::remote::config::load_hosts().unwrap_or_else(|e| {
        eprintln!("ccs: {e}; remote hosts disabled");
        Vec::new()
    })
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
    let cli = Cli::parse();
    // `serve` speaks a protocol on stdout; the hook's terminal escapes
    // would corrupt it, and it never draws a TUI.
    if !matches!(cli.command, Some(Commands::Serve { .. })) {
        install_panic_hook();
    }

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
            host,
        } => {
            let selection = select_hosts_or_exit(&host);
            ccs::cli::cli_search(
                &query,
                &ccs::get_search_paths(),
                regex,
                limit,
                full_content,
                &selection,
            );
            Ok(())
        }
        Commands::List { limit, host } => {
            let selection = select_hosts_or_exit(&host);
            ccs::cli::cli_list(&ccs::get_search_paths(), limit, &selection);
            Ok(())
        }
        Commands::Show {
            target,
            line,
            uuid,
            tail,
            context,
            max_chars,
            host,
        } => {
            let remote = (host != "local").then(|| {
                find_host(&host).unwrap_or_else(|e| {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                })
            });
            ccs::cli::cli_show(
                &target,
                line,
                uuid.as_deref(),
                tail,
                context,
                max_chars,
                remote.as_ref(),
                &ccs::get_search_paths(),
            );
            Ok(())
        }
        Commands::Pick {
            query,
            output,
            host,
        } => run_pick(query, output, &host),
        Commands::Serve { stdio } => run_serve(stdio),
        Commands::Resume {
            file,
            uuid,
            session_id,
            host,
        } => run_resume(
            &file,
            uuid.as_deref(),
            session_id.as_deref(),
            host.as_deref(),
        ),
        #[cfg(not(windows))]
        Commands::Update => run_update(),
    }
}

/// Handle `ccs resume`: run the session here (what a remote ccs does at the
/// other end of `ssh -t`), or hop to `--host`. Never waits for Enter: the
/// ccs that started this one decides what to show.
fn run_resume(
    file: &str,
    uuid: Option<&str>,
    session_id: Option<&str>,
    host: Option<&str>,
) -> io::Result<()> {
    let source = ccs::session::SessionSource::from_path(file);
    let result = match host {
        Some(name) => {
            find_host(name).and_then(|h| launch_remote_resume(&h, session_id, file, source, uuid))
        }
        None => ccs::resume::resume(&resume_session_id(file, session_id), file, source, uuid),
    };
    if let Err(e) = result {
        eprintln!("Error resuming session: {e}");
        std::process::exit(1);
    }
    Ok(())
}

/// The id the caller passed, else the one in the file.
fn resume_session_id(file: &str, given: Option<&str>) -> String {
    given
        .map(str::to_string)
        .unwrap_or_else(|| session_id_for_resume(file))
}

fn select_hosts_or_exit(values: &[String]) -> ccs::remote::HostSelection {
    ccs::remote::select_hosts(values, &remote_hosts()).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(2);
    })
}

fn find_host(name: &str) -> Result<ccs::remote::config::HostConfig, String> {
    remote_hosts()
        .into_iter()
        .find(|h| h.name == name)
        .ok_or_else(|| {
            format!("unknown host {name} (not in [[hosts]] of config.toml or CCS_HOSTS)")
        })
}

/// Resume on a remote host. Claude Desktop opens an app on that machine's
/// screen, which is not this terminal.
fn launch_remote_resume(
    host: &ccs::remote::config::HostConfig,
    session_id: Option<&str>,
    file_path: &str,
    source: ccs::session::SessionSource,
    uuid: Option<&str>,
) -> Result<(), String> {
    if source == ccs::session::SessionSource::ClaudeDesktop {
        return Err(format!(
            "{}: Claude Desktop-сессию с другой машины не открыть",
            host.name
        ));
    }
    ccs::resume::launcher::resume_remote_child(host, session_id, file_path, uuid)
}

/// Handle `ccs serve --stdio`, the remote end another ccs reaches over ssh.
fn run_serve(stdio: bool) -> io::Result<()> {
    if !stdio {
        eprintln!("ccs serve: only --stdio is supported");
        std::process::exit(2);
    }
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    let code = ccs::remote::serve::run_stdio(
        io::stdin().lock(),
        io::stdout(),
        ccs::get_search_paths(),
        home,
    );
    std::process::exit(code);
}

/// Handle `ccs pick`: run the TUI in picker mode and exit with the pick result.
fn run_pick(query: Option<String>, output: Option<String>, host: &[String]) -> io::Result<()> {
    // Remove any stale output file upfront so that every non-success
    // exit path (TUI init error, write failure, cancel) leaves a clean
    // state for callers that reuse the same --output path.
    remove_stale_output(output.as_deref());
    let selection = select_hosts_or_exit(host);
    let mut app = init_app_with_hosts(ccs::get_search_paths(), None, true, query, selection);
    let outcome = run_tui(&mut app)?;
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

    if let Err(e) = launch_resume(&session_id, &file_path, source, Some(uuid), overlay) {
        eprintln!("Error resuming session: {}", e);
        if overlay {
            wait_for_enter("Press Enter to close: ");
        }
        std::process::exit(1);
    }
    Ok(())
}

/// Return to the TUI after an overlay session, a quick exit from a resumed
/// session, or a failed launch. The same app comes back each time, so the
/// picker reopens in the state it was left in.
fn run_tui_loop(
    search_paths: Vec<String>,
    tree_target: Option<String>,
    overlay: bool,
) -> io::Result<()> {
    let mut app = init_app(search_paths, tree_target, false, None);
    while handle_outcome(run_tui(&mut app)?, overlay, &mut app) {
        discard_pending_events();
    }
    Ok(())
}

/// Drop key events crossterm read ahead before the session took the terminal.
/// A double-tapped Enter arrives in one read, and the second one would
/// reopen the same session as soon as the picker comes back. A failed poll
/// or read only ends the drain early.
fn discard_pending_events() {
    while matches!(event::poll(Duration::ZERO), Ok(true)) && event::read().is_ok() {}
}

/// What follows a resume attempt.
enum AfterResume {
    /// The resume is over, or failed with nothing to return to.
    Exit,
    /// Back to the picker; `launched` is false when the launch failed.
    Picker { launched: bool },
}

/// Decide what to do after the TUI exits. Returns `true` to loop back into
/// the same app (overlay resume, quick exit or failed launch), `false` to stop
/// (quit, or a resume that is over).
fn handle_outcome(outcome: ccs::tui::TuiOutcome, overlay: bool, app: &mut ccs::tui::App) -> bool {
    match outcome {
        ccs::tui::TuiOutcome::Resume {
            host,
            session_id,
            file_path,
            source,
            uuid,
        } => {
            let after = resume_outcome(
                &host,
                &session_id,
                &file_path,
                source,
                uuid.as_deref(),
                overlay,
            );
            apply_after_resume(app, after, &host, &session_id)
        }
        _ => {
            // Quit
            false
        }
    }
}

/// Reopen the picker after a resume when one follows. Returns `true` to loop
/// back into the app.
fn apply_after_resume(
    app: &mut ccs::tui::App,
    after: AfterResume,
    host: &ccs::remote::Host,
    session_id: &str,
) -> bool {
    match after {
        AfterResume::Exit => false,
        AfterResume::Picker { launched } => {
            app.return_from_resume(launched.then_some((host, session_id)));
            true
        }
    }
}

/// Perform the resume and decide whether the picker follows it.
fn resume_outcome(
    host: &ccs::remote::Host,
    session_id: &str,
    file_path: &str,
    source: ccs::session::SessionSource,
    uuid: Option<&str>,
    overlay: bool,
) -> AfterResume {
    let child = picker_launch_runs_as_child(source, overlay, host.is_local());
    let started = Instant::now();
    let result = start_resume(host, session_id, file_path, source, uuid, child);
    let quick_exit = is_quick_exit(source, started.elapsed());
    let launched = result.is_ok();
    if picker_follows_resume(result, child, overlay, quick_exit) {
        AfterResume::Picker { launched }
    } else {
        AfterResume::Exit
    }
}

/// Resume on the machine the session lives on: here as a child or via exec,
/// or on its remote host over ssh.
fn start_resume(
    host: &ccs::remote::Host,
    session_id: &str,
    file_path: &str,
    source: ccs::session::SessionSource,
    uuid: Option<&str>,
    child: bool,
) -> Result<(), String> {
    match host {
        ccs::remote::Host::Local => launch_resume(session_id, file_path, source, uuid, child),
        ccs::remote::Host::Remote(name) => find_host(name)
            .and_then(|h| launch_remote_resume(&h, Some(session_id), file_path, source, uuid)),
    }
}

/// Resume as a child or via exec, as the caller chose. Codex runs as a child
/// either way, see `resume_codex_child`.
fn launch_resume(
    session_id: &str,
    file_path: &str,
    source: ccs::session::SessionSource,
    uuid: Option<&str>,
    child: bool,
) -> Result<(), String> {
    if child {
        ccs::resume::resume_child(session_id, file_path, source, uuid)
    } else {
        ccs::resume::resume(session_id, file_path, source, uuid)
    }
}

/// Whether the picker launches this session as a child: always under
/// `--overlay` and for remote sessions (ssh runs here), otherwise for every
/// session that runs in the terminal, so a quick exit can come back to the
/// picker.
fn picker_launch_runs_as_child(
    source: ccs::session::SessionSource,
    overlay: bool,
    local: bool,
) -> bool {
    overlay || !local || ccs::resume::runs_in_terminal(source)
}

/// Whether a session closed soon enough to count as opened by mistake.
fn is_quick_exit(source: ccs::session::SessionSource, ran_for: Duration) -> bool {
    ccs::resume::runs_in_terminal(source) && ran_for < ccs::resume::launcher::QUICK_EXIT_WINDOW
}

/// Whether the picker comes back after a resume attempt: always under
/// `--overlay`, otherwise after a quick exit. A failed launch is held on
/// screen when it ran as a child; one that leaves nothing to return to exits
/// with status 1.
fn picker_follows_resume(
    result: Result<(), String>,
    child: bool,
    overlay: bool,
    quick_exit: bool,
) -> bool {
    let Err(error) = result else {
        return overlay || quick_exit;
    };
    eprintln!("Error resuming session: {error}");
    let stay = child && wait_for_enter("Press Enter to return to ccs: ");
    if !stay {
        std::process::exit(1);
    }
    true
}

/// Hold a failed launch on screen until the user has read it; the
/// picker's alternate screen or a closing overlay popup would hide it at once.
/// Returns `false` when stdin is closed and nobody can acknowledge.
fn wait_for_enter(prompt: &str) -> bool {
    eprint!("{prompt}");
    let _ = io::stderr().flush();
    io::stdin().read_line(&mut String::new()).unwrap_or(0) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccs::session::SessionSource;

    #[test]
    fn successful_resume_when_deciding_next_step_then_picker_returns_in_overlay_or_after_quick_exit(
    ) {
        assert!(picker_follows_resume(Ok(()), true, true, false));
        assert!(picker_follows_resume(Ok(()), true, false, true));
        assert!(!picker_follows_resume(Ok(()), true, false, false));
    }

    #[test]
    fn cli_session_when_measuring_exit_then_quick_only_below_window() {
        let w = ccs::resume::launcher::QUICK_EXIT_WINDOW;
        assert!(is_quick_exit(
            SessionSource::CLI,
            w - Duration::from_millis(1)
        ));
        assert!(!is_quick_exit(SessionSource::CLI, w));
    }

    #[test]
    fn desktop_session_when_measuring_exit_then_never_quick() {
        assert!(!is_quick_exit(SessionSource::ClaudeDesktop, Duration::ZERO));
    }

    #[test]
    fn resume_session_id_when_given_then_used_over_the_file_name() {
        assert_eq!(
            resume_session_id("/db/opencode.db#ses_x", Some("ses_x")),
            "ses_x"
        );
    }

    #[test]
    fn resume_session_id_when_not_given_then_read_from_file() {
        assert_eq!(resume_session_id("/nope/abc.jsonl", None), "abc");
    }

    #[test]
    fn picker_launch_when_remote_then_child() {
        assert!(picker_launch_runs_as_child(
            SessionSource::ClaudeDesktop,
            false,
            false
        ));
    }

    #[test]
    fn remote_desktop_session_when_resumed_then_error() {
        let host = ccs::remote::config::HostConfig {
            name: "Z".into(),
            ccs_path: "ccs".into(),
        };
        let err = launch_remote_resume(
            &host,
            None,
            "/x/local-agent-mode-sessions/a.jsonl",
            SessionSource::ClaudeDesktop,
            None,
        )
        .unwrap_err();
        assert!(err.contains("Claude Desktop"), "{err}");
    }

    #[test]
    fn picker_resume_when_choosing_launch_mode_then_child_unless_desktop_without_overlay() {
        assert!(picker_launch_runs_as_child(SessionSource::CLI, false, true));
        assert!(!picker_launch_runs_as_child(
            SessionSource::ClaudeDesktop,
            false,
            true
        ));
        assert!(picker_launch_runs_as_child(
            SessionSource::ClaudeDesktop,
            true,
            true
        ));
    }

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

    #[test]
    fn test_handle_outcome_quit_stops_loop() {
        let mut app = ccs::tui::App::new(vec![]);
        assert!(!handle_outcome(ccs::tui::TuiOutcome::Quit, true, &mut app));
    }

    #[test]
    fn picker_after_successful_launch_when_applying_then_app_returns_and_marks_session() {
        let mut app = ccs::tui::App::new(vec![]);
        app.should_quit = true;
        assert!(apply_after_resume(
            &mut app,
            AfterResume::Picker { launched: true },
            &ccs::remote::Host::Local,
            "s1"
        ));
        assert!(!app.should_quit);
        assert!(app
            .opened_sessions
            .contains(&(ccs::remote::Host::Local, "s1".to_string())));
    }

    #[test]
    fn picker_after_failed_launch_when_applying_then_session_is_not_marked() {
        let mut app = ccs::tui::App::new(vec![]);
        assert!(apply_after_resume(
            &mut app,
            AfterResume::Picker { launched: false },
            &ccs::remote::Host::Local,
            "s1"
        ));
        assert!(app.opened_sessions.is_empty());
    }

    #[test]
    fn exit_when_applying_then_loop_stops() {
        let mut app = ccs::tui::App::new(vec![]);
        assert!(!apply_after_resume(
            &mut app,
            AfterResume::Exit,
            &ccs::remote::Host::Local,
            "s1"
        ));
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
            host: ccs::remote::Host::Local,
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
