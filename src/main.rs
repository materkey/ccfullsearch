mod resume;
mod search;
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

                // Handle Ctrl+R for regex toggle
                if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.on_toggle_regex();
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
