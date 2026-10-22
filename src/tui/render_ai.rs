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

/// The agent's hits: the usual session header, then the agent's reasons,
/// one line each.
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
        for why in &hit.whys {
            let mut why_item = ListItem::new(Line::from(vec![
                Span::styled(WHY_PREFIX, Style::default().fg(DIM_FG)),
                Span::styled(
                    truncate_to_width(why, max_why),
                    Style::default().fg(PREVIEW_CONTENT_FG),
                ),
            ]));
            if is_selected {
                why_item = why_item.style(Style::default().bg(SELECTION_BG));
            }
            items.push(why_item);
        }
    }

    let mut list_state = ListState::default();
    list_state.select(selected_item_idx);
    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(Style::default());
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// The AI status, cut to `width` columns.
pub(crate) fn ai_status_span(app: &AppView, width: usize) -> Span<'static> {
    if app.ai.searching() {
        let text = if app.ai.progress.is_empty() {
            "AI searching...".to_string()
        } else {
            searching_text(&app.ai.progress, width)
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

/// `searching: "q1", "q2"` within `width` terminal columns. The newest
/// queries stay; the older ones fold into a `+N` count.
fn searching_text(queries: &[String], width: usize) -> String {
    const HEAD: &str = "searching: ";
    let quoted: Vec<String> = queries.iter().map(|q| format!("\"{q}\"")).collect();
    let mut kept = 0;
    let mut used = HEAD.len();
    for q in quoted.iter().rev() {
        let skipped = quoted.len() - kept - 1;
        let fold = if skipped > 0 {
            format!("+{skipped}, ").len()
        } else {
            0
        };
        let sep = if kept > 0 { ", ".len() } else { 0 };
        let len = columns(q) + sep;
        if used + len + fold > width {
            break;
        }
        used += len;
        kept += 1;
    }
    let skipped = quoted.len() - kept.max(1);
    let mut parts = Vec::with_capacity(kept + 1);
    if skipped > 0 {
        parts.push(format!("+{skipped}"));
    }
    parts.extend(quoted[skipped..].iter().cloned());
    cut_to_columns(&format!("{HEAD}{}", parts.join(", ")), width)
}

/// Terminal columns `s` takes: a wide (CJK, emoji) character takes two.
fn columns(s: &str) -> usize {
    Span::raw(s).width()
}

/// `s` cut to `width` terminal columns, ending in `...` when cut.
fn cut_to_columns(s: &str, width: usize) -> String {
    if columns(s) <= width {
        return s.to_string();
    }
    let room = width.saturating_sub(3);
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = columns(ch.encode_utf8(&mut [0; 4]));
        if used + w > room {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push_str(&"..."[..width.min(3)]);
    out
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
                host: crate::remote::Host::Local,
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
            whys: vec![why.to_string()],
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
        app.opened_sessions
            .insert((crate::remote::Host::Local, "aaaa1111".to_string()));
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
        let span = ai_status_span(&app.view(), 200);
        assert!(
            span.content.contains("searching: \"kitty\", \"котлин\""),
            "{}",
            span.content
        );
    }

    #[test]
    fn many_queries_narrow_width_searching_text_keeps_newest_and_counts_rest() {
        let queries: Vec<String> = (1..=8).map(|i| format!("query number {i}")).collect();
        let text = searching_text(&queries, 60);
        assert!(text.chars().count() <= 60, "{text}");
        assert!(text.starts_with("searching: +"), "{text}");
        assert!(text.ends_with("\"query number 8\""), "{text}");
        assert!(!text.contains("query number 1\""), "{text}");
    }

    #[test]
    fn queries_fit_searching_text_lists_all() {
        let queries = vec!["a".to_string(), "b".to_string()];
        assert_eq!(searching_text(&queries, 80), "searching: \"a\", \"b\"");
    }

    #[test]
    fn newest_query_wider_than_line_searching_text_truncates_it() {
        let queries = vec!["old".to_string(), "x".repeat(100)];
        let text = searching_text(&queries, 40);
        assert_eq!(text.chars().count(), 40, "{text}");
        assert!(text.starts_with("searching: +1, \"xxx"), "{text}");
    }

    #[test]
    fn wide_char_queries_searching_text_fits_display_columns() {
        let queries = vec!["日本語のテスト検索".to_string(), "絵文字🔍検索".to_string()];
        for width in [0, 2, 12, 20, 30, 45] {
            let text = searching_text(&queries, width);
            assert!(columns(&text) <= width, "{width}: {text}");
        }
        let text = searching_text(&queries, 30);
        assert!(text.starts_with("searching: +1, \"絵文字"), "{text}");
    }

    #[test]
    fn merged_hit_render_shows_every_why_under_one_header() {
        let mut merged = hit("aaaa1111", "pdf");
        merged.whys.push("pages".into());
        let app = ai_app_with_hits(vec![merged, hit("bbbb2222", "gradle")]);
        let rows = rendered_rows(&app);
        assert!(rows[0].contains("proj-aaaa1111"), "{rows:?}");
        assert!(rows[1].contains("↳ pdf"), "{rows:?}");
        assert!(rows[2].contains("↳ pages"), "{rows:?}");
        assert!(rows[3].contains("proj-bbbb2222"), "{rows:?}");
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
