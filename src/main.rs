mod resume;
mod search;
mod tree;
mod tui;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, Clear, ClearType},
};
use ratatui::prelude::*;
use std::io::{self, stdout};
use std::time::Duration;

fn main() -> io::Result<()> {
    // Parse CLI args
    let args: Vec<String> = std::env::args().collect();
    let tree_target = if let Some(pos) = args.iter().position(|a| a == "--tree") {
        args.get(pos + 1).cloned()
    } else {
        None
    };

    // Get search paths for both CLI and Desktop sessions
    let mut search_paths = Vec::new();

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

    // Initialize terminal with proper setup
    enable_raw_mode()?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        Clear(ClearType::All),
        cursor::Hide
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Create app
    let mut app = tui::App::new(search_paths);

    // Enter tree mode if --tree flag was provided
    if let Some(target) = tree_target {
        app.enter_tree_mode_direct(&target);
    }

    // Main loop
    loop {
        // Force full redraw if needed (clears diff optimization artifacts)
        if app.needs_full_redraw {
            terminal.clear()?;
            app.needs_full_redraw = false;
        }

        // Draw
        terminal.draw(|frame| tui::render(frame, &app))?;

        // Handle events
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.tree_mode {
                    // Tree mode key handling
                    match key.code {
                        KeyCode::Esc => app.exit_tree_mode(),
                        KeyCode::Up => app.on_up_tree(),
                        KeyCode::Down => app.on_down_tree(),
                        KeyCode::Left => app.on_left_tree(),
                        KeyCode::Right => app.on_right_tree(),
                        KeyCode::Tab => app.on_tab_tree(),
                        KeyCode::Enter => app.on_enter_tree(),
                        KeyCode::Char('b') => app.exit_tree_mode(),
                        KeyCode::Char('q') => {
                            app.should_quit = true;
                        }
                        _ => {}
                    }
                } else {
                    // Search mode key handling

                    // Handle Ctrl+R for regex toggle
                    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        app.on_toggle_regex();
                        continue;
                    }

                    // Handle Ctrl+B for tree mode
                    if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        if !app.groups.is_empty() {
                            app.enter_tree_mode();
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::Esc => {
                            app.should_quit = true;
                        }
                        KeyCode::Up => app.on_up(),
                        KeyCode::Down => app.on_down(),
                        KeyCode::Left => app.on_left(),
                        KeyCode::Right => app.on_right(),
                        KeyCode::Tab => app.on_tab(),
                        KeyCode::Enter => app.on_enter(),
                        KeyCode::Backspace => app.on_backspace(),
                        KeyCode::Char(c) => app.on_key(c),
                        _ => {}
                    }
                }
            }
        }

        // Tick for debounce
        app.tick();

        // Check if should quit
        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        stdout(),
        cursor::Show,
        LeaveAlternateScreen
    )?;

    // Resume if requested
    if let (Some(session_id), Some(file_path), Some(source)) = (&app.resume_id, &app.resume_file_path, &app.resume_source) {
        let uuid = app.resume_uuid.as_deref();
        if let Err(e) = resume::resume(session_id, file_path, *source, uuid) {
            eprintln!("Error resuming session: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
