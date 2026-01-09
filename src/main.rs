mod resume;
mod search;
mod tui;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io::{self, stdout};
use std::time::Duration;

fn main() -> io::Result<()> {
    // Get default search path
    let search_path = dirs::home_dir()
        .map(|h| h.join(".claude").join("projects"))
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "~/.claude/projects".to_string());

    // Initialize terminal
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    // Create app
    let mut app = tui::App::new(search_path);

    // Main loop
    loop {
        // Draw
        terminal.draw(|frame| tui::render(frame, &app))?;

        // Handle events
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
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
    execute!(stdout(), LeaveAlternateScreen)?;

    // Resume if requested
    if let (Some(session_id), Some(file_path)) = (&app.resume_id, &app.resume_file_path) {
        if let Err(e) = resume::resume(session_id, file_path) {
            eprintln!("Error resuming session: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
