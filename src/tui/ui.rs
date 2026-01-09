use crate::search::{extract_context, extract_project_from_path, RipgrepMatch, SessionGroup};
use crate::tui::App;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

pub fn render(frame: &mut Frame, app: &App) {
    let [header_area, input_area, status_area, list_area, help_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // Header
    let header = Paragraph::new("Claude Code Session Search")
        .style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD));
    frame.render_widget(header, header_area);

    // Input
    let input_style = if app.typing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    };
    let input = Paragraph::new(format!("{}_", app.input))
        .style(input_style)
        .block(Block::default().borders(Borders::ALL).title("Search"));
    frame.render_widget(input, input_area);

    // Status
    let status = if app.typing {
        Span::styled("Typing...", Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC))
    } else if app.searching {
        Span::styled("Searching...", Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC))
    } else if let Some(ref err) = app.error {
        Span::styled(format!("Error: {}", err), Style::default().fg(Color::Red))
    } else if !app.groups.is_empty() {
        Span::styled(
            format!("Found {} matches in {} sessions", app.results.len(), app.groups.len()),
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
        "[Tab/Enter] Close preview  [Esc] Quit"
    } else if !app.groups.is_empty() {
        "[↑↓] Navigate  [→←] Expand/Collapse  [Tab] Preview  [Enter] Resume  [Esc] Quit"
    } else {
        "[↑↓] Navigate  [Tab] Preview  [Enter] Resume  [Esc] Quit"
    };
    let help = Paragraph::new(help_text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help, help_area);
}

fn render_groups(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut items: Vec<ListItem> = vec![];

    for (i, group) in app.groups.iter().enumerate() {
        let is_selected = i == app.group_cursor;
        let is_expanded = is_selected && app.expanded;

        // Group header
        let header = render_group_header(group, is_selected, is_expanded);
        items.push(header);

        // If expanded, show individual messages
        if is_expanded {
            for (j, m) in group.matches.iter().enumerate() {
                let is_match_selected = j == app.sub_cursor;
                let sub_item = render_sub_match(m, is_match_selected, &app.results_query);
                items.push(sub_item);
            }
        }
    }

    let list = List::new(items).block(Block::default().borders(Borders::NONE));
    frame.render_widget(list, area);
}

fn render_group_header<'a>(group: &SessionGroup, selected: bool, expanded: bool) -> ListItem<'a> {
    let first_match = group.first_match();
    let (date_str, branch) = if let Some(m) = first_match {
        if let Some(ref msg) = m.message {
            let date = msg.timestamp.format("%Y-%m-%d %H:%M").to_string();
            let branch = msg.branch.clone().unwrap_or_else(|| "-".to_string());
            (date, branch)
        } else {
            ("-".to_string(), "-".to_string())
        }
    } else {
        ("-".to_string(), "-".to_string())
    };

    let project = extract_project_from_path(&group.file_path);
    let expand_indicator = if expanded { "▼" } else { "▶" };
    let session_display = if group.session_id.len() > 8 {
        &group.session_id[..8]
    } else {
        &group.session_id
    };

    let header_text = format!(
        "{} {} | {} | {} | {} ({} messages)",
        expand_indicator,
        date_str,
        project,
        branch,
        session_display,
        group.matches.len()
    );

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

fn render_sub_match<'a>(m: &RipgrepMatch, selected: bool, query: &str) -> ListItem<'a> {
    let (role_str, role_style, content) = if let Some(ref msg) = m.message {
        let role_style = if msg.role == "user" {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        };
        let role_str = if msg.role == "user" { "User:" } else { "Claude:" };
        let content = extract_context(&msg.content, query, 30);
        (role_str.to_string(), role_style, content)
    } else {
        ("???:".to_string(), Style::default(), String::new())
    };

    let style = if selected {
        Style::default().fg(Color::Yellow).bg(Color::Rgb(75, 0, 130))
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let prefix = if selected { "    → " } else { "      " };

    // Build the line with styled spans
    let line = Line::from(vec![
        Span::styled(prefix, style),
        Span::styled(role_str, role_style),
        Span::raw(" "),
        Span::styled(format!("\"{}\"", content), style),
    ]);

    ListItem::new(line)
}

fn render_preview(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(m) = app.selected_match() else {
        return;
    };

    let Some(ref msg) = m.message else {
        return;
    };

    let project = extract_project_from_path(&m.file_path);
    let date_str = msg.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
    let branch = msg.branch.clone().unwrap_or_else(|| "-".to_string());

    let mut lines = vec![
        Line::from(format!("Session: {}", msg.session_id)),
        Line::from(format!("Project: {} | Branch: {}", project, branch)),
        Line::from(format!("Date: {}", date_str)),
        Line::from(format!("Role: {}", msg.role)),
        Line::from("─".repeat(60)),
        Line::raw(""),
    ];

    // Content - truncate if too long
    let content = if msg.content.len() > 2000 {
        format!("{}...\n(truncated)", &msg.content[..2000])
    } else {
        msg.content.clone()
    };

    for line in content.lines() {
        lines.push(Line::raw(line.to_string()));
    }

    let preview = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(98, 98, 255)))
                .title("Preview"),
        )
        .style(Style::default().fg(Color::White));

    frame.render_widget(preview, area);
}
