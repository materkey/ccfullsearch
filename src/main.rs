use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::prelude::*;
use std::io::{self, stdout};
use std::time::Duration;

fn main() -> io::Result<()> {
    // Set panic hook to restore terminal on unexpected crashes
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
        original_hook(info);
    }));

    let args: Vec<String> = std::env::args().collect();

    // CLI subcommands: search, list
    if args.len() > 1 {
        match args[1].as_str() {
            "search" => {
                let query = args.get(2).map(|s| s.as_str()).unwrap_or_else(|| {
                    eprintln!("Usage: ccs search <query> [--regex] [--limit N]");
                    std::process::exit(1);
                });
                let use_regex = args.iter().any(|a| a == "--regex");
                let limit = args
                    .iter()
                    .position(|a| a == "--limit")
                    .and_then(|i| args.get(i + 1))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(100);
                ccs::cli::cli_search(query, &ccs::get_search_paths(), use_regex, limit);
                return Ok(());
            }
            "list" => {
                let limit = args
                    .iter()
                    .position(|a| a == "--limit")
                    .and_then(|i| args.get(i + 1))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(50);
                ccs::cli::cli_list(&ccs::get_search_paths(), limit);
                return Ok(());
            }
            _ => {}
        }
    }

    // Parse TUI-specific args
    let tree_target = if let Some(pos) = args.iter().position(|a| a == "--tree") {
        args.get(pos + 1).cloned()
    } else {
        None
    };

    let search_paths = ccs::get_search_paths();

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
    let mut app = ccs::tui::App::new(search_paths);

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
        terminal.draw(|frame| ccs::tui::render(frame, &app))?;

        // Handle events
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.tree_mode {
                    // Tree mode: Ctrl-C exits tree mode (or quits if standalone)
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.exit_tree_mode();
                        continue;
                    }

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

                    // Handle Ctrl+C: clear input or quit
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if app.input.is_empty() {
                            app.should_quit = true;
                        } else {
                            app.clear_input();
                        }
                        continue;
                    }

                    // Handle Ctrl+R for regex toggle
                    if key.code == KeyCode::Char('r')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.on_toggle_regex();
                        continue;
                    }

                    // Handle Ctrl+B for tree mode
                    if key.code == KeyCode::Char('b')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if !app.groups.is_empty() {
                            app.enter_tree_mode();
                        }
                        continue;
                    }

                    // Word movement: Alt+Left/Right, Ctrl+Left/Right,
                    // and Alt+b/f (macOS Option sends ESC+b/ESC+f)
                    if key.code == KeyCode::Left
                        && key
                            .modifiers
                            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
                        || key.code == KeyCode::Char('b')
                            && key.modifiers.contains(KeyModifiers::ALT)
                    {
                        app.move_cursor_word_left();
                        continue;
                    }
                    if key.code == KeyCode::Right
                        && key
                            .modifiers
                            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
                        || key.code == KeyCode::Char('f')
                            && key.modifiers.contains(KeyModifiers::ALT)
                    {
                        app.move_cursor_word_right();
                        continue;
                    }

                    // Word deletion: Alt+Backspace (macOS) or Ctrl+W (Linux)
                    // Alt+d for delete word right (readline-style)
                    if key.code == KeyCode::Backspace && key.modifiers.contains(KeyModifiers::ALT) {
                        app.delete_word_left();
                        continue;
                    }
                    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::ALT) {
                        app.delete_word_right();
                        continue;
                    }
                    if key.code == KeyCode::Char('w')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.delete_word_left();
                        continue;
                    }

                    // Ctrl+A: toggle project filter (current project only / all sessions)
                    if key.code == KeyCode::Char('a')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.toggle_project_filter();
                        continue;
                    }

                    // Ctrl+V: toggle preview (same as Tab)
                    if key.code == KeyCode::Char('v')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.on_tab();
                        continue;
                    }

                    // Home/End and Ctrl+E for line end
                    if key.code == KeyCode::Char('e')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.move_cursor_end();
                        continue;
                    }

                    match key.code {
                        KeyCode::Esc => {
                            app.should_quit = true;
                        }
                        KeyCode::Home => app.move_cursor_home(),
                        KeyCode::End => app.move_cursor_end(),
                        KeyCode::Up => app.on_up(),
                        KeyCode::Down => app.on_down(),
                        KeyCode::Left => app.on_left(),
                        KeyCode::Right => app.on_right(),
                        KeyCode::Tab => app.on_tab(),
                        KeyCode::Enter => app.on_enter(),
                        KeyCode::Backspace => app.on_backspace(),
                        KeyCode::Delete => app.on_delete(),
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
    execute!(stdout(), cursor::Show, LeaveAlternateScreen)?;

    // Resume if requested
    if let (Some(session_id), Some(file_path), Some(source)) =
        (&app.resume_id, &app.resume_file_path, &app.resume_source)
    {
        let uuid = app.resume_uuid.as_deref();
        if let Err(e) = ccs::resume::resume(session_id, file_path, *source, uuid) {
            eprintln!("Error resuming session: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
