//! Ctrl+G agent search: results list, status line and key hints.

use crate::tui::render_search::{
    recent_session_header_item, truncate_to_width, HintItem, DIM_FG, PREVIEW_CONTENT_FG,
    SELECTION_BG,
};
use crate::tui::view::AppView;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

const WHY_PREFIX: &str = "     ↳ ";

/// The agent's hits: the usual session header, then the agent's reason.
pub(crate) fn render_ai_hits(frame: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    let Some(hits) = app.ai.hits.as_ref() else {
        return;
    };
    let available_width = area.width as usize;
    let mut items: Vec<ListItem> = Vec::new();
    let mut selected_item_idx = None;

    for (i, hit) in hits.iter().enumerate() {
        let is_selected = i == app.ai.hit_cursor;
        if is_selected {
            selected_item_idx = Some(items.len());
        }
        let prefix = if is_selected { "> " } else { "  " };
        items.push(recent_session_header_item(
            app,
            &hit.session,
            prefix,
            is_selected,
            available_width,
        ));

        let max_why = available_width.saturating_sub(WHY_PREFIX.chars().count());
        let mut why_item = ListItem::new(Line::from(vec![
            Span::styled(WHY_PREFIX, Style::default().fg(DIM_FG)),
            Span::styled(
                truncate_to_width(&hit.why, max_why),
                Style::default().fg(PREVIEW_CONTENT_FG),
            ),
        ]));
        if is_selected {
            why_item = why_item.style(Style::default().bg(SELECTION_BG));
        }
        items.push(why_item);
    }

    let mut list_state = ListState::default();
    list_state.select(selected_item_idx);
    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(Style::default());
    frame.render_stateful_widget(list, area, &mut list_state);
}

pub(crate) fn ai_status_span(app: &AppView) -> Span<'static> {
    if app.ai.searching() {
        let text = if app.ai.progress.is_empty() {
            "AI searching...".to_string()
        } else {
            let queries: Vec<String> = app.ai.progress.iter().map(|q| format!("\"{q}\"")).collect();
            format!("searching: {}", queries.join(", "))
        };
        return Span::styled(
            text,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::ITALIC),
        );
    }
    if let Some(ref err) = app.ai.error {
        return Span::styled(format!("AI: {}", err), Style::default().fg(Color::Red));
    }
    if let Some(hits) = app.ai.hits.as_ref() {
        return Span::styled(
            format!("AI: {} sessions found", hits.len()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );
    }
    Span::styled(
        "Describe a session, Enter to search",
        Style::default().fg(Color::Magenta),
    )
}

pub(crate) fn build_ai_hints(app: &AppView) -> Vec<HintItem<'static>> {
    let labels: &[&'static str] = if app.ai.searching() {
        &["[Esc] Cancel", "[Ctrl+G] Exit"]
    } else if app.ai.hits.is_some() {
        &["[Enter] Resume", "[↑↓] Navigate", "[→] Tree", "[Esc] Back"]
    } else {
        &["[Enter] Search", "[Esc/Ctrl+G] Cancel"]
    };
    let dim = Style::default().fg(DIM_FG);
    labels
        .iter()
        .map(|label| HintItem {
            spans: vec![Span::styled(*label, dim)],
            min_width: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_search::FoundSession;
    use crate::recent::RecentSession;
    use crate::session::SessionSource;
    use crate::tui::App;
    use chrono::Utc;
    use ratatui::{backend::TestBackend, Terminal};

    fn hit(id: &str, why: &str) -> FoundSession {
        FoundSession {
            session: RecentSession {
                session_id: id.to_string(),
                file_path: format!("/p/{id}.jsonl"),
                project: format!("proj-{id}"),
                source: SessionSource::CLI,
                timestamp: Utc::now(),
                summary: "summary".to_string(),
                automation: None,
                branch: None,
                message_count: None,
                preview_role: crate::session::record::MessageRole::User,
                cwd: None,
            },
            why: why.to_string(),
        }
    }

    fn ai_app_with_hits(hits: Vec<FoundSession>) -> App {
        let mut app = App::new(vec!["/test".to_string()]);
        app.ai.active = true;
        app.ai.hits = Some(hits);
        app
    }

    fn rendered_rows(app: &App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| render_ai_hits(frame, &app.view(), frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..20)
            .map(|y| (0..100).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    fn hint_texts(hints: &[HintItem]) -> Vec<String> {
        hints
            .iter()
            .map(|h| h.spans.iter().map(|s| s.content.to_string()).collect())
            .collect()
    }

    #[test]
    fn hits_render_shows_header_and_why_line_per_hit() {
        let app = ai_app_with_hits(vec![
            hit("aaaa1111", "kotlin bump"),
            hit("bbbb2222", "gradle"),
        ]);
        let rows = rendered_rows(&app);
        assert!(rows[0].contains("proj-aaaa1111"), "{rows:?}");
        assert!(rows[1].contains("↳ kotlin bump"), "{rows:?}");
        assert!(rows[2].contains("proj-bbbb2222"), "{rows:?}");
        assert!(rows[3].contains("↳ gradle"), "{rows:?}");
    }

    #[test]
    fn opened_hit_render_shows_opened_mark() {
        let mut app = ai_app_with_hits(vec![hit("aaaa1111", "a"), hit("bbbb2222", "b")]);
        app.opened_sessions.insert("aaaa1111".to_string());
        let rows = rendered_rows(&app);
        assert!(rows[0].contains("[opened]"), "{rows:?}");
        assert!(!rows[2].contains("[opened]"), "{rows:?}");
    }

    #[test]
    fn searching_status_lists_progress_queries() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.ai.active = true;
        let (_tx, rx) = std::sync::mpsc::channel();
        app.ai.search = Some(crate::agent_search::AgentSearchHandle::for_test(rx).0);
        app.ai.progress = vec!["kitty".into(), "котлин".into()];
        let span = ai_status_span(&app.view());
        assert!(
            span.content.contains("searching: \"kitty\", \"котлин\""),
            "{}",
            span.content
        );
    }

    #[test]
    fn searching_hints_show_esc_cancel() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.ai.active = true;
        let (_tx, rx) = std::sync::mpsc::channel();
        app.ai.search = Some(crate::agent_search::AgentSearchHandle::for_test(rx).0);
        assert!(hint_texts(&build_ai_hints(&app.view())).contains(&"[Esc] Cancel".to_string()));
    }

    #[test]
    fn results_hints_show_resume_tree_back() {
        let app = ai_app_with_hits(vec![hit("aaaa1111", "a")]);
        let texts = hint_texts(&build_ai_hints(&app.view())).join(" ");
        for label in ["Resume", "Tree", "Back"] {
            assert!(texts.contains(label), "{texts}");
        }
    }

    #[test]
    fn input_hints_show_search() {
        let mut app = App::new(vec!["/test".to_string()]);
        app.ai.active = true;
        assert!(hint_texts(&build_ai_hints(&app.view())).contains(&"[Enter] Search".to_string()));
    }
}
