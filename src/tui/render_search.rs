use crate::search::{
    extract_context, extract_project_from_path, sanitize_content, RipgrepMatch, SessionGroup,
};
use crate::tui::render_tree::render_tree_mode;
use crate::tui::App;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use std::collections::HashSet;

pub fn render(frame: &mut Frame, app: &App) {
    if app.tree_mode {
        render_tree_mode(frame, app);
        return;
    }

    let [header_area, input_area, status_area, list_area, help_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // Header
    let header = Paragraph::new(format!("Claude Code Session Search v{}", env!("CARGO_PKG_VERSION"))).style(
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(header, header_area);

    // Input
    let input_style = if app.typing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    };
    let search_title = match (app.regex_mode, app.project_filter) {
        (true, true) => "Search [Regex] [Project]",
        (true, false) => "Search [Regex]",
        (false, true) => "Search [Project]",
        (false, false) => "Search",
    };
    let title_style = if app.regex_mode || app.project_filter {
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let input = Paragraph::new(app.input.as_str()).style(input_style).block(
        Block::default()
            .borders(Borders::ALL)
            .title(search_title)
            .title_style(title_style),
    );
    frame.render_widget(input, input_area);
    // Place native terminal cursor at cursor_pos (inside the border: +1 for border offset)
    let cursor_x = app.input[..app.cursor_pos].chars().count() as u16;
    frame.set_cursor_position((input_area.x + 1 + cursor_x, input_area.y + 1));

    // Status
    let status = if app.typing {
        Span::styled(
            "Typing...",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )
    } else if app.searching {
        Span::styled(
            "Searching...",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )
    } else if let Some(ref err) = app.error {
        Span::styled(format!("Error: {}", err), Style::default().fg(Color::Red))
    } else if !app.groups.is_empty() {
        Span::styled(
            format!(
                "Found {} matches in {} sessions",
                app.results.len(),
                app.groups.len()
            ),
            Style::default().fg(Color::DarkGray),
        )
    } else if !app.results_query.is_empty() {
        Span::styled("No matches found", Style::default().fg(Color::DarkGray))
    } else {
        Span::raw("")
    };
    frame.render_widget(Paragraph::new(Line::from(status)), status_area);

    // List of results (grouped view)
    if app.preview_mode {
        render_preview(frame, app, list_area);
    } else {
        render_groups(frame, app, list_area);
    }

    // Help
    let help_text = if app.preview_mode {
        "[Tab/Ctrl+V/Enter] Close preview  [Ctrl+A] Project  [Ctrl+R] Regex  [Esc] Quit"
    } else if !app.groups.is_empty() {
        "[↑↓] Navigate  [→←] Expand  [Tab/Ctrl+V] Preview  [Enter] Resume  [Ctrl+A] Project  [Ctrl+B] Tree  [Ctrl+R] Regex  [Esc] Quit"
    } else {
        "[↑↓] Navigate  [Tab/Ctrl+V] Preview  [Enter] Resume  [Ctrl+A] Project  [Ctrl+R] Regex  [Esc] Quit"
    };
    let help = Paragraph::new(help_text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help, help_area);
}

fn render_groups(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    // Clear the area by filling with spaces - more reliable than Clear widget
    // This handles wide Unicode characters better
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_style(Style::default());
            }
        }
    }

    let mut items: Vec<ListItem> = vec![];

    for (i, group) in app.groups.iter().enumerate() {
        let is_selected = i == app.group_cursor;
        let is_expanded = is_selected && app.expanded;

        // Group header
        let header = render_group_header(group, is_selected, is_expanded);
        items.push(header);

        // If expanded, show individual messages
        if is_expanded {
            let latest_chain = app.latest_chains.get(&group.file_path);
            for (j, m) in group.matches.iter().enumerate() {
                let is_match_selected = j == app.sub_cursor;
                let sub_item =
                    render_sub_match(m, is_match_selected, &app.results_query, latest_chain);
                items.push(sub_item);
            }
        }
    }

    let list = List::new(items).block(Block::default().borders(Borders::NONE));
    frame.render_widget(list, area);
}

/// Build the header text for a session group (testable function)
pub(crate) fn build_group_header_text(group: &SessionGroup, expanded: bool) -> String {
    let first_match = group.first_match();
    let (date_str, branch, source) = if let Some(m) = first_match {
        let source = m.source.display_name();
        if let Some(ref msg) = m.message {
            let date = msg.timestamp.format("%Y-%m-%d %H:%M").to_string();
            let branch = msg.branch.clone().unwrap_or_else(|| "-".to_string());
            (date, branch, source)
        } else {
            ("-".to_string(), "-".to_string(), source)
        }
    } else {
        ("-".to_string(), "-".to_string(), "CLI")
    };

    let project = extract_project_from_path(&group.file_path);
    let expand_indicator = if expanded { "▼" } else { "▶" };
    let session_display = if group.session_id.len() > 8 {
        &group.session_id[..8]
    } else {
        &group.session_id
    };

    format!(
        "{} [{}] {} | {} | {} | {} ({} messages)",
        expand_indicator,
        source,
        date_str,
        project,
        branch,
        session_display,
        group.matches.len()
    )
}

fn render_group_header<'a>(group: &SessionGroup, selected: bool, expanded: bool) -> ListItem<'a> {
    let header_text = build_group_header_text(group, expanded);

    let style = if selected && !expanded {
        Style::default()
            .fg(Color::Yellow)
            .bg(Color::Rgb(75, 0, 130))
            .add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let prefix = if selected { "> " } else { "  " };
    ListItem::new(format!("{}{}", prefix, header_text)).style(style)
}

fn render_sub_match<'a>(
    m: &RipgrepMatch,
    selected: bool,
    query: &str,
    latest_chain: Option<&HashSet<String>>,
) -> ListItem<'a> {
    let (role_str, role_style, content) = if let Some(ref msg) = m.message {
        let role_style = if msg.role == "user" {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        };
        let role_str = if msg.role == "user" {
            "User:"
        } else {
            "Claude:"
        };
        // Sanitize content before extracting context to remove ANSI codes
        let sanitized = sanitize_content(&msg.content);
        let content = extract_context(&sanitized, query, 30);
        (role_str.to_string(), role_style, content)
    } else {
        ("???:".to_string(), Style::default(), String::new())
    };

    // Determine if this message is on a fork (not on the latest chain)
    let is_fork = latest_chain
        .map(|chain| {
            m.message
                .as_ref()
                .and_then(|msg| msg.uuid.as_deref())
                .map(|uuid| !chain.contains(uuid))
                .unwrap_or(false)
        })
        .unwrap_or(false);

    let style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .bg(Color::Rgb(75, 0, 130))
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let prefix = if selected { "    → " } else { "      " };

    // Build the line with styled spans
    let mut spans = vec![Span::styled(prefix, style)];
    if is_fork {
        spans.push(Span::styled(
            "[fork] ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(role_str, role_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(format!("\"{}\"", content), style));

    ListItem::new(Line::from(spans))
}

/// Truncate a string to fit within max_width display columns.
/// Uses char count as approximation (accurate for ASCII/Latin/Cyrillic).
pub(crate) fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let char_count = s.chars().count();
    if char_count <= max_width {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_width.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// Truncate content around the first query match so the match is visible
/// If content is shorter than max_chars, returns it unchanged
/// If query is not found, truncates from the beginning
fn truncate_around_query(content: &str, query: &str, max_chars: usize) -> String {
    let char_count = content.chars().count();

    if char_count <= max_chars {
        return content.to_string();
    }

    // Find the first occurrence of query (case-insensitive)
    let content_lower = content.to_lowercase();
    let query_lower = query.to_lowercase();

    if let Some(byte_pos) = content_lower.find(&query_lower) {
        // Convert byte position to character position
        let match_char_pos = content[..byte_pos].chars().count();

        // Calculate how much context to show before and after
        let context_before = max_chars / 3; // ~33% before match
        let context_after = max_chars - context_before; // ~67% after match

        let start_char = match_char_pos.saturating_sub(context_before);
        let end_char = (match_char_pos + context_after).min(char_count);

        let truncated: String = content
            .chars()
            .skip(start_char)
            .take(end_char - start_char)
            .collect();

        let mut result = String::new();
        if start_char > 0 {
            result.push_str("...\n");
        }
        result.push_str(&truncated);
        if end_char < char_count {
            result.push_str("\n...(truncated)");
        }
        result
    } else {
        // Query not found, truncate from beginning
        let truncated: String = content.chars().take(max_chars).collect();
        format!("{}...\n(truncated)", truncated)
    }
}

/// Highlight query matches in a line, returning a Line with styled spans
fn highlight_line<'a>(text: &'a str, query: &str) -> Line<'a> {
    if query.is_empty() {
        return Line::raw(text.to_string());
    }

    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();

    let highlight_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let mut spans = Vec::new();
    let mut last_end = 0;

    // Find all occurrences of query (case-insensitive)
    let mut search_start = 0;
    while let Some(pos) = text_lower[search_start..].find(&query_lower) {
        let match_start = search_start + pos;
        let match_end = match_start + query.len();

        // Add text before the match
        if match_start > last_end {
            spans.push(Span::raw(text[last_end..match_start].to_string()));
        }

        // Add highlighted match (preserving original case)
        spans.push(Span::styled(
            text[match_start..match_end].to_string(),
            highlight_style,
        ));

        last_end = match_end;
        search_start = match_end;
    }

    // Add remaining text after last match
    if last_end < text.len() {
        spans.push(Span::raw(text[last_end..].to_string()));
    }

    if spans.is_empty() {
        Line::raw(text.to_string())
    } else {
        Line::from(spans)
    }
}

fn render_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    // Clear the area by filling with spaces - more reliable than Clear widget
    // This handles wide Unicode characters better
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_style(Style::default());
            }
        }
    }

    let Some(m) = app.selected_match() else {
        // Render empty block if no match selected
        let empty = Paragraph::new("")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(98, 98, 255)))
                    .title("Preview"),
            )
            .style(Style::default().bg(Color::Reset));
        frame.render_widget(empty, area);
        return;
    };

    let Some(ref msg) = m.message else {
        let empty = Paragraph::new("")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(98, 98, 255)))
                    .title("Preview"),
            )
            .style(Style::default().bg(Color::Reset));
        frame.render_widget(empty, area);
        return;
    };

    let project = extract_project_from_path(&m.file_path);
    let date_str = msg.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
    let branch = msg.branch.clone().unwrap_or_else(|| "-".to_string());
    let query = &app.results_query;

    let mut lines = vec![
        Line::from(format!("Session: {}", msg.session_id)),
        Line::from(format!("Project: {} | Branch: {}", project, branch)),
        Line::from(format!("Date: {}", date_str)),
        Line::from(format!("Role: {}", msg.role)),
        Line::from("─".repeat(60)),
        Line::raw(""),
    ];

    // Content - sanitize to remove ANSI escape codes, then truncate around query match
    let sanitized = sanitize_content(&msg.content);
    let content = truncate_around_query(&sanitized, query, 2000);

    // Add content lines with query highlighting
    for line in content.lines() {
        lines.push(highlight_line(line, query));
    }

    let preview = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(98, 98, 255)))
                .title("Preview"),
        )
        .style(Style::default().fg(Color::White).bg(Color::Reset))
        .wrap(ratatui::widgets::Wrap { trim: false });

    frame.render_widget(preview, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Message, SessionSource};
    use chrono::{TimeZone, Utc};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn make_test_app_with_groups() -> App {
        let mut app = App::new(vec!["/test".to_string()]);

        let msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Test content for preview".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            uuid: None,
            parent_uuid: None,
        };

        let m = RipgrepMatch {
            file_path: "/path/to/projects/-Users-test-projects-myapp/session.jsonl".to_string(),
            message: Some(msg),
            source: SessionSource::ClaudeCodeCLI,
        };

        app.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: m.file_path.clone(),
            matches: vec![m],
        }];
        app.results_query = "test".to_string();

        app
    }

    #[test]
    fn test_truncate_around_query_short_content() {
        let content = "Short content with adb in it";
        let result = truncate_around_query(content, "adb", 100);
        assert_eq!(result, content); // No truncation needed
    }

    #[test]
    fn test_truncate_around_query_centers_on_match() {
        // Create long content with "adb" in the middle
        let prefix = "x".repeat(500);
        let suffix = "y".repeat(500);
        let content = format!("{}adb{}", prefix, suffix);

        let result = truncate_around_query(&content, "adb", 100);

        // Result should contain "adb"
        assert!(result.contains("adb"), "Result should contain the query");
        // Result should be truncated
        assert!(result.contains("..."), "Result should show truncation");
    }

    #[test]
    fn test_truncate_around_query_at_end() {
        // Create long content with "adb" at the end
        let prefix = "x".repeat(1000);
        let content = format!("{}adb", prefix);

        let result = truncate_around_query(&content, "adb", 100);

        assert!(result.contains("adb"), "Result should contain the query");
    }

    #[test]
    fn test_truncate_around_query_not_found() {
        let content = "x".repeat(500);
        let result = truncate_around_query(&content, "notfound", 100);

        // Should truncate from beginning
        assert!(result.len() <= 120); // 100 chars + "...\n(truncated)"
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_highlight_line_basic() {
        let line = highlight_line("Hello world", "world");
        assert_eq!(line.spans.len(), 2); // "Hello " and "world"
    }

    #[test]
    fn test_highlight_line_case_insensitive() {
        let line = highlight_line("Hello WORLD", "world");
        assert_eq!(line.spans.len(), 2); // "Hello " and "WORLD"
    }

    #[test]
    fn test_highlight_line_multiple_matches() {
        let line = highlight_line("adb shell adb devices", "adb");
        assert_eq!(line.spans.len(), 4); // "adb", " shell ", "adb", " devices"
    }

    #[test]
    fn test_highlight_line_no_match() {
        let line = highlight_line("Hello world", "xyz");
        assert_eq!(line.spans.len(), 1); // just "Hello world"
    }

    #[test]
    fn test_highlight_line_empty_query() {
        let line = highlight_line("Hello world", "");
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn test_render_does_not_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let app = App::new(vec!["/test".to_string()]);

        terminal
            .draw(|frame| render(frame, &app))
            .expect("Render should not panic");
    }

    #[test]
    fn test_render_with_groups() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let app = make_test_app_with_groups();

        terminal
            .draw(|frame| render(frame, &app))
            .expect("Render with groups should not panic");
    }

    #[test]
    fn test_render_preview_mode() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();
        app.preview_mode = true;

        terminal
            .draw(|frame| render(frame, &app))
            .expect("Preview mode render should not panic");
    }

    #[test]
    fn test_render_toggle_preview_clears_area() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();

        // First render normal mode
        terminal
            .draw(|frame| render(frame, &app))
            .expect("Normal render should not panic");

        // Toggle to preview
        app.preview_mode = true;
        terminal
            .draw(|frame| render(frame, &app))
            .expect("Preview render should not panic");

        // Toggle back to normal
        app.preview_mode = false;
        terminal
            .draw(|frame| render(frame, &app))
            .expect("Toggle back render should not panic");

        // The buffer should have valid content without artifacts
        let buffer = terminal.backend().buffer();

        // Check that there are no obvious artifacts (NUL or other control chars)
        for cell in buffer.content() {
            let ch = cell.symbol();
            // Valid chars: printable chars (including Unicode), whitespace
            // Invalid: NUL bytes, control characters
            for c in ch.chars() {
                assert!(
                    !c.is_control() || c.is_whitespace(),
                    "Unexpected control character in buffer: {:?} (U+{:04X})",
                    ch,
                    c as u32
                );
            }
        }
    }

    #[test]
    fn test_render_expanded_group() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();
        app.expanded = true;

        terminal
            .draw(|frame| render(frame, &app))
            .expect("Expanded group render should not panic");
    }

    #[test]
    fn test_render_with_cyrillic_content() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);

        let msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Сделаю: 1. Preview режим 2. Индикатор compacted".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            uuid: None,
            parent_uuid: None,
        };

        let m = RipgrepMatch {
            file_path: "/path/to/session.jsonl".to_string(),
            message: Some(msg),
            source: SessionSource::ClaudeCodeCLI,
        };

        app.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: m.file_path.clone(),
            matches: vec![m],
        }];
        app.preview_mode = true;

        terminal
            .draw(|frame| render(frame, &app))
            .expect("Cyrillic content render should not panic");
    }

    #[test]
    fn test_render_navigation_clears_properly() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);

        // Create multiple groups
        for i in 0..3 {
            let msg = Message {
                session_id: format!("session-{}", i),
                role: "user".to_string(),
                content: format!("Content for session {}", i),
                timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, i as u32, 0).unwrap(),
                branch: Some("main".to_string()),
                line_number: 1,
                uuid: None,
                parent_uuid: None,
            };

            let m = RipgrepMatch {
                file_path: format!(
                    "/path/to/projects/-Users-test-projects-app{}/session.jsonl",
                    i
                ),
                message: Some(msg),
                source: SessionSource::ClaudeCodeCLI,
            };

            app.groups.push(SessionGroup {
                session_id: format!("session-{}", i),
                file_path: m.file_path.clone(),
                matches: vec![m],
            });
        }

        // Navigate through groups
        terminal.draw(|frame| render(frame, &app)).unwrap();
        app.on_down();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        app.on_down();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        app.on_up();
        terminal.draw(|frame| render(frame, &app)).unwrap();

        // All renders should succeed without artifacts
    }

    /// Test for bug: navigating from large content to small content in preview mode
    /// leaves artifacts (scattered characters) on screen.
    #[test]
    fn test_preview_large_to_small_content_no_artifacts() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);

        // Create a large content message (simulating tool_result with many lines)
        let large_content = (0..100)
            .map(|i| format!("Line {}: This is a long line of text that fills the terminal width with content", i))
            .collect::<Vec<_>>()
            .join("\n");

        let large_msg = Message {
            session_id: "test-session".to_string(),
            role: "assistant".to_string(),
            content: large_content,
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            uuid: None,
            parent_uuid: None,
        };

        // Create a small content message
        let small_msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Short".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 1, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 2,
            uuid: None,
            parent_uuid: None,
        };

        let large_match = RipgrepMatch {
            file_path: "/path/to/projects/-Users-test-projects-myapp/session.jsonl".to_string(),
            message: Some(large_msg),
            source: SessionSource::ClaudeCodeCLI,
        };

        let small_match = RipgrepMatch {
            file_path: "/path/to/projects/-Users-test-projects-myapp/session.jsonl".to_string(),
            message: Some(small_msg),
            source: SessionSource::ClaudeCodeCLI,
        };

        // Single group with both messages
        app.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: large_match.file_path.clone(),
            matches: vec![large_match, small_match],
        }];
        app.results_query = "test".to_string();

        // Enter preview mode on large content
        app.preview_mode = true;
        app.expanded = true;
        app.sub_cursor = 0; // Start on large message

        // Render with large content
        terminal.draw(|frame| render(frame, &app)).unwrap();

        // Navigate down to small content
        app.sub_cursor = 1;
        terminal.draw(|frame| render(frame, &app)).unwrap();

        // Check buffer for artifacts
        let buffer = terminal.backend().buffer();

        for cell in buffer.content() {
            let ch = cell.symbol();
            for c in ch.chars() {
                assert!(
                    !c.is_control() || c.is_whitespace(),
                    "Artifact found in buffer: {:?} (U+{:04X})",
                    ch,
                    c as u32
                );
            }
        }

        // Additional check: after small content, most lines should be empty
        let mut non_empty_lines_after_content = 0;
        for y in 15..23 {
            let mut line_content = String::new();
            for x in 0..80 {
                let cell = buffer.cell((x, y)).unwrap();
                line_content.push_str(cell.symbol());
            }
            let trimmed = line_content.trim();
            if !trimmed.is_empty()
                && trimmed != "│"
                && !trimmed.chars().all(|c| c == '│' || c == ' ')
            {
                non_empty_lines_after_content += 1;
            }
        }

        assert!(
            non_empty_lines_after_content <= 2,
            "Found {} non-empty lines after small content - possible artifacts",
            non_empty_lines_after_content
        );
    }

    /// Test navigating through multiple messages of varying sizes in preview mode
    #[test]
    fn test_preview_navigation_varying_sizes_no_artifacts() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);

        // Create messages with varying sizes: large, small, medium, tiny
        let sizes = [
            (
                "Large message with lots of content\n".repeat(50),
                "assistant",
            ),
            ("Tiny".to_string(), "user"),
            (
                "Medium sized message with some content\n".repeat(10),
                "assistant",
            ),
            ("X".to_string(), "user"),
        ];

        let mut matches = Vec::new();
        for (i, (content, role)) in sizes.iter().enumerate() {
            let msg = Message {
                session_id: "test-session".to_string(),
                role: role.to_string(),
                content: content.to_string(),
                timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, i as u32, 0).unwrap(),
                branch: Some("main".to_string()),
                line_number: i + 1,
                uuid: None,
                parent_uuid: None,
            };
            matches.push(RipgrepMatch {
                file_path: "/path/to/projects/-Users-test-projects-app/session.jsonl".to_string(),
                message: Some(msg),
                source: SessionSource::ClaudeCodeCLI,
            });
        }

        app.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: "/path/to/projects/-Users-test-projects-app/session.jsonl".to_string(),
            matches,
        }];
        app.results_query = "test".to_string();
        app.preview_mode = true;
        app.expanded = true;

        // Navigate through all messages, checking buffer after each
        for i in 0..4 {
            app.sub_cursor = i;
            terminal.draw(|frame| render(frame, &app)).unwrap();

            let buffer = terminal.backend().buffer();

            // Check for control character artifacts
            for cell in buffer.content() {
                let ch = cell.symbol();
                for c in ch.chars() {
                    assert!(
                        !c.is_control() || c.is_whitespace(),
                        "Artifact at message {} in buffer: {:?} (U+{:04X})",
                        i,
                        ch,
                        c as u32
                    );
                }
            }
        }

        // Navigate backwards and check again
        for i in (0..4).rev() {
            app.sub_cursor = i;
            terminal.draw(|frame| render(frame, &app)).unwrap();

            let buffer = terminal.backend().buffer();

            for cell in buffer.content() {
                let ch = cell.symbol();
                for c in ch.chars() {
                    assert!(
                        !c.is_control() || c.is_whitespace(),
                        "Artifact (reverse nav) at message {} in buffer: {:?} (U+{:04X})",
                        i,
                        ch,
                        c as u32
                    );
                }
            }
        }
    }

    /// Test with realistic tool_use content (like adb logcat output)
    #[test]
    fn test_preview_realistic_tool_output_no_artifacts() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);

        // Realistic adb logcat output (what the user was viewing)
        let tool_output = r#"12-11 15:25:07.603   211   215 E android.system.suspend@1.0-service: Error opening kernel wakelock stats for: wakeup34: Permission denied
12-11 15:25:07.603   211   215 E android.system.suspend@1.0-service: Error opening kernel wakelock stats for: wakeup35: Permission denied
12-11 15:26:16.284  6931  6931 E AndroidRuntime: FATAL EXCEPTION: main
12-11 15:26:16.284  6931  6931 E AndroidRuntime: Process: com.avito.android.dev, PID: 6931
12-11 15:26:16.284  6931  6931 E AndroidRuntime: java.lang.RuntimeException: Unable to start activity
12-11 15:26:16.284  6931  6931 E AndroidRuntime:        at android.app.ActivityThread.performLaunchActivity(ActivityThread.java:3449)
12-11 15:26:16.284  6931  6931 E AndroidRuntime:        at android.app.ActivityThread.handleLaunchActivity(ActivityThread.java:3601)
12-11 15:26:16.284  6931  6931 E AndroidRuntime:        at android.app.servertransaction.LaunchActivityItem.execute(LaunchActivityItem.java:85)"#;

        let large_msg = Message {
            session_id: "test-session".to_string(),
            role: "assistant".to_string(),
            content: format!("[tool_result]\n{}\n[/tool_result]", tool_output.repeat(5)),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            uuid: None,
            parent_uuid: None,
        };

        // Small follow-up message (Cyrillic like in user's session)
        let small_msg = Message {
            session_id: "test-session".to_string(),
            role: "assistant".to_string(),
            content: "Вижу ключевую строку.".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 1, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 2,
            uuid: None,
            parent_uuid: None,
        };

        app.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: "/path/to/session.jsonl".to_string(),
            matches: vec![
                RipgrepMatch {
                    file_path: "/path/to/session.jsonl".to_string(),
                    message: Some(large_msg),
                    source: SessionSource::ClaudeCodeCLI,
                },
                RipgrepMatch {
                    file_path: "/path/to/session.jsonl".to_string(),
                    message: Some(small_msg),
                    source: SessionSource::ClaudeCodeCLI,
                },
            ],
        }];
        app.results_query = "test".to_string();
        app.preview_mode = true;
        app.expanded = true;

        // Render large tool output
        app.sub_cursor = 0;
        terminal.draw(|frame| render(frame, &app)).unwrap();

        // Navigate to small content
        app.sub_cursor = 1;
        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();

        // Check for leftover content from the large render
        for y in 15..25 {
            let mut line_content = String::new();
            for x in 1..99 {
                let cell = buffer.cell((x, y)).unwrap();
                line_content.push_str(cell.symbol());
            }
            let trimmed = line_content.trim();

            if trimmed.contains("android")
                || trimmed.contains("Exception")
                || trimmed.contains("12-11")
            {
                panic!(
                    "Leftover content from large render on line {}: {:?}",
                    y, trimmed
                );
            }
        }

        // Verify the buffer doesn't contain control characters
        for cell in buffer.content() {
            let ch = cell.symbol();
            for c in ch.chars() {
                assert!(
                    !c.is_control() || c.is_whitespace(),
                    "Control char artifact: {:?} (U+{:04X})",
                    ch,
                    c as u32
                );
            }
        }
    }

    /// Test with content containing ANSI escape sequences (like tool output)
    #[test]
    fn test_preview_ansi_content_no_artifacts() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);

        // Content with ANSI escape sequences (simulating adb logcat output)
        let ansi_content = "\x1b[31mE/AndroidRuntime\x1b[0m: FATAL EXCEPTION: main\n\
            \x1b[33mProcess: com.example.app\x1b[0m\n\
            \x1b[32mjava.lang.NullPointerException\x1b[0m\n\
            \x1b[34m    at com.example.MainActivity.onCreate\x1b[0m\n\
            \x1b[2J\x1b[H\n\
            \x1b[?25l\x1b[?25h\n\
            Normal text after escapes";

        let ansi_msg = Message {
            session_id: "test-session".to_string(),
            role: "assistant".to_string(),
            content: ansi_content.to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            uuid: None,
            parent_uuid: None,
        };

        let small_msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "ok".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 1, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 2,
            uuid: None,
            parent_uuid: None,
        };

        app.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: "/path/to/session.jsonl".to_string(),
            matches: vec![
                RipgrepMatch {
                    file_path: "/path/to/session.jsonl".to_string(),
                    message: Some(ansi_msg),
                    source: SessionSource::ClaudeCodeCLI,
                },
                RipgrepMatch {
                    file_path: "/path/to/session.jsonl".to_string(),
                    message: Some(small_msg),
                    source: SessionSource::ClaudeCodeCLI,
                },
            ],
        }];
        app.results_query = "test".to_string();
        app.preview_mode = true;
        app.expanded = true;

        // Render ANSI content
        app.sub_cursor = 0;
        terminal.draw(|frame| render(frame, &app)).unwrap();

        // Navigate to small content
        app.sub_cursor = 1;
        terminal.draw(|frame| render(frame, &app)).unwrap();

        // Check buffer - no ANSI artifacts should remain
        let buffer = terminal.backend().buffer();
        for cell in buffer.content() {
            let ch = cell.symbol();
            for c in ch.chars() {
                assert!(
                    !c.is_control() || c.is_whitespace(),
                    "ANSI artifact in buffer: {:?} (U+{:04X})",
                    ch,
                    c as u32
                );
                // Also check for escape character specifically
                assert!(
                    c != '\x1b',
                    "ESC character found in buffer - ANSI sequence not stripped"
                );
            }
        }
    }

    #[test]
    fn test_build_group_header_shows_cli_source() {
        let msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Test content".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            uuid: None,
            parent_uuid: None,
        };

        let m = RipgrepMatch {
            file_path: "/Users/test/.claude/projects/-Users-test-myapp/session.jsonl".to_string(),
            message: Some(msg),
            source: SessionSource::ClaudeCodeCLI,
        };

        let group = SessionGroup {
            session_id: "test-session".to_string(),
            file_path: m.file_path.clone(),
            matches: vec![m],
        };

        let text = build_group_header_text(&group, false);
        assert!(
            text.contains("[CLI]"),
            "Header should contain [CLI] indicator, got: {}",
            text
        );
    }

    #[test]
    fn test_build_group_header_shows_desktop_source() {
        let msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Test content".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            uuid: None,
            parent_uuid: None,
        };

        let m = RipgrepMatch {
            file_path: "/Users/test/Library/Application Support/Claude/local-agent-mode-sessions/uuid/uuid/local_123/audit.jsonl".to_string(),
            message: Some(msg),
            source: SessionSource::ClaudeDesktop,
        };

        let group = SessionGroup {
            session_id: "test-session".to_string(),
            file_path: m.file_path.clone(),
            matches: vec![m],
        };

        let text = build_group_header_text(&group, false);
        assert!(
            text.contains("[Desktop]"),
            "Header should contain [Desktop] indicator, got: {}",
            text
        );
    }
}
