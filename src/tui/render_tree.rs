use crate::search::{extract_project_from_path, sanitize_content};
use crate::tui::render_search::{build_help_line, truncate_to_width, HintItem, DIM_FG};
use crate::tui::view::AppView;
use chrono::Local;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

pub(crate) fn render_tree_mode(frame: &mut Frame, app: &AppView) {
    let [header_area, tree_area, help_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // Header
    let title = if let Some(ref tree) = app.tree.session_tree {
        let sid = if tree.session_id.len() > 8 {
            &tree.session_id[..8]
        } else {
            &tree.session_id
        };
        let project = extract_project_from_path(&tree.file_path);
        format!(
            "Branch Tree: {} | {} | {} messages, {} branches",
            project,
            sid,
            tree.rows.len(),
            tree.branch_count()
        )
    } else if app.tree.tree_loading {
        "Branch Tree: Loading...".to_string()
    } else {
        "Branch Tree".to_string()
    };

    let header = Paragraph::new(title).style(
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(header, header_area);

    // Tree content
    if app.preview_mode {
        render_tree_preview(frame, app, tree_area);
    } else {
        render_tree(frame, app, tree_area);
    }

    // Help bar
    let dim = Style::default().fg(DIM_FG);
    let hints: Vec<HintItem> = if app.preview_mode {
        vec![
            HintItem {
                spans: vec![Span::styled("[Tab/Enter] Close preview", dim)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[Esc] Back", dim)],
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
                spans: vec![Span::styled("[←→] Jump branches", dim)],
                min_width: 80,
            },
            HintItem {
                spans: vec![Span::styled("[Tab] Preview", dim)],
                min_width: 70,
            },
            HintItem {
                spans: vec![Span::styled("[Enter] Resume", dim)],
                min_width: 0,
            },
            HintItem {
                spans: vec![Span::styled("[b/Esc] Back", dim)],
                min_width: 0,
            },
        ]
    };
    let help_line = build_help_line(&hints, help_area.width);
    let help = Paragraph::new(help_line);
    frame.render_widget(help, help_area);
}

fn render_tree(frame: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    // Clear area
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_style(Style::default());
            }
        }
    }

    let Some(ref tree) = app.tree.session_tree else {
        if app.tree.tree_loading {
            let loading = Paragraph::new("  Loading session tree...")
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(loading, area);
        }
        return;
    };

    if tree.rows.is_empty() {
        let empty = Paragraph::new("  No displayable messages in this session")
            .style(Style::default().fg(DIM_FG));
        frame.render_widget(empty, area);
        return;
    }

    let visible_height = area.height as usize;
    let start = app
        .tree
        .tree_scroll_offset
        .min(tree.rows.len().saturating_sub(1));
    let end = (start + visible_height).min(tree.rows.len());

    let mut items: Vec<ListItem> = Vec::new();

    for i in start..end {
        let row = &tree.rows[i];
        let is_selected = i == app.tree.tree_cursor;

        let mut spans = Vec::new();

        // Graph gutter
        let graph_style = if row.is_on_latest_chain {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(DIM_FG)
        };
        spans.push(Span::styled(&row.graph_symbols, graph_style));

        // Calculate prefix width (graph gutter) using char count for display width
        let graph_width = row.graph_symbols.chars().count();

        // Compaction events get special rendering
        if row.is_compaction {
            let compact_style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(255, 140, 0))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Rgb(255, 140, 0))
                    .add_modifier(Modifier::BOLD)
            };
            spans.push(Span::styled("~", compact_style));
            spans.push(Span::raw(" "));

            let time_str = row
                .timestamp
                .with_timezone(&Local)
                .format("%m/%d %H:%M")
                .to_string();
            spans.push(Span::styled(time_str, Style::default().fg(DIM_FG)));
            spans.push(Span::raw("  "));

            spans.push(Span::styled("[COMPACT] ", compact_style));

            // ~(1) + space(1) + time(11) + spaces(2) + [COMPACT](10) = 25
            let prefix_width = graph_width + 25;
            let max_content = (area.width as usize).saturating_sub(prefix_width);
            let preview = truncate_to_width(&row.content_preview, max_content);
            spans.push(Span::styled(preview, compact_style));
        } else {
            // Regular message rendering
            // Role indicator
            let (role_char, role_style) = if row.role == "user" {
                (
                    "U",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    "C",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            };
            spans.push(Span::styled(role_char, role_style));
            spans.push(Span::raw(" "));

            // Timestamp (compact)
            let time_str = row
                .timestamp
                .with_timezone(&Local)
                .format("%m/%d %H:%M")
                .to_string();
            spans.push(Span::styled(time_str, Style::default().fg(DIM_FG)));
            spans.push(Span::raw("  "));

            // Branch indicator
            let fork_width = if row.is_branch_point {
                spans.push(Span::styled(
                    "[fork] ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                7
            } else {
                0
            };

            // Content preview — role(1) + space(1) + time(11) + spaces(2) + fork
            let prefix_width = graph_width + 15 + fork_width;
            let max_content = (area.width as usize).saturating_sub(prefix_width);
            let preview = truncate_to_width(&row.content_preview, max_content);

            let content_style = if is_selected {
                Style::default().fg(Color::Yellow)
            } else if !row.is_on_latest_chain {
                Style::default().fg(DIM_FG)
            } else {
                Style::default().fg(Color::White)
            };
            spans.push(Span::styled(preview, content_style));
        }

        let item_style = if is_selected {
            Style::default().bg(crate::tui::render_search::SELECTION_BG)
        } else {
            Style::default()
        };
        items.push(ListItem::new(Line::from(spans)).style(item_style));
    }

    let list = List::new(items).block(Block::default().borders(Borders::NONE));
    frame.render_widget(list, area);
}

fn render_tree_preview(frame: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    // Clear area
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_style(Style::default());
            }
        }
    }

    let Some(ref tree) = app.tree.session_tree else {
        return;
    };

    let Some(row) = tree.rows.get(app.tree.tree_cursor) else {
        return;
    };

    // Load full content for preview
    let full_content = tree
        .get_full_content(&row.uuid)
        .unwrap_or_else(|| row.content_preview.clone());
    let sanitized = sanitize_content(&full_content);

    let date_str = row
        .timestamp
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let chain_status = if row.is_on_latest_chain {
        "latest chain"
    } else {
        "fork"
    };

    let mut lines = vec![
        Line::from(format!("Session: {}", tree.session_id)),
        Line::from(format!(
            "Date: {} | Role: {} | {}",
            date_str, row.role, chain_status
        )),
        Line::from(format!("UUID: {}", row.uuid)),
        Line::from("─".repeat(60)),
        Line::raw(""),
    ];

    for line in sanitized.lines() {
        lines.push(Line::raw(line.to_string()));
    }

    let preview = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(98, 98, 255)))
                .title("Message Preview"),
        )
        .style(Style::default().fg(Color::White).bg(Color::Reset))
        .wrap(ratatui::widgets::Wrap { trim: false });

    frame.render_widget(preview, area);
}

#[cfg(test)]
mod tests {
    use crate::tree::SessionTree;
    use crate::tui::render_search::render;
    use crate::tui::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::io::Write as _;
    use tempfile::TempDir;

    #[test]
    fn test_tree_mode_scroll_through_all_rows_no_crash() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);

        // Create a branched session with many messages
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            // Write 50 messages with some branches
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello start"}}]}},"uuid":"u1","sessionId":"s1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();
            for i in 2..=40 {
                let parent = if i == 21 {
                    "u10".to_string()
                } else {
                    format!("u{}", i - 1)
                }; // fork at u10
                let content = format!(
                    "Message number {} with <xml-tag>some content</xml-tag> args>",
                    i
                );
                writeln!(f, r#"{{"type":"{}","message":{{"role":"{}","content":[{{"type":"text","text":"{}"}}]}},"uuid":"u{}","parentUuid":"{}","sessionId":"s1","timestamp":"2025-01-01T00:{:02}:00Z"}}"#,
                    if i % 2 == 0 { "user" } else { "assistant" },
                    if i % 2 == 0 { "user" } else { "assistant" },
                    content, i, parent, i
                ).unwrap();
            }
        }

        let tree = SessionTree::from_file(path.to_str().unwrap()).unwrap();
        let num_rows = tree.rows.len();
        app.tree_mode = true;
        app.tree.session_tree = Some(tree);

        // Scroll down through all rows
        for _ in 0..num_rows {
            terminal.draw(|frame| render(frame, &app.view())).unwrap();
            app.on_down_tree();
        }

        // Scroll back up through all rows
        for _ in 0..num_rows {
            terminal.draw(|frame| render(frame, &app.view())).unwrap();
            app.on_up_tree();
        }

        // Jump branch points
        for _ in 0..5 {
            app.on_right_tree();
            terminal.draw(|frame| render(frame, &app.view())).unwrap();
        }
        for _ in 0..5 {
            app.on_left_tree();
            terminal.draw(|frame| render(frame, &app.view())).unwrap();
        }

        // Toggle preview mode at various positions
        app.tree.tree_cursor = 0;
        app.on_tab_tree();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();
        app.on_tab_tree();

        app.tree.tree_cursor = num_rows / 2;
        app.on_tab_tree();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();
        app.on_tab_tree();

        app.tree.tree_cursor = num_rows.saturating_sub(1);
        app.on_tab_tree();
        terminal.draw(|frame| render(frame, &app.view())).unwrap();

        // Check buffer for artifacts
        let buffer = terminal.backend().buffer();
        for cell in buffer.content() {
            let ch = cell.symbol();
            for c in ch.chars() {
                assert!(
                    !c.is_control() || c.is_whitespace(),
                    "Control char in tree buffer: {:?} (U+{:04X})",
                    ch,
                    c as u32
                );
            }
        }
    }
}
