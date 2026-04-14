use crate::search::{
    extract_context, extract_project_from_path, sanitize_content, RipgrepMatch, SessionGroup,
};
use crate::tui::render_tree::render_tree_mode;
use crate::tui::view::AppView;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::collections::HashSet;

#[derive(Clone)]
pub(crate) struct HintItem<'a> {
    pub spans: Vec<Span<'a>>,
    pub min_width: u16,
}

pub(crate) fn build_help_line<'a>(hints: &[HintItem<'a>], available_width: u16) -> Line<'a> {
    let separator = "  ";
    let sep_len = separator.len();
    let width = available_width as usize;

    // Pre-compute char widths for all hints
    let widths: Vec<usize> = hints
        .iter()
        .map(|h| h.spans.iter().map(|s| s.content.chars().count()).sum())
        .collect();

    // Phase 1: keep hints whose min_width threshold is met
    let mut keep: Vec<bool> = hints
        .iter()
        .map(|h| h.min_width <= available_width)
        .collect();

    // Phase 2: if selected hints overflow, drop optional hints (highest min_width first)
    while kept_line_width(&keep, &widths, sep_len) > width {
        let mut best: Option<usize> = None;
        for i in 0..hints.len() {
            if keep[i]
                && hints[i].min_width > 0
                && best.is_none_or(|b| hints[i].min_width > hints[b].min_width)
            {
                best = Some(i);
            }
        }
        match best {
            Some(idx) => keep[idx] = false,
            None => break, // only essentials remain
        }
    }

    // Build final spans
    let mut result_spans: Vec<Span<'a>> = Vec::new();
    let mut first = true;
    for (i, hint) in hints.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        if !first {
            result_spans.push(Span::raw(separator));
        }
        result_spans.extend(hint.spans.clone());
        first = false;
    }
    Line::from(result_spans)
}

fn kept_line_width(keep: &[bool], widths: &[usize], sep_len: usize) -> usize {
    let mut total = 0;
    let mut count = 0usize;
    for (i, &k) in keep.iter().enumerate() {
        if k {
            total += widths[i];
            count += 1;
        }
    }
    if count > 1 {
        total += (count - 1) * sep_len;
    }
    total
}

fn search_results_status_text(app: &AppView) -> Option<String> {
    if app.search.results_query.is_empty() {
        return None;
    }

    let total_groups = app.search.all_groups.len().max(app.search.groups.len());
    if total_groups == 0 {
        if app.search.search_truncated {
            return Some(
                "No matches found (results may be incomplete — try a more specific query)"
                    .to_string(),
            );
        }
        return Some("No matches found".to_string());
    }

    let truncation_warning = if app.search.search_truncated {
        " (results may be incomplete)"
    } else {
        ""
    };

    let hidden = total_groups.saturating_sub(app.search.groups.len());
    if hidden == 0 {
        return Some(format!(
            "Found {} matches in {} sessions{}",
            app.search.results_count,
            app.search.groups.len(),
            truncation_warning
        ));
    }

    let hidden_text = if app.search.groups.is_empty() {
        "all hidden by filter".to_string()
    } else {
        format!("{} hidden by filter", hidden)
    };

    let suffix = if app.search.search_truncated {
        format!("({}; results may be incomplete)", hidden_text)
    } else {
        format!("({})", hidden_text)
    };

    Some(format!(
        "Found {} matches in {} sessions {}",
        app.search.results_count,
        app.search.groups.len(),
        suffix
    ))
}

fn recent_sessions_status_text(app: &AppView) -> Option<String> {
    if !app.input.is_empty() {
        return None;
    }

    if app.recent.is_loading(app.project_filter) {
        return Some("Loading recent sessions...".to_string());
    }

    let total = app.recent.total_count(app.project_filter);
    let shown = app.recent.filtered.len();
    if shown > 0 {
        if shown < total {
            return Some(format!(
                "{} recent sessions ({} hidden by filter)",
                shown,
                total - shown
            ));
        }
        return Some(format!("{} recent sessions", shown));
    }

    if total > 0 {
        return Some(format!("0 recent sessions ({} hidden by filter)", total));
    }

    Some("No recent sessions found".to_string())
}

pub fn render(frame: &mut Frame, view: &AppView) {
    if view.tree_mode {
        render_tree_mode(frame, view);
        return;
    }

    let app = view;

    let [header_area, input_area, status_area, list_area, help_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // Header
    let header = Paragraph::new(format!(
        "Claude Code Session Search v{}",
        env!("CARGO_PKG_VERSION")
    ))
    .style(
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(header, header_area);

    // Input — switch between normal search and AI query
    let (display_text, display_cursor) = if app.ai.active {
        (app.ai.query.text(), app.ai.query.cursor_pos())
    } else {
        (app.input.text(), app.input.cursor_pos())
    };
    let input_style = if app.ai.active {
        Style::default().fg(Color::Magenta)
    } else if app.typing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    };
    use crate::tui::state::AutomationFilter;
    let mut search_title = if app.ai.active {
        String::from("AI")
    } else {
        String::from("Search")
    };
    if app.picker_mode {
        search_title.push_str(" [PICK]");
    }
    if app.regex_mode {
        search_title.push_str(" [Regex]");
    }
    if app.project_filter {
        search_title.push_str(" [Project]");
    }
    match app.automation_filter {
        AutomationFilter::All => {}
        AutomationFilter::Manual => search_title.push_str(" [Manual]"),
        AutomationFilter::Auto => search_title.push_str(" [Auto]"),
    }
    let has_active_filter = app.ai.active
        || app.regex_mode
        || app.project_filter
        || app.automation_filter != AutomationFilter::All;
    let title_style = if has_active_filter {
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let input = Paragraph::new(display_text).style(input_style).block(
        Block::default()
            .borders(Borders::ALL)
            .title(search_title.as_str())
            .title_style(title_style),
    );
    frame.render_widget(input, input_area);
    let cursor_x = display_text[..display_cursor].chars().count() as u16;
    frame.set_cursor_position((input_area.x + 1 + cursor_x, input_area.y + 1));

    // Status — AI mode takes priority
    let status = if app.ai.active && app.ai.thinking {
        Span::styled(
            "AI thinking...",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC),
        )
    } else if app.ai.active {
        if let Some(ref err) = app.ai.error {
            Span::styled(format!("AI: {}", err), Style::default().fg(Color::Red))
        } else if let Some(n) = app.ai.ranked_count {
            Span::styled(
                format!("AI: {} sessions ranked", n),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "Type query, Enter to rank",
                Style::default().fg(Color::Magenta),
            )
        }
    } else if app.typing {
        Span::styled(
            "Typing...",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )
    } else if app.search.searching {
        Span::styled(
            "Searching...",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )
    } else if let Some(ref err) = app.search.error {
        Span::styled(format!("Error: {}", err), Style::default().fg(Color::Red))
    } else if let Some(text) = search_results_status_text(app) {
        Span::styled(text, Style::default().fg(Color::DarkGray))
    } else if let Some(text) = recent_sessions_status_text(app) {
        let style = if app.recent.loading {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        Span::styled(text, style)
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

    // Help — show current filter mode inline with color
    use crate::tui::state::AutomationFilter as AF;
    let in_recent_mode = app.in_recent_sessions_mode() && !app.recent.filtered.is_empty();
    let filter_label = match app.automation_filter {
        AF::All => "All",
        AF::Manual => "Manual",
        AF::Auto => "Auto",
    };
    let filter_style = match app.automation_filter {
        AF::All => Style::default().fg(Color::DarkGray),
        AF::Manual => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        AF::Auto => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    };

    let dim = Style::default().fg(Color::DarkGray);

    let hints: Vec<HintItem> = if app.ai.active {
        vec![
            HintItem {
                spans: vec![Span::styled("[Enter] Rank", dim)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[↑↓] Navigate", dim)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[Esc/Ctrl+G] Cancel", dim)],
                min_width: 0,
            },
        ]
    } else {
        let filter_hint = HintItem {
            spans: vec![
                Span::styled("[Ctrl+H] ", dim),
                Span::styled(filter_label, filter_style),
            ],
            min_width: 60,
        };

        if app.preview_mode {
            vec![
                HintItem {
                    spans: vec![Span::styled("[Tab/Ctrl+V/Enter] Close preview", dim)],
                    min_width: 0,
                },
                HintItem {
                    spans: vec![Span::styled("[Ctrl+A] Project", dim)],
                    min_width: 70,
                },
                filter_hint,
                HintItem {
                    spans: vec![Span::styled("[Ctrl+R] Regex", dim)],
                    min_width: 90,
                },
                HintItem {
                    spans: vec![Span::styled("[Esc] Quit", dim)],
                    min_width: 0,
                },
            ]
        } else if in_recent_mode {
            vec![
                HintItem {
                    spans: vec![Span::styled("[↑↓] Navigate", dim)],
                    min_width: 0,
                },
                HintItem {
                    spans: vec![Span::styled("[Enter] Resume", dim)],
                    min_width: 0,
                },
                HintItem {
                    spans: vec![Span::styled("[Ctrl+G] AI", dim)],
                    min_width: 80,
                },
                HintItem {
                    spans: vec![Span::styled("[Ctrl+A] Project", dim)],
                    min_width: 70,
                },
                filter_hint,
                HintItem {
                    spans: vec![Span::styled("[Ctrl+B] Tree", dim)],
                    min_width: 90,
                },
                HintItem {
                    spans: vec![Span::styled("[Esc] Quit", dim)],
                    min_width: 0,
                },
            ]
        } else if !app.search.groups.is_empty() {
            vec![
                HintItem {
                    spans: vec![Span::styled("[↑↓] Navigate", dim)],
                    min_width: 0,
                },
                HintItem {
                    spans: vec![Span::styled("[→←] Expand", dim)],
                    min_width: 100,
                },
                HintItem {
                    spans: vec![Span::styled("[Tab/Ctrl+V] Preview", dim)],
                    min_width: 90,
                },
                HintItem {
                    spans: vec![Span::styled("[Enter] Resume", dim)],
                    min_width: 0,
                },
                HintItem {
                    spans: vec![Span::styled("[Ctrl+G] AI", dim)],
                    min_width: 80,
                },
                HintItem {
                    spans: vec![Span::styled("[Ctrl+A] Project", dim)],
                    min_width: 70,
                },
                filter_hint,
                HintItem {
                    spans: vec![Span::styled("[Ctrl+B] Tree", dim)],
                    min_width: 90,
                },
                HintItem {
                    spans: vec![Span::styled("[Ctrl+R] Regex", dim)],
                    min_width: 100,
                },
                HintItem {
                    spans: vec![Span::styled("[Esc] Quit", dim)],
                    min_width: 0,
                },
            ]
        } else {
            vec![
                HintItem {
                    spans: vec![Span::styled("[↑↓] Navigate", dim)],
                    min_width: 0,
                },
                HintItem {
                    spans: vec![Span::styled("[Tab/Ctrl+V] Preview", dim)],
                    min_width: 80,
                },
                HintItem {
                    spans: vec![Span::styled("[Enter] Resume", dim)],
                    min_width: 0,
                },
                HintItem {
                    spans: vec![Span::styled("[Ctrl+A] Project", dim)],
                    min_width: 70,
                },
                filter_hint,
                HintItem {
                    spans: vec![Span::styled("[Ctrl+R] Regex", dim)],
                    min_width: 90,
                },
                HintItem {
                    spans: vec![Span::styled("[Esc] Quit", dim)],
                    min_width: 0,
                },
            ]
        }
    };

    let help = Paragraph::new(build_help_line(&hints, help_area.width));
    frame.render_widget(help, help_area);
}

fn render_groups(frame: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
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

    // Show recent sessions when input is empty and no search results
    if app.input.is_empty() && app.search.groups.is_empty() {
        render_recent_sessions(frame, app, area);
        return;
    }

    let mut items: Vec<ListItem> = vec![];
    let mut selected_item_idx = 0usize;

    for (i, group) in app.search.groups.iter().enumerate() {
        let is_selected = i == app.search.group_cursor;
        let is_expanded = is_selected && app.search.expanded;

        // Track selected item index: for expanded group, header + 1 + sub_cursor;
        // for collapsed group, just the header position.
        if is_selected {
            selected_item_idx = if is_expanded {
                items.len() + 1 + app.search.sub_cursor
            } else {
                items.len()
            };
        }

        // Group header
        let header = render_group_header(group, is_selected, is_expanded);
        items.push(header);

        // Preview line for collapsed groups (like recent sessions show summaries)
        if !is_expanded {
            let preview_msg = group
                .matches
                .iter()
                .filter_map(|m| m.message.as_ref())
                .find(|msg| !msg.text_content.trim().is_empty());
            if let Some(msg) = preview_msg {
                let role_label = if msg.role == "user" { "User" } else { "Claude" };
                let content = sanitize_content(&msg.text_content);
                let prefix = format!("     {}: ", role_label);
                let prefix_len = prefix.len();
                let max_content = (area.width as usize).saturating_sub(prefix_len);
                let truncated = truncate_to_width(&content, max_content);
                let preview_item = ListItem::new(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::styled(truncated, Style::default().fg(Color::DarkGray)),
                ]));
                items.push(preview_item);
            }
        }

        // If expanded, show individual messages
        if is_expanded {
            let latest_chain = app.search.latest_chains.get(&group.file_path);
            for (j, m) in group.matches.iter().enumerate() {
                let is_match_selected = j == app.search.sub_cursor;
                let sub_item = render_sub_match(
                    m,
                    is_match_selected,
                    &app.search.results_query,
                    latest_chain,
                );
                items.push(sub_item);
            }
        }
    }

    let mut list_state = ListState::default();
    list_state.select(Some(selected_item_idx));
    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(Style::default()); // no-op — custom styles already on items
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_recent_sessions(frame: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    // Loading and empty states are shown in the status bar only
    if app.recent.loading || app.recent.filtered.is_empty() {
        return;
    }

    let visible_height = area.height as usize;
    let available_width = area.width as usize;
    let mut items: Vec<ListItem> = vec![];

    // Use pre-computed scroll offset (adjusted in event handlers, not here)
    let scroll_offset = app.recent.scroll_offset;

    let end = (scroll_offset + visible_height).min(app.recent.filtered.len());

    for i in scroll_offset..end {
        let session = &app.recent.filtered[i];
        let is_selected = i == app.recent.cursor;

        let date_str = session.timestamp.format("%Y-%m-%d %H:%M").to_string();
        // Reserve space: "  " prefix + date (16) + "  " + project + "  " + summary
        let project_max = 20;
        let project_display = truncate_to_width(&session.project, project_max);
        let is_automated = session.automation.is_some();
        let auto_prefix = if is_automated { "[A] " } else { "" };
        let prefix_len =
            2 + date_str.len() + 2 + project_display.chars().count() + 2 + auto_prefix.len();
        let summary_max = available_width.saturating_sub(prefix_len);
        let summary_display = truncate_to_width(&session.summary, summary_max);

        let prefix = if is_selected { "> " } else { "  " };

        let mut spans = vec![
            Span::raw(prefix.to_string()),
            Span::styled(date_str, Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(project_display, Style::default().fg(Color::Cyan)),
            Span::raw("  "),
        ];

        if is_automated {
            spans.push(Span::styled("[A] ", Style::default().fg(Color::DarkGray)));
        }

        let summary_color = if is_automated {
            Color::Gray
        } else {
            Color::White
        };

        if is_selected {
            spans.push(Span::styled(
                summary_display,
                Style::default()
                    .fg(summary_color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                summary_display,
                Style::default().fg(summary_color),
            ));
        }

        let style = if is_selected {
            Style::default().bg(Color::Rgb(75, 0, 130))
        } else {
            Style::default()
        };

        items.push(ListItem::new(Line::from(spans)).style(style));
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

    let auto_tag = if group.automation.is_some() {
        " [A]"
    } else {
        ""
    };

    let count_str = match group.message_count {
        Some(total) => {
            let suffix = if group.message_count_compacted {
                "+"
            } else {
                ""
            };
            format!("{}/{}{}", group.matches.len(), total, suffix)
        }
        None => format!("{}", group.matches.len()),
    };

    format!(
        "{} [{}] {} | {} | {} | {} ({} matches){}",
        expand_indicator, source, date_str, project, branch, session_display, count_str, auto_tag
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

    let query_lower = query.to_lowercase();
    let (text_lower, lower_start_map, lower_end_map) = build_lowercase_index(text);

    let highlight_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let mut spans = Vec::new();
    let mut last_end = 0;

    // Find all occurrences of query (case-insensitive)
    let mut search_start = 0;
    while let Some((match_start, match_end, next_search_start)) = find_case_insensitive_match(
        text,
        &text_lower,
        &lower_start_map,
        &lower_end_map,
        &query_lower,
        search_start,
    ) {
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
        search_start = next_search_start;
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

fn build_lowercase_index(text: &str) -> (String, Vec<Option<usize>>, Vec<Option<usize>>) {
    let mut text_lower = String::new();
    let mut lower_start_map = vec![None; 1];
    let mut lower_end_map = vec![Some(0); 1];
    let mut chars = text.char_indices().peekable();

    while let Some((char_start, ch)) = chars.next() {
        let char_end = chars.peek().map(|(idx, _)| *idx).unwrap_or(text.len());
        let lower_start = text_lower.len();
        let lower_chunk = ch.to_lowercase().collect::<String>();
        text_lower.push_str(&lower_chunk);
        let lower_end = text_lower.len();

        if lower_start_map.len() <= lower_end {
            lower_start_map.resize(lower_end + 1, None);
        }
        if lower_end_map.len() <= lower_end {
            lower_end_map.resize(lower_end + 1, None);
        }

        lower_start_map[lower_start] = Some(char_start);
        for (offset, _) in lower_chunk.char_indices().skip(1) {
            lower_end_map[lower_start + offset] = Some(char_end);
        }
        lower_end_map[lower_end] = Some(char_end);
    }

    (text_lower, lower_start_map, lower_end_map)
}

fn find_case_insensitive_match(
    text: &str,
    text_lower: &str,
    lower_start_map: &[Option<usize>],
    lower_end_map: &[Option<usize>],
    query_lower: &str,
    mut search_start: usize,
) -> Option<(usize, usize, usize)> {
    while search_start <= text_lower.len() {
        let relative_pos = text_lower[search_start..].find(query_lower)?;
        let lower_match_start = search_start + relative_pos;
        let lower_match_end = lower_match_start + query_lower.len();

        let match_start = lower_start_map.get(lower_match_start).copied().flatten();
        let match_end = lower_end_map.get(lower_match_end).copied().flatten();

        if let (Some(match_start), Some(match_end)) = (match_start, match_end) {
            if text.is_char_boundary(match_start) && text.is_char_boundary(match_end) {
                return Some((match_start, match_end, lower_match_end));
            }
        }

        let next_char = text_lower[lower_match_start..].chars().next()?;
        search_start = lower_match_start + next_char.len_utf8();
    }

    None
}

fn render_preview(frame: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
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
    let query = &app.search.results_query;

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
    use crate::search::Message;
    use crate::session::SessionSource;
    use crate::tui::App;
    use chrono::{TimeZone, Utc};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn buffer_contains(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> bool {
        (0..height).any(|y| {
            let mut line = String::new();
            for x in 0..width {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            line.contains(needle)
        })
    }

    fn make_test_app_with_groups() -> App {
        let mut app = App::new(vec!["/test".to_string()]);

        let msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Test content for preview".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            ..Default::default()
        };

        let m = RipgrepMatch {
            file_path: "/path/to/projects/-Users-test-projects-myapp/session.jsonl".to_string(),
            message: Some(msg),
            source: SessionSource::ClaudeCodeCLI,
        };

        app.search.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: m.file_path.clone(),
            matches: vec![m],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.results_query = "test".to_string();

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
    fn test_highlight_line_handles_unicode_lowercase_expansion() {
        let line = highlight_line("İstanbul", "i");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content.as_ref(), "İ");
        assert_eq!(line.spans[1].content.as_ref(), "stanbul");
    }

    #[test]
    fn test_render_does_not_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let app = App::new(vec!["/test".to_string()]);

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");
    }

    #[test]
    fn test_render_with_groups() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let app = make_test_app_with_groups();

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render with groups should not panic");
    }

    #[test]
    fn test_render_preview_mode() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();
        app.preview_mode = true;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Preview mode render should not panic");
    }

    #[test]
    fn test_render_toggle_preview_clears_area() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();

        // First render normal mode
        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Normal render should not panic");

        // Toggle to preview
        app.preview_mode = true;
        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Preview render should not panic");

        // Toggle back to normal
        app.preview_mode = false;
        terminal
            .draw(|frame| render(frame, &app.view()))
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
        app.search.expanded = true;

        terminal
            .draw(|frame| render(frame, &app.view()))
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
            ..Default::default()
        };

        let m = RipgrepMatch {
            file_path: "/path/to/session.jsonl".to_string(),
            message: Some(msg),
            source: SessionSource::ClaudeCodeCLI,
        };

        app.search.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: m.file_path.clone(),
            matches: vec![m],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.preview_mode = true;

        terminal
            .draw(|frame| render(frame, &app.view()))
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
                ..Default::default()
            };

            let m = RipgrepMatch {
                file_path: format!(
                    "/path/to/projects/-Users-test-projects-app{}/session.jsonl",
                    i
                ),
                message: Some(msg),
                source: SessionSource::ClaudeCodeCLI,
            };

            app.search.groups.push(SessionGroup {
                session_id: format!("session-{}", i),
                file_path: m.file_path.clone(),
                matches: vec![m],
                automation: None,
                message_count: None,
                message_count_compacted: false,
            });
        }

        // Navigate through groups
        terminal.draw(|frame| render(frame, &app.view())).unwrap();
        app.on_down();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();
        app.on_down();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();
        app.on_up();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

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
            ..Default::default()
        };

        // Create a small content message
        let small_msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Short".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 1, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 2,
            ..Default::default()
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
        app.search.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: large_match.file_path.clone(),
            matches: vec![large_match, small_match],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.results_query = "test".to_string();

        // Enter preview mode on large content
        app.preview_mode = true;
        app.search.expanded = true;
        app.search.sub_cursor = 0; // Start on large message

        // Render with large content
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

        // Navigate down to small content
        app.search.sub_cursor = 1;
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

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
                ..Default::default()
            };
            matches.push(RipgrepMatch {
                file_path: "/path/to/projects/-Users-test-projects-app/session.jsonl".to_string(),
                message: Some(msg),
                source: SessionSource::ClaudeCodeCLI,
            });
        }

        app.search.groups = vec![SessionGroup {
            session_id: "test-session".to_string(),
            file_path: "/path/to/projects/-Users-test-projects-app/session.jsonl".to_string(),
            matches,
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.results_query = "test".to_string();
        app.preview_mode = true;
        app.search.expanded = true;

        // Navigate through all messages, checking buffer after each
        for i in 0..4 {
            app.search.sub_cursor = i;
            terminal.draw(|frame| render(frame, &app.view())).unwrap();

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
            app.search.sub_cursor = i;
            terminal.draw(|frame| render(frame, &app.view())).unwrap();

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
            ..Default::default()
        };

        // Small follow-up message (Cyrillic like in user's session)
        let small_msg = Message {
            session_id: "test-session".to_string(),
            role: "assistant".to_string(),
            content: "Вижу ключевую строку.".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 1, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 2,
            ..Default::default()
        };

        app.search.groups = vec![SessionGroup {
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
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.results_query = "test".to_string();
        app.preview_mode = true;
        app.search.expanded = true;

        // Render large tool output
        app.search.sub_cursor = 0;
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

        // Navigate to small content
        app.search.sub_cursor = 1;
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

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
            ..Default::default()
        };

        let small_msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "ok".to_string(),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, 1, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 2,
            ..Default::default()
        };

        app.search.groups = vec![SessionGroup {
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
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.results_query = "test".to_string();
        app.preview_mode = true;
        app.search.expanded = true;

        // Render ANSI content
        app.search.sub_cursor = 0;
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

        // Navigate to small content
        app.search.sub_cursor = 1;
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

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
            ..Default::default()
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
            automation: None,
            message_count: None,
            message_count_compacted: false,
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
            ..Default::default()
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
            automation: None,
            message_count: None,
            message_count_compacted: false,
        };

        let text = build_group_header_text(&group, false);
        assert!(
            text.contains("[Desktop]"),
            "Header should contain [Desktop] indicator, got: {}",
            text
        );
    }

    #[test]
    fn test_render_recent_sessions_loading() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = true;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render with loading recent sessions should not panic");

        let buffer = terminal.backend().buffer();
        let mut found_loading = false;
        for y in 0..24 {
            let mut line = String::new();
            for x in 0..80 {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            if line.contains("Loading recent sessions") {
                found_loading = true;
                break;
            }
        }
        assert!(found_loading, "Should show loading indicator");
    }

    #[test]
    fn test_render_recent_sessions_empty() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render with empty recent sessions should not panic");

        let buffer = terminal.backend().buffer();
        let mut found_empty = false;
        for y in 0..24 {
            let mut line = String::new();
            for x in 0..80 {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            if line.contains("No recent sessions found") {
                found_empty = true;
                break;
            }
        }
        assert!(found_empty, "Should show empty state message");
    }

    #[test]
    fn test_render_recent_sessions_with_data() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        app.recent.filtered = vec![
            RecentSession {
                session_id: "sess-1".to_string(),
                file_path: "/test/session1.jsonl".to_string(),
                project: "my-project".to_string(),
                source: SessionSource::ClaudeCodeCLI,
                timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                summary: "Fix the login bug".to_string(),
                automation: None,
            },
            RecentSession {
                session_id: "sess-2".to_string(),
                file_path: "/test/session2.jsonl".to_string(),
                project: "other-app".to_string(),
                source: SessionSource::ClaudeCodeCLI,
                timestamp: Utc.with_ymd_and_hms(2025, 5, 31, 9, 0, 0).unwrap(),
                summary: "Add new feature".to_string(),
                automation: None,
            },
        ];

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render with recent sessions should not panic");

        let buffer = terminal.backend().buffer();
        let mut found_project = false;
        let mut found_summary = false;
        let mut found_status = false;
        for y in 0..24 {
            let mut line = String::new();
            for x in 0..100 {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            if line.contains("my-project") {
                found_project = true;
            }
            if line.contains("Fix the login bug") {
                found_summary = true;
            }
            if line.contains("2 recent sessions") {
                found_status = true;
            }
        }
        assert!(found_project, "Should show project name");
        assert!(found_summary, "Should show session summary");
        assert!(found_status, "Should show session count in status bar");
    }

    #[test]
    fn test_render_recent_sessions_help_bar() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        app.recent.filtered = vec![RecentSession {
            session_id: "sess-1".to_string(),
            file_path: "/test/session1.jsonl".to_string(),
            project: "proj".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
            summary: "hello".to_string(),
            automation: None,
        }];

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        // Check that help bar shows recent sessions keybindings
        // Width 120 ensures all hints fit without overflow truncation
        let buffer = terminal.backend().buffer();
        let mut last_line = String::new();
        for x in 0..120 {
            last_line.push_str(buffer.cell((x, 23)).unwrap().symbol());
        }
        assert!(
            last_line.contains("Navigate")
                && last_line.contains("Resume")
                && last_line.contains("Tree"),
            "Help bar should show recent session keybindings, got: {}",
            last_line.trim()
        );
    }

    #[test]
    fn test_render_search_status_reports_hidden_groups() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.search.results_query = "later".to_string();
        let test_match = RipgrepMatch {
            file_path: "/test/session.jsonl".to_string(),
            message: Some(Message {
                session_id: "sess-1".to_string(),
                role: "assistant".to_string(),
                content: "Later answer".to_string(),
                timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::ClaudeCodeCLI,
        };
        app.search.results_count = 1;
        app.search.all_groups = vec![SessionGroup {
            session_id: "sess-1".to_string(),
            file_path: "/test/session.jsonl".to_string(),
            matches: vec![test_match],
            automation: Some("ralphex".to_string()),
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.groups = vec![];

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render with hidden groups should not panic");

        assert!(buffer_contains(
            terminal.backend().buffer(),
            100,
            24,
            "Found 1 matches in 0 sessions (all hidden by filter)"
        ));
    }

    #[test]
    fn test_search_truncation_surfaces_in_status_bar() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.search.results_query = "test".to_string();
        let test_match = RipgrepMatch {
            file_path: "/test/session.jsonl".to_string(),
            message: Some(Message {
                session_id: "sess-1".to_string(),
                role: "assistant".to_string(),
                content: "test answer".to_string(),
                timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::ClaudeCodeCLI,
        };
        app.search.results_count = 1;
        app.search.all_groups = vec![SessionGroup {
            session_id: "sess-1".to_string(),
            file_path: "/test/session.jsonl".to_string(),
            matches: vec![test_match],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.groups = app.search.all_groups.clone();
        app.search.search_truncated = true;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render with truncation should not panic");

        assert!(buffer_contains(
            terminal.backend().buffer(),
            120,
            24,
            "results may be incomplete"
        ));
    }

    #[test]
    fn test_search_no_truncation_hides_warning() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.search.results_query = "test".to_string();
        let test_match = RipgrepMatch {
            file_path: "/test/session.jsonl".to_string(),
            message: Some(Message {
                session_id: "sess-1".to_string(),
                role: "assistant".to_string(),
                content: "test answer".to_string(),
                timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
                line_number: 1,
                ..Default::default()
            }),
            source: SessionSource::ClaudeCodeCLI,
        };
        app.search.results_count = 1;
        app.search.all_groups = vec![SessionGroup {
            session_id: "sess-1".to_string(),
            file_path: "/test/session.jsonl".to_string(),
            matches: vec![test_match],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];
        app.search.groups = app.search.all_groups.clone();
        app.search.search_truncated = false;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render without truncation should not panic");

        assert!(!buffer_contains(
            terminal.backend().buffer(),
            120,
            24,
            "results may be incomplete"
        ));
    }

    #[test]
    fn test_render_recent_sessions_status_reports_hidden_sessions() {
        use crate::recent::RecentSession;

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        app.automation_filter = crate::tui::state::AutomationFilter::Manual;
        app.recent.all = vec![RecentSession {
            session_id: "sess-1".to_string(),
            file_path: "/test/session1.jsonl".to_string(),
            project: "proj".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
            summary: "Automated session".to_string(),
            automation: Some("ralphex".to_string()),
        }];
        app.recent.filtered = vec![];

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render with hidden recent sessions should not panic");

        assert!(buffer_contains(
            terminal.backend().buffer(),
            100,
            24,
            "0 recent sessions (1 hidden by filter)"
        ));
    }

    #[test]
    fn test_picker_mode_shows_pick_indicator() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = true;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render in picker mode should not panic");

        assert!(
            buffer_contains(terminal.backend().buffer(), 100, 24, "[PICK]"),
            "Status bar should contain [PICK] indicator when picker_mode is true"
        );
    }

    #[test]
    fn test_normal_mode_no_pick_indicator() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.picker_mode = false;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render in normal mode should not panic");

        assert!(
            !buffer_contains(terminal.backend().buffer(), 100, 24, "[PICK]"),
            "Status bar should NOT contain [PICK] indicator in normal mode"
        );
    }

    fn line_to_string(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn make_test_hints() -> Vec<HintItem<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        vec![
            HintItem {
                spans: vec![Span::styled("[Enter] Resume", dim)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[Ctrl+A] Project", dim)],
                min_width: 70,
            },
            HintItem {
                spans: vec![Span::styled("[Ctrl+R] Regex", dim)],
                min_width: 90,
            },
            HintItem {
                spans: vec![Span::styled("[Esc] Quit", dim)],
                min_width: 0,
            },
        ]
    }

    #[test]
    fn test_build_help_line_wide_shows_all() {
        let hints = make_test_hints();
        let line = build_help_line(&hints, 120);
        let text = line_to_string(&line);
        assert!(text.contains("[Enter] Resume"));
        assert!(text.contains("[Ctrl+A] Project"));
        assert!(text.contains("[Ctrl+R] Regex"));
        assert!(text.contains("[Esc] Quit"));
    }

    #[test]
    fn test_build_help_line_narrow_shows_only_essential() {
        let hints = make_test_hints();
        let line = build_help_line(&hints, 50);
        let text = line_to_string(&line);
        assert!(text.contains("[Enter] Resume"));
        assert!(!text.contains("[Ctrl+A] Project"));
        assert!(!text.contains("[Ctrl+R] Regex"));
        assert!(text.contains("[Esc] Quit"));
    }

    #[test]
    fn test_build_help_line_mid_range_shows_partial() {
        let hints = make_test_hints();
        let line = build_help_line(&hints, 85);
        let text = line_to_string(&line);
        assert!(text.contains("[Enter] Resume"));
        assert!(
            text.contains("[Ctrl+A] Project"),
            "min_width 70 should show at width 85"
        );
        assert!(
            !text.contains("[Ctrl+R] Regex"),
            "min_width 90 should hide at width 85"
        );
        assert!(text.contains("[Esc] Quit"));
    }

    #[test]
    fn test_build_help_line_boundary_width_equals_min_width() {
        let hints = make_test_hints();
        let line = build_help_line(&hints, 70);
        let text = line_to_string(&line);
        assert!(
            text.contains("[Ctrl+A] Project"),
            "hint with min_width 70 should show at exactly width 70"
        );
        assert!(
            !text.contains("[Ctrl+R] Regex"),
            "hint with min_width 90 should hide at width 70"
        );
    }

    #[test]
    fn test_build_help_line_empty_hints() {
        let line = build_help_line(&[], 100);
        assert!(
            line.spans.is_empty(),
            "empty hints should produce empty line"
        );
    }

    #[test]
    fn test_build_help_line_all_hints_filtered() {
        let dim = Style::default().fg(Color::DarkGray);
        let hints = vec![
            HintItem {
                spans: vec![Span::styled("[A] First", dim)],
                min_width: 80,
            },
            HintItem {
                spans: vec![Span::styled("[B] Second", dim)],
                min_width: 90,
            },
        ];
        let line = build_help_line(&hints, 50);
        assert!(
            line.spans.is_empty(),
            "all hints above threshold should produce empty line"
        );
    }

    #[test]
    fn test_build_help_line_multi_span_hint() {
        let dim = Style::default().fg(Color::DarkGray);
        let bold = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let hints = vec![
            HintItem {
                spans: vec![Span::styled("[Ctrl+H] ", dim), Span::styled("Manual", bold)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[Esc] Quit", dim)],
                min_width: 0,
            },
        ];
        let line = build_help_line(&hints, 100);
        let text = line_to_string(&line);
        assert!(
            text.contains("[Ctrl+H] "),
            "multi-span hint key label should appear"
        );
        assert!(
            text.contains("Manual"),
            "multi-span hint value label should appear"
        );
        assert!(text.contains("[Esc] Quit"));
    }

    #[test]
    fn test_build_help_line_overflow_drops_optional_highest_first() {
        let dim = Style::default().fg(Color::DarkGray);
        // Total if all shown: 14 + 2 + 16 + 2 + 17 + 2 + 10 = 63 chars
        // At width 50, min_width filter passes all (all <= 50 or 0),
        // but total overflows — optional hints should be dropped highest-first.
        let hints = vec![
            HintItem {
                spans: vec![Span::styled("[Enter] Resume", dim)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[Ctrl+A] Project", dim)],
                min_width: 40,
            },
            HintItem {
                spans: vec![Span::styled("[Ctrl+H] AllSess", dim)],
                min_width: 30,
            },
            HintItem {
                spans: vec![Span::styled("[Esc] Quit", dim)],
                min_width: 0,
            },
        ];
        let line = build_help_line(&hints, 50);
        let text = line_to_string(&line);
        // Essentials must survive
        assert!(
            text.contains("[Enter] Resume"),
            "essential hint should survive overflow"
        );
        assert!(
            text.contains("[Esc] Quit"),
            "essential [Esc] Quit must not be clipped"
        );
        // Highest optional (min_width=40) dropped first
        assert!(
            !text.contains("[Ctrl+A] Project"),
            "highest optional should be dropped first on overflow"
        );
    }

    #[test]
    fn test_build_help_line_no_overflow_when_fits() {
        let dim = Style::default().fg(Color::DarkGray);
        let hints = vec![
            HintItem {
                spans: vec![Span::styled("[A] X", dim)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[B] Y", dim)],
                min_width: 10,
            },
        ];
        // Total: 5 + 2 + 5 = 12, width 20 — fits fine
        let line = build_help_line(&hints, 20);
        let text = line_to_string(&line);
        assert!(text.contains("[A] X"));
        assert!(text.contains("[B] Y"));
    }
}
