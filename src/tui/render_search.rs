use crate::search::{
    extract_context, extract_project_from_path, sanitize_content, RipgrepMatch, SessionGroup,
};
use crate::session::record::MessageRole;
use crate::session::SessionProvider;
use crate::tui::render_tree::render_tree_mode;
use crate::tui::view::AppView;
use chrono::Local;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use regex::RegexBuilder;
use std::collections::HashSet;

/// RGB(75,0,130) — the selected-row background used across every list
/// renderer (search headers, expanded sub-matches, recent-session headers,
/// tree rows). Keep all selection styles pointing at this constant.
pub(crate) const SELECTION_BG: Color = Color::Rgb(75, 0, 130);

/// Secondary / dim foreground (theme.dim in the TUI-redesign `slate` theme,
/// `#6b7180`). Used for every unselected row, status-bar text, help hints,
/// and preview prefixes so the whole UI renders at a consistent brightness.
/// Replaces ad-hoc `Color::DarkGray` usage, whose actual shade in most
/// terminals (ANSI bright-black ≈ `#555`) is too muddy against the dark
/// panel and mis-matched the design handoff.
pub(crate) const DIM_FG: Color = Color::Rgb(107, 113, 128);

/// Project chip foreground from the slate handoff accent (`#6262ff`).
const PROJECT_FG: Color = Color::Rgb(98, 98, 255);
/// Subtle project chip background. Dark enough to preserve contrast on the
/// default terminal background while still separating the project from pipes.
const PROJECT_BG: Color = Color::Rgb(27, 31, 39);
/// Project chip background used on the selected purple row.
const SELECTED_PROJECT_BG: Color = Color::Rgb(94, 36, 130);
/// Branch marker foreground from the handoff cyan (`#56c2ff`).
const BRANCH_FG: Color = Color::Rgb(86, 194, 255);

/// Pre-built preview prefixes shared by `render_groups` (collapsed groups)
/// and `render_recent_sessions`. Hoisted so the render hot path does not
/// `format!`/walk them every frame.
const PREVIEW_PREFIX_USER: &str = "     User: ";
const PREVIEW_PREFIX_CLAUDE: &str = "     Claude: ";
/// Both PREVIEW_PREFIX_* literals are 11 ASCII bytes — no chars() walk needed.
const PREVIEW_PREFIX_LEN: usize = 11;
const AI_UNRANKED_SEPARATOR_LABEL: &str = "── Unranked below ──";

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

pub(crate) fn build_ai_hints(ranked_count: Option<usize>) -> Vec<HintItem<'static>> {
    let dim = Style::default().fg(DIM_FG);
    let enter_label = if ranked_count.is_some() {
        "[Enter] Resume"
    } else {
        "[Enter] AI Rank"
    };
    vec![
        HintItem {
            spans: vec![Span::styled(enter_label, dim)],
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
}

fn ai_unranked_separator_at(ranked_count: Option<usize>, total_items: usize) -> Option<usize> {
    ranked_count.filter(|&n| n > 0 && n < total_items)
}

fn build_ai_unranked_separator() -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        AI_UNRANKED_SEPARATOR_LABEL,
        Style::default().fg(DIM_FG),
    )))
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
        Span::styled(text, Style::default().fg(DIM_FG))
    } else if let Some(text) = recent_sessions_status_text(app) {
        let style = if app.recent.loading {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default().fg(DIM_FG)
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
        AF::All | AF::Manual => Style::default().fg(DIM_FG),
        AF::Auto => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    };

    let dim = Style::default().fg(DIM_FG);

    let hints: Vec<HintItem> = if app.ai.active {
        build_ai_hints(app.ai.ranked_count)
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

    // Pre-compile regex and lowercase query once for preview matching
    let preview_regex = if app.regex_mode && !app.search.results_query.is_empty() {
        RegexBuilder::new(&app.search.results_query)
            .case_insensitive(true)
            .build()
            .ok()
    } else {
        None
    };
    let query_lower = app.search.results_query.to_lowercase();

    // Pre-computed width budget for the collapsed preview. Both
    // PREVIEW_PREFIX_* constants are fixed-length; centring context around
    // the match depends on the query length, which is also stable across
    // the whole draw pass.
    let area_width = area.width as usize;
    let preview_max_content = area_width.saturating_sub(PREVIEW_PREFIX_LEN);
    let query_chars = app.search.results_query.chars().count();
    let preview_context_chars = preview_max_content
        .saturating_sub(query_chars)
        .saturating_sub(6) // two "..." markers
        / 2;
    let preview_context_chars = preview_context_chars.max(1);

    // Only build ListItems for groups in the visible slice — every
    // collapsed row goes through `sanitize_single_line` +
    // `highlight_line_with_base`, and paying for all groups while only
    // ~10 fit on screen made touchpad-scroll flicks visibly laggy on
    // large result sets. Centre the window on the cursor so scrolling
    // feels symmetric in both directions.
    let visible_height = area.height as usize;
    let rows_per_collapsed = 2usize;
    let max_visible = (visible_height / rows_per_collapsed).max(1);
    let cursor_group = app.search.group_cursor;
    let total_groups = app.search.groups.len();
    let window_cap = max_visible.min(total_groups);
    let window_start = cursor_group
        .saturating_sub(max_visible / 2)
        .min(total_groups.saturating_sub(window_cap));
    let window_end = (window_start + max_visible).min(total_groups);

    // `sort_by_key` in `handle_ai_result` puts ranked IDs first, so
    // `ranked_count` is the boundary between ranked and untouched results.
    let ai_separator_at = ai_unranked_separator_at(app.ai.ranked_count, total_groups);

    for (i, group) in app
        .search
        .groups
        .iter()
        .enumerate()
        .skip(window_start)
        .take(window_end - window_start)
    {
        if ai_separator_at == Some(i) {
            items.push(build_ai_unranked_separator());
        }

        let is_selected = i == cursor_group;
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
            let matches_iter = || group.matches.iter().filter_map(|m| m.message.as_ref());
            // Phase 1: prefer message where text_content contains the query
            let preview_msg = if !app.search.results_query.is_empty() {
                matches_iter().find(|msg| {
                    if msg.text_content.trim().is_empty() {
                        return false;
                    }
                    if app.regex_mode {
                        preview_regex
                            .as_ref()
                            .is_some_and(|re| re.is_match(&msg.text_content))
                    } else {
                        msg.text_content.to_lowercase().contains(&query_lower)
                    }
                })
            } else {
                None
            };
            // Phase 2: first with non-empty text_content
            let preview_msg = preview_msg
                .or_else(|| matches_iter().find(|msg| !msg.text_content.trim().is_empty()));
            // Phase 3: first with non-empty content
            let preview_msg =
                preview_msg.or_else(|| matches_iter().find(|msg| !msg.content.trim().is_empty()));
            if let Some(msg) = preview_msg {
                let query = &app.search.results_query;
                let text_content_matches = if query.is_empty() {
                    true
                } else if app.regex_mode {
                    preview_regex
                        .as_ref()
                        .is_some_and(|re| re.is_match(&msg.text_content))
                } else {
                    msg.text_content.to_lowercase().contains(&query_lower)
                };
                let preview_text = if msg.text_content.trim().is_empty() {
                    &msg.content
                } else if !text_content_matches {
                    // text_content doesn't contain the search query (match is
                    // likely in a tool_result block) — fall back to full content
                    // so the matched portion is visible in the preview.
                    &msg.content
                } else {
                    &msg.text_content
                };
                let content = sanitize_single_line(preview_text);
                let preview_prefix = if msg.role == "user" {
                    PREVIEW_PREFIX_USER
                } else {
                    PREVIEW_PREFIX_CLAUDE
                };
                // Centre the preview on the first query occurrence so the
                // match stays visible even when it sits past column 120 in a
                // long message. `render_sub_match` does the same for
                // expanded rows.
                let centered = if query.is_empty() {
                    content
                } else {
                    extract_context(&content, query, preview_context_chars)
                };
                let truncated = truncate_to_width(&centered, preview_max_content);
                // Selected preview keeps DIM_FG on top of SELECTION_BG — the
                // header above uses selFg/yellow so the selected element
                // reads as a two-part row, not a monochrome block. Purple
                // trailing cells come from the outer ListItem style below;
                // we do NOT also paint bg on the span base to avoid
                // double-styling every styled cell.
                let base = Style::default().fg(DIM_FG);
                let mut spans = vec![Span::styled(preview_prefix, base)];
                let highlighted = highlight_line_with_base(&truncated, query, base);
                spans.extend(highlighted.spans);
                let mut preview_item = ListItem::new(Line::from(spans));
                if is_selected {
                    preview_item = preview_item.style(Style::default().bg(SELECTION_BG));
                }
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

/// Row grammar used by `render_recent_sessions`: one header line followed by
/// one dim preview line (`User:` / `Claude:` prefix + summary). Keep in sync
/// with `render_groups` so the two screens read as one continuous surface.
const RECENT_LINES_PER_SESSION: usize = 2;

/// Shared header layout used by both collapsed search groups and recent
/// sessions: `<caret> [provider] [source] date | project | branch | sid (count)[ [A]]`.
/// `session_id` is truncated to 8 chars here so callers don't have to.
struct SessionHeaderParts<'a> {
    caret: &'a str,
    provider: &'a str,
    source: &'a str,
    date: &'a str,
    project: &'a str,
    branch: &'a str,
    session_id: &'a str,
    count: &'a str,
    has_automation: bool,
}

#[cfg(test)]
fn format_session_header_line(p: SessionHeaderParts<'_>) -> String {
    let sid = if p.session_id.len() > 8 {
        &p.session_id[..8]
    } else {
        p.session_id
    };
    let auto_tag = if p.has_automation { " [A]" } else { "" };
    format!(
        "{} [{}] [{}] {} | {} | {} | {} ({}){}",
        p.caret, p.provider, p.source, p.date, p.project, p.branch, sid, p.count, auto_tag
    )
}

fn truncated_session_id(session_id: &str) -> &str {
    if session_id.len() > 8 {
        &session_id[..8]
    } else {
        session_id
    }
}

fn style_with_bg(style: Style, bg: Option<Color>) -> Style {
    match bg {
        Some(bg) => style.bg(bg),
        None => style,
    }
}

/// Styled header line used by search results and recent sessions. It keeps
/// the same text grammar as `format_session_header_line`, but gives the
/// scannable fields their own visual hierarchy: source, project, branch, and
/// count no longer blend into the timestamp/session-id scaffolding.
fn render_session_header_line(
    prefix: &str,
    p: SessionHeaderParts<'_>,
    base_style: Style,
    selected: bool,
) -> Line<'static> {
    let row_bg = base_style.bg;
    let base = base_style;
    let accent = if selected {
        base
    } else {
        Style::default().fg(Color::Magenta)
    };
    let provider = if selected {
        base
    } else if p.provider == "Codex" {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Magenta)
    };
    let project = if selected {
        Style::default()
            .fg(Color::Yellow)
            .bg(SELECTED_PROJECT_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(PROJECT_FG)
            .bg(PROJECT_BG)
            .add_modifier(Modifier::BOLD)
    };
    let branch = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(BRANCH_FG).add_modifier(Modifier::BOLD)
    };
    let count = if selected {
        base
    } else {
        Style::default().fg(Color::Green)
    };

    let mut spans = vec![
        Span::styled(prefix.to_string(), base),
        Span::styled(p.caret.to_string(), base),
        Span::styled(" ".to_string(), base),
        Span::styled(
            format!("[{}] ", p.provider),
            style_with_bg(provider, row_bg),
        ),
        Span::styled(format!("[{}]", p.source), style_with_bg(accent, row_bg)),
        Span::styled(format!(" {} | ", p.date), base),
        Span::styled(p.project.to_string(), project),
        Span::styled(" | ".to_string(), base),
    ];

    if p.branch == "-" {
        spans.push(Span::styled("-".to_string(), base));
    } else {
        spans.push(Span::styled(
            format!("⎇ {}", p.branch),
            style_with_bg(branch, row_bg),
        ));
    }

    spans.extend([
        Span::styled(format!(" | {} ", truncated_session_id(p.session_id)), base),
        Span::styled(format!("({})", p.count), style_with_bg(count, row_bg)),
    ]);

    if p.has_automation {
        let automation = if selected {
            base
        } else {
            Style::default().fg(Color::Yellow)
        };
        spans.push(Span::styled(
            " [A]".to_string(),
            style_with_bg(automation, row_bg),
        ));
    }

    Line::from(spans)
}

/// Recent-session header text. Uses `▶` (non-expandable) and a `N msgs` tail.
#[cfg(test)]
pub(crate) fn build_recent_session_header_text(session: &crate::recent::RecentSession) -> String {
    let count_str = match session.message_count {
        Some(n) => format!("{} msgs", n),
        None => "-".to_string(),
    };
    format_session_header_line(SessionHeaderParts {
        caret: "▶",
        provider: SessionProvider::from_path(&session.file_path).display_name(),
        source: session.source.display_name(),
        date: &session
            .timestamp
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        project: &session.project,
        branch: session.branch.as_deref().unwrap_or("-"),
        session_id: &session.session_id,
        count: &count_str,
        has_automation: session.automation.is_some(),
    })
}

fn render_recent_sessions(frame: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    // Loading and empty states are shown in the status bar only
    if app.recent.loading || app.recent.filtered.is_empty() {
        return;
    }

    let visible_height = area.height as usize;
    let available_width = area.width as usize;
    let mut items: Vec<ListItem> = vec![];

    // Use pre-computed scroll offset (adjusted in event handlers, not here).
    // scroll_offset is a *session* index; each session consumes two list items.
    let scroll_offset = app.recent.scroll_offset;
    let visible_items = (visible_height / RECENT_LINES_PER_SESSION).max(1);
    let end = (scroll_offset + visible_items).min(app.recent.filtered.len());
    let ai_separator_at = ai_unranked_separator_at(app.ai.ranked_count, app.recent.filtered.len());

    let mut selected_item_idx: Option<usize> = None;

    for i in scroll_offset..end {
        if ai_separator_at == Some(i) {
            items.push(build_ai_unranked_separator());
        }

        let session = &app.recent.filtered[i];
        let is_selected = i == app.recent.cursor;

        let prefix = if is_selected { "> " } else { "  " };
        // Selected style intentionally omits `Modifier::BOLD`: in some
        // monospace fonts (e.g. Iosevka) the bold variant of the `▶` glyph
        // is narrower than the regular one, which makes the selected row's
        // caret look visually clipped next to the solid purple background.
        let header_style = if is_selected {
            Style::default().fg(Color::Yellow).bg(SELECTION_BG)
        } else {
            Style::default().fg(DIM_FG)
        };

        if is_selected {
            selected_item_idx = Some(items.len());
        }
        let count_str = match session.message_count {
            Some(n) => format!("{} msgs", n),
            None => "-".to_string(),
        };
        let date = session
            .timestamp
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let header = render_session_header_line(
            prefix,
            SessionHeaderParts {
                caret: "▶",
                provider: SessionProvider::from_path(&session.file_path).display_name(),
                source: session.source.display_name(),
                date: &date,
                project: &session.project,
                branch: session.branch.as_deref().unwrap_or("-"),
                session_id: &session.session_id,
                count: &count_str,
                has_automation: session.automation.is_some(),
            },
            header_style,
            is_selected,
        );
        items.push(ListItem::new(header).style(header_style));

        // Preview line — same `     User:/Claude: <text>` grammar as the
        // collapsed search preview. Purple selection bg comes from the outer
        // ListItem style when selected; span base only carries fg so we
        // don't double-style every cell.
        let preview_prefix = match session.preview_role {
            MessageRole::User => PREVIEW_PREFIX_USER,
            MessageRole::Assistant => PREVIEW_PREFIX_CLAUDE,
        };
        let max_content = available_width.saturating_sub(PREVIEW_PREFIX_LEN);
        let preview_content = truncate_to_width(&session.summary, max_content);
        let preview_style = Style::default().fg(DIM_FG);
        let mut preview_item = ListItem::new(Line::from(vec![
            Span::styled(preview_prefix, preview_style),
            Span::styled(preview_content, preview_style),
        ]));
        if is_selected {
            preview_item = preview_item.style(Style::default().bg(SELECTION_BG));
        }
        items.push(preview_item);
    }

    let mut list_state = ListState::default();
    if let Some(idx) = selected_item_idx {
        list_state.select(Some(idx));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(Style::default());
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Search-group header text. Uses `▶`/`▼` (collapsed vs expanded) and a
/// `N/M matches` tail (with `+` suffix for compacted sessions).
#[cfg(test)]
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

    let count_str = match group.message_count {
        Some(total) => {
            let suffix = if group.message_count_compacted {
                "+"
            } else {
                ""
            };
            format!("{}/{}{} matches", group.matches.len(), total, suffix)
        }
        None => format!("{} matches", group.matches.len()),
    };

    format_session_header_line(SessionHeaderParts {
        caret: if expanded { "▼" } else { "▶" },
        provider: SessionProvider::from_path(&group.file_path).display_name(),
        source,
        date: &date_str,
        project: &extract_project_from_path(&group.file_path),
        branch: &branch,
        session_id: &group.session_id,
        count: &count_str,
        has_automation: group.automation.is_some(),
    })
}

fn render_group_header<'a>(group: &SessionGroup, selected: bool, expanded: bool) -> ListItem<'a> {
    // No BOLD on selected+collapsed — see `render_recent_sessions` for the
    // Iosevka ▶ clipping rationale; both screens must stay in sync.
    let style = if selected && !expanded {
        Style::default().fg(Color::Yellow).bg(SELECTION_BG)
    } else if selected {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(DIM_FG)
    };

    let prefix = if selected { "> " } else { "  " };
    let first_match = group.first_match();
    let (date_str, branch, source) = if let Some(m) = first_match {
        let source = m.source.display_name();
        if let Some(ref msg) = m.message {
            let date = msg
                .timestamp
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string();
            let branch = msg.branch.clone().unwrap_or_else(|| "-".to_string());
            (date, branch, source)
        } else {
            ("-".to_string(), "-".to_string(), source)
        }
    } else {
        ("-".to_string(), "-".to_string(), "CLI")
    };

    let count_str = match group.message_count {
        Some(total) => {
            let suffix = if group.message_count_compacted {
                "+"
            } else {
                ""
            };
            format!("{}/{}{} matches", group.matches.len(), total, suffix)
        }
        None => format!("{} matches", group.matches.len()),
    };

    let project = extract_project_from_path(&group.file_path);
    let header = render_session_header_line(
        prefix,
        SessionHeaderParts {
            caret: if expanded { "▼" } else { "▶" },
            provider: SessionProvider::from_path(&group.file_path).display_name(),
            source,
            date: &date_str,
            project: &project,
            branch: &branch,
            session_id: &group.session_id,
            count: &count_str,
            has_automation: group.automation.is_some(),
        },
        style,
        selected,
    );

    ListItem::new(header).style(style)
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
        // Strip ANSI + collapse newlines: sub-match rows are single-line
        // ListItems, so any embedded newline would wrap and shift every
        // subsequent row off by one.
        let sanitized = sanitize_single_line(&msg.content);
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
        Style::default().fg(Color::Yellow).bg(SELECTION_BG)
    } else {
        Style::default().fg(DIM_FG)
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
    spans.push(Span::styled(" \"", style));
    // Highlight span (yellow bg + black fg + bold) overrides the row's
    // base style, so matches stay visible on both unselected and selected
    // rows.
    let highlighted = highlight_line_with_base(&content, query, style);
    spans.extend(highlighted.spans);
    spans.push(Span::styled("\"", style));

    ListItem::new(Line::from(spans))
}

/// Sanitize for display inside a single-line `ListItem`. On top of
/// `sanitize_content` (strips ANSI), collapse every `\n` / `\t` / `\r` into a
/// single space so the resulting `Span` cannot make the list row wrap and
/// push every subsequent header down by one visual row.
fn sanitize_single_line(content: &str) -> String {
    let sanitized = sanitize_content(content);
    sanitized
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\t' | '\r') {
                ' '
            } else {
                c
            }
        })
        .collect()
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
fn highlight_line(text: &str, query: &str) -> Line<'static> {
    highlight_line_with_base(text, query, Style::default())
}

/// Same as `highlight_line` but paints non-matched spans with `base_style`.
/// Use it when the surrounding context already has a colour (e.g. `DIM_FG`
/// for collapsed previews, or yellow-on-purple for the selected sub-match)
/// and you still want matches of `query` to pop on their own yellow
/// background.
///
/// Returns `Line<'static>` because every span owns its content via
/// `.to_string()` — the result never borrows from `text`.
fn highlight_line_with_base(text: &str, query: &str, base_style: Style) -> Line<'static> {
    if query.is_empty() {
        return Line::from(Span::styled(text.to_string(), base_style));
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
            spans.push(Span::styled(
                text[last_end..match_start].to_string(),
                base_style,
            ));
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
        spans.push(Span::styled(text[last_end..].to_string(), base_style));
    }

    if spans.is_empty() {
        Line::from(Span::styled(text.to_string(), base_style))
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
            text_content: "Test content for preview".to_string(),
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

    fn make_test_app_with_n_groups(n: usize) -> App {
        let mut app = App::new(vec!["/test".to_string()]);
        app.search.groups = (0..n)
            .map(|i| {
                let file_path =
                    format!("/path/to/projects/-Users-test-projects-myapp/session-{i}.jsonl");
                let msg = Message {
                    session_id: format!("s{i}"),
                    role: "user".to_string(),
                    content: format!("Content {i}"),
                    text_content: format!("Content {i}"),
                    timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, i as u32, 0).unwrap(),
                    line_number: 1,
                    ..Default::default()
                };
                SessionGroup {
                    session_id: format!("s{i}"),
                    file_path: file_path.clone(),
                    matches: vec![RipgrepMatch {
                        file_path,
                        message: Some(msg),
                        source: SessionSource::ClaudeCodeCLI,
                    }],
                    automation: None,
                    message_count: None,
                    message_count_compacted: false,
                }
            })
            .collect();
        app.search.results_query = String::new();
        app
    }

    fn make_test_app_with_n_recent_sessions(n: usize) -> App {
        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        app.recent.filtered = (0..n)
            .map(|i| crate::recent::RecentSession {
                session_id: format!("recent-{i}"),
                file_path: format!("/test/recent-{i}.jsonl"),
                project: "proj".to_string(),
                source: SessionSource::ClaudeCodeCLI,
                timestamp: Utc.with_ymd_and_hms(2025, 1, 9, 10, i as u32, 0).unwrap(),
                summary: format!("Recent summary {i}"),
                automation: None,
                branch: Some("main".to_string()),
                message_count: Some(i + 1),
                preview_role: MessageRole::User,
            })
            .collect();
        app
    }

    #[test]
    fn render_groups_shows_unranked_separator_only_on_partial_rank() {
        for (ranked, expect) in [
            (None, false),
            (Some(0), false),
            (Some(3), false), // Some(total_groups)
            (Some(1), true),
            (Some(2), true),
        ] {
            let mut app = make_test_app_with_n_groups(3);
            app.ai.ranked_count = ranked;

            let backend = TestBackend::new(120, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| render(frame, &app.view()))
                .expect("render must not panic");

            assert_eq!(
                buffer_contains(
                    terminal.backend().buffer(),
                    120,
                    24,
                    AI_UNRANKED_SEPARATOR_LABEL
                ),
                expect,
                "ranked_count = {ranked:?}: separator presence mismatch",
            );
        }
    }

    #[test]
    fn render_recent_sessions_shows_unranked_separator_only_on_partial_rank() {
        for (ranked, expect) in [
            (None, false),
            (Some(0), false),
            (Some(3), false), // Some(total_sessions)
            (Some(1), true),
            (Some(2), true),
        ] {
            let mut app = make_test_app_with_n_recent_sessions(3);
            app.ai.ranked_count = ranked;

            let backend = TestBackend::new(120, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| render(frame, &app.view()))
                .expect("render must not panic");

            assert_eq!(
                buffer_contains(
                    terminal.backend().buffer(),
                    120,
                    24,
                    AI_UNRANKED_SEPARATOR_LABEL
                ),
                expect,
                "recent ranked_count = {ranked:?}: separator presence mismatch",
            );
        }
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
    fn test_highlight_line_with_base_paints_non_matches() {
        let base = Style::default().fg(DIM_FG);
        let line = highlight_line_with_base("Hello world", "world", base);
        assert_eq!(line.spans.len(), 2);
        // Non-matched span must carry the base style so the preview row
        // reads as dim rather than white.
        assert_eq!(line.spans[0].content.as_ref(), "Hello ");
        assert_eq!(line.spans[0].style.fg, Some(DIM_FG));
        // Matched span ignores the base and uses the fixed highlight style.
        assert_eq!(line.spans[1].content.as_ref(), "world");
        assert_eq!(line.spans[1].style.fg, Some(Color::Black));
        assert_eq!(line.spans[1].style.bg, Some(Color::Yellow));
    }

    #[test]
    fn test_highlight_line_with_base_styles_no_match() {
        let base = Style::default().fg(DIM_FG);
        let line = highlight_line_with_base("Hello world", "xyz", base);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].style.fg, Some(DIM_FG));
    }

    #[test]
    fn test_render_groups_collapsed_preview_highlights_query() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let app = make_test_app_with_groups();

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        let buffer = terminal.backend().buffer();

        let mut preview_y: Option<u16> = None;
        for y in 0..24 {
            let mut line = String::new();
            for x in 0..120 {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            if line.starts_with("     User: ") {
                preview_y = Some(y);
                break;
            }
        }
        let y = preview_y.expect("collapsed preview line not found");

        assert!(
            (0..120).any(|x| {
                let cell = buffer.cell((x, y)).unwrap();
                cell.bg == Color::Yellow && cell.fg == Color::Black
            }),
            "collapsed preview line must contain at least one highlighted cell"
        );
    }

    /// A query match deep inside the message (past column 120) must still
    /// render highlighted in the collapsed preview — otherwise the preview
    /// renders its leading content and the user sees no match.
    #[test]
    fn test_render_groups_collapsed_preview_centers_on_deep_match() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.search.results_query = "NEEDLE".to_string();

        // Place the match 500 characters into the content — well past any
        // reasonable terminal width.
        let prefix = "x".repeat(500);
        let suffix = "y".repeat(500);
        let content = format!("{}NEEDLE{}", prefix, suffix);

        let msg = Message {
            session_id: "deep-match".to_string(),
            role: "user".to_string(),
            content: content.clone(),
            text_content: content,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap(),
            branch: Some("main".to_string()),
            line_number: 1,
            ..Default::default()
        };
        app.search.groups = vec![SessionGroup {
            session_id: "deep-match".to_string(),
            file_path: "/path/to/projects/-Users-test-projects-deep/session.jsonl".to_string(),
            matches: vec![RipgrepMatch {
                file_path: "/path/to/projects/-Users-test-projects-deep/session.jsonl".to_string(),
                message: Some(msg),
                source: SessionSource::ClaudeCodeCLI,
            }],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        }];

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        let buffer = terminal.backend().buffer();

        let mut preview_y: Option<u16> = None;
        for y in 0..24 {
            let mut line = String::new();
            for x in 0..120 {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            if line.starts_with("     User: ") {
                preview_y = Some(y);
                break;
            }
        }
        let y = preview_y.expect("collapsed preview line not found");

        assert!(
            (0..120).any(|x| {
                let cell = buffer.cell((x, y)).unwrap();
                cell.bg == Color::Yellow && cell.fg == Color::Black
            }),
            "collapsed preview must centre on the NEEDLE match and render it \
             highlighted, even when the match sits deep inside the content"
        );
    }

    /// Red-test for the "rendering goes wonky when scrolling below the
    /// visible area" bug. A preview whose content contains a `\n` turns the
    /// corresponding Line into a multi-row ListItem, which shifts every
    /// subsequent header by one visual row; with many groups + a cursor
    /// near the end the user sees leaked preview text at the start of a
    /// later header row (e.g. `"ension6I-...26-04-11 20:27 | …"` — the
    /// wrapped tail of the previous preview overwriting the `  ▶ [Claude] [CLI] ` of
    /// the next header).
    ///
    /// Expectation: every header row in the rendered buffer starts cleanly
    /// with `  ▶ [` or `> ▶ [`, regardless of newlines / tabs / carriage
    /// returns inside preview content.
    #[test]
    fn test_render_groups_preview_newlines_do_not_shift_next_header() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.search.results_query = "needle".to_string();

        // Build many groups, each with a preview that contains \n / \t / \r.
        // Scrolling the cursor through the list must not leak the tails of
        // previous previews into the next header row.
        let make_group = |idx: usize, content: String| SessionGroup {
            session_id: format!("sess-{:08}", idx),
            file_path: format!("/p/grp-{:02}.jsonl", idx),
            matches: vec![RipgrepMatch {
                file_path: format!("/p/grp-{:02}.jsonl", idx),
                message: Some(Message {
                    session_id: format!("sess-{:08}", idx),
                    role: "user".to_string(),
                    content: content.clone(),
                    text_content: content,
                    timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
                    line_number: 1,
                    ..Default::default()
                }),
                source: SessionSource::ClaudeCodeCLI,
            }],
            automation: None,
            message_count: None,
            message_count_compacted: false,
        };
        app.search.groups = (0..20)
            .map(|i| {
                make_group(
                    i,
                    format!("needle-{i}\npart-two\ttabbed\rreturned part-three more text"),
                )
            })
            .collect();
        app.search.group_cursor = 0;
        app.search.expanded = false;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        let buffer = terminal.backend().buffer();

        // No cell in the list area may carry a `\n`, `\r`, or `\t` as its
        // symbol. Those bytes make the real terminal jump the cursor (tab
        // stop / CR) when ratatui flushes the buffer, which is how the
        // preview tail ends up overwriting the next header row. TestBackend
        // stores the raw symbols without interpreting them, so we have to
        // assert on the stored cells directly.
        for y in 0..24 {
            for x in 0..120 {
                let sym = buffer.cell((x, y)).unwrap().symbol();
                assert!(
                    !sym.contains('\n') && !sym.contains('\r') && !sym.contains('\t'),
                    "cell at ({},{}) carries a control char that will break terminal rendering: {:?}",
                    x,
                    y,
                    sym
                );
            }
        }
    }

    /// Red-test for touchpad-scroll latency. Simulates the full per-event
    /// work path (handle scroll key → `tick()` → render) and asserts that a
    /// burst of 30 rapid scroll events (the order of magnitude a fast
    /// touchpad flick dispatches) completes within the wall-clock budget
    /// the user perceives as "not laggy".
    ///
    /// On the loaded path `tick()` currently drains the background
    /// message-count channel on every iteration and, if anything arrived,
    /// rebuilds `app.search.groups` via `apply_groups_filter` — which
    /// `clones` every `SessionGroup` (including all `Message` strings). With
    /// 500 groups × 5 matches × ~500-char content each event ends up
    /// cloning tens of MB, turning a scroll flick into a visible wait.
    #[test]
    fn test_scroll_burst_under_background_updates_is_snappy() {
        use std::sync::mpsc;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.search.results_query = "needle".to_string();
        let long_body = "filler text needle ".repeat(50);
        let mut groups = Vec::with_capacity(500);
        for gi in 0..500 {
            let mut matches = Vec::with_capacity(5);
            for mi in 0..5 {
                matches.push(RipgrepMatch {
                    file_path: format!("/p/g{:04}.jsonl", gi),
                    message: Some(Message {
                        session_id: format!("sess-{:04}", gi),
                        role: if mi % 2 == 0 { "user" } else { "assistant" }.to_string(),
                        content: long_body.clone(),
                        text_content: long_body.clone(),
                        timestamp: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
                        line_number: mi + 1,
                        ..Default::default()
                    }),
                    source: SessionSource::ClaudeCodeCLI,
                });
            }
            groups.push(SessionGroup {
                session_id: format!("sess-{:04}", gi),
                file_path: format!("/p/g{:04}.jsonl", gi),
                matches,
                automation: None,
                message_count: None,
                message_count_compacted: false,
            });
        }
        app.search.all_groups = groups.clone();
        app.search.groups = groups;
        app.search.group_cursor = 0;
        app.search.expanded = false;

        // Attach a channel that delivers a *trickle* of background message
        // counts — one update right before each scroll tick, matching the
        // real-world pattern where per-file counts arrive one by one.
        let (tx, rx) = mpsc::channel();
        app.search.message_count_rx = Some(rx);

        // Warm up.
        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("warm-up render should not panic");

        const ITERS: u32 = 30;
        let start = std::time::Instant::now();
        for i in 0..ITERS {
            tx.send((format!("/p/g{:04}.jsonl", i), (i as usize + 1) * 100, false))
                .unwrap();
            app.on_down();
            app.tick();
            terminal
                .draw(|frame| render(frame, &app.view()))
                .expect("render should not panic");
        }
        let elapsed = start.elapsed();

        let budget = std::time::Duration::from_millis(500);
        assert!(
            elapsed < budget,
            "{} scroll events with trickling background updates took {:?}, expected < {:?} — tick()/render path is doing too much work per event",
            ITERS, elapsed, budget
        );
    }

    #[test]
    fn test_render_sub_match_highlights_query_in_content() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();
        app.search.group_cursor = 0;
        app.search.expanded = true;
        app.search.sub_cursor = 0;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        let buffer = terminal.backend().buffer();

        let mut match_y: Option<u16> = None;
        for y in 0..24 {
            if buffer.cell((4, y)).unwrap().symbol() == "→" {
                match_y = Some(y);
                break;
            }
        }
        let y = match_y.expect("expanded sub-match row not found");

        assert!(
            (0..120).any(|x| {
                let cell = buffer.cell((x, y)).unwrap();
                cell.bg == Color::Yellow && cell.fg == Color::Black
            }),
            "expanded sub-match row must contain at least one highlighted cell"
        );
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
        assert!(
            text.contains("[Claude]"),
            "Header should contain [Claude] provider indicator, got: {}",
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
        assert!(
            text.contains("[Claude]"),
            "Header should contain [Claude] provider indicator, got: {}",
            text
        );
    }

    #[test]
    fn test_build_group_header_shows_codex_provider() {
        let msg = Message {
            session_id: "test-session".to_string(),
            role: "user".to_string(),
            content: "Test content".to_string(),
            timestamp: Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap(),
            branch: None,
            line_number: 1,
            ..Default::default()
        };

        let m = RipgrepMatch {
            file_path:
                "/Users/test/.codex/sessions/2026/05/01/rollout-2026-05-01T10-00-00-session.jsonl"
                    .to_string(),
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
            text.contains("[Codex]"),
            "Header should contain [Codex] provider indicator, got: {}",
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
                branch: Some("main".to_string()),
                message_count: Some(42),
                preview_role: crate::session::record::MessageRole::User,
            },
            RecentSession {
                session_id: "sess-2".to_string(),
                file_path: "/test/session2.jsonl".to_string(),
                project: "other-app".to_string(),
                source: SessionSource::ClaudeCodeCLI,
                timestamp: Utc.with_ymd_and_hms(2025, 5, 31, 9, 0, 0).unwrap(),
                summary: "Add new feature".to_string(),
                automation: None,
                branch: None,
                message_count: Some(12),
                preview_role: crate::session::record::MessageRole::Assistant,
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
    fn test_build_recent_session_header_text_cli_with_branch() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let ts = Utc.with_ymd_and_hms(2026, 4, 17, 19, 51, 0).unwrap();
        let date = ts
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let session = RecentSession {
            session_id: "b701e752abcd1234".to_string(),
            file_path: "/projects/avito/sess.jsonl".to_string(),
            project: "avito-android".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: ts,
            summary: "hello".to_string(),
            automation: Some("ralphex".to_string()),
            branch: Some("MBSA-2197".to_string()),
            message_count: Some(3613),
            preview_role: MessageRole::User,
        };
        assert_eq!(
            build_recent_session_header_text(&session),
            format!(
                "▶ [Claude] [CLI] {date} | avito-android | MBSA-2197 | b701e752 (3613 msgs) [A]"
            )
        );
    }

    #[test]
    fn test_build_recent_session_header_text_desktop_without_branch() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let ts = Utc.with_ymd_and_hms(2026, 3, 18, 18, 20, 0).unwrap();
        let date = ts
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let session = RecentSession {
            session_id: "short-id".to_string(),
            file_path: "/sessions/desktop.jsonl".to_string(),
            project: "~".to_string(),
            source: SessionSource::ClaudeDesktop,
            timestamp: ts,
            summary: "hello".to_string(),
            automation: None,
            branch: None,
            message_count: Some(42),
            preview_role: MessageRole::Assistant,
        };
        assert_eq!(
            build_recent_session_header_text(&session),
            format!("▶ [Claude] [Desktop] {date} | ~ | - | short-id (42 msgs)")
        );
    }

    #[test]
    fn test_build_recent_session_header_text_codex_provider() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let ts = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let date = ts
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let session = RecentSession {
            session_id: "019f-codex-session".to_string(),
            file_path: "/Users/test/.codex/sessions/2026/05/01/rollout-2026-05-01T12-00-00-019f-codex-session.jsonl".to_string(),
            project: "myapp".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: ts,
            summary: "hello".to_string(),
            automation: None,
            branch: None,
            message_count: Some(3),
            preview_role: MessageRole::User,
        };
        assert_eq!(
            build_recent_session_header_text(&session),
            format!("▶ [Codex] [CLI] {date} | myapp | - | 019f-cod (3 msgs)")
        );
    }

    #[test]
    fn test_render_recent_sessions_unified_grammar() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        let timestamp = Utc.with_ymd_and_hms(2026, 4, 17, 19, 51, 0).unwrap();
        let expected_date = timestamp
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        app.recent.filtered = vec![RecentSession {
            session_id: "b701e752abcd".to_string(),
            file_path: "/projects/sess.jsonl".to_string(),
            project: "avito-android".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp,
            summary: "grep samsungapps across this week's sessions".to_string(),
            automation: None,
            branch: Some("main".to_string()),
            message_count: Some(17),
            preview_role: MessageRole::User,
        }];

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        let buffer = terminal.backend().buffer();
        let mut header_line: Option<String> = None;
        let mut preview_line: Option<String> = None;
        for y in 0..24 {
            let mut line = String::new();
            for x in 0..120 {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            let trimmed = line.trim_end();
            if trimmed.contains("▶ [Claude] [CLI]") {
                header_line = Some(trimmed.to_string());
            } else if trimmed.contains("User: ") {
                preview_line = Some(trimmed.to_string());
            }
        }
        let header = header_line.expect("header line should render");
        let expected_header =
            format!("[Claude] [CLI] {expected_date} | avito-android | ⎇ main | b701e752 (17 msgs)");
        assert!(
            header.contains(&expected_header),
            "unexpected header: {:?}",
            header
        );
        let preview = preview_line.expect("preview line should render");
        assert!(
            preview.contains("User: grep samsungapps"),
            "unexpected preview: {:?}",
            preview
        );
    }

    fn find_cell_for_text(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<(u16, u16)> {
        for y in 0..height {
            let mut line = String::new();
            for x in 0..width {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            if let Some(byte_idx) = line.find(needle) {
                let x = line[..byte_idx].chars().count() as u16;
                return Some((x, y));
            }
        }
        None
    }

    fn assert_unselected_project_and_branch_are_highlighted(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        height: u16,
        project: &str,
        branch: &str,
    ) {
        let (project_x, project_y) = find_cell_for_text(buffer, width, height, project)
            .unwrap_or_else(|| panic!("project {project:?} should render"));
        let project_cell = buffer.cell((project_x, project_y)).unwrap();
        assert_eq!(project_cell.fg, PROJECT_FG);
        assert_eq!(project_cell.bg, PROJECT_BG);
        assert!(
            project_cell.modifier.contains(Modifier::BOLD),
            "project should be bold"
        );

        let branch_marker = format!("⎇ {branch}");
        let (branch_x, branch_y) = find_cell_for_text(buffer, width, height, &branch_marker)
            .unwrap_or_else(|| panic!("branch {branch_marker:?} should render"));
        let branch_cell = buffer.cell((branch_x, branch_y)).unwrap();
        assert_eq!(branch_cell.fg, BRANCH_FG);
        assert!(
            branch_cell.modifier.contains(Modifier::BOLD),
            "branch marker should be bold"
        );
    }

    #[test]
    fn test_render_search_header_highlights_project_and_branch() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.search.results_query = "test".to_string();
        let msg = Message {
            session_id: "search-style".to_string(),
            role: "user".to_string(),
            content: "test content".to_string(),
            text_content: "test content".to_string(),
            timestamp: Utc.with_ymd_and_hms(2026, 4, 30, 10, 0, 0).unwrap(),
            branch: Some("feature-branch".to_string()),
            line_number: 1,
            ..Default::default()
        };
        let file_path =
            "/path/to/projects/-Users-test-projects-highlight-app/session.jsonl".to_string();
        app.search.groups = vec![SessionGroup {
            session_id: "search-style".to_string(),
            file_path: file_path.clone(),
            matches: vec![RipgrepMatch {
                file_path,
                message: Some(msg),
                source: SessionSource::ClaudeCodeCLI,
            }],
            automation: None,
            message_count: Some(12),
            message_count_compacted: false,
        }];
        app.search.group_cursor = 99; // keep the only rendered row unselected

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        assert_unselected_project_and_branch_are_highlighted(
            terminal.backend().buffer(),
            120,
            24,
            "highlight-app",
            "feature-branch",
        );
    }

    #[test]
    fn test_render_recent_header_highlights_project_and_branch() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        app.recent.filtered = vec![RecentSession {
            session_id: "recent-style".to_string(),
            file_path: "/p/sess.jsonl".to_string(),
            project: "recent-app".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 30, 10, 0, 0).unwrap(),
            summary: "recent summary".to_string(),
            automation: None,
            branch: Some("recent-branch".to_string()),
            message_count: Some(8),
            preview_role: MessageRole::User,
        }];
        app.recent.cursor = 99; // keep the only rendered row unselected

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        assert_unselected_project_and_branch_are_highlighted(
            terminal.backend().buffer(),
            120,
            24,
            "recent-app",
            "recent-branch",
        );
    }

    /// Locate the `>` caret at column 0 that marks the selected row. Both
    /// caret- and preview-layout assertions need the same scan.
    fn find_selected_header_y(buffer: &ratatui::buffer::Buffer, label: &str) -> u16 {
        for y in 0..24 {
            if buffer.cell((0, y)).unwrap().symbol() == ">" {
                return y;
            }
        }
        panic!("selected {} row not found", label);
    }

    /// Shared contract check for the selected-header caret across the search
    /// and recent-sessions screens. Pins the "> ▶ [Claude] [CLI]" leading
    /// sequence, the continuous purple background across those 18 cells, and
    /// the "no BOLD on the ▶ glyph" invariant — Iosevka renders bold `▶`
    /// narrower than regular, which visually clips on the solid selection bg.
    fn assert_selected_caret_layout(buffer: &ratatui::buffer::Buffer, label: &str) {
        let y = find_selected_header_y(buffer, label);

        let expected: &[&str] = &[
            ">", " ", "▶", " ", "[", "C", "l", "a", "u", "d", "e", "]", " ", "[", "C", "L", "I",
            "]",
        ];
        for (i, &want) in expected.iter().enumerate() {
            let got = buffer.cell((i as u16, y)).unwrap().symbol();
            assert_eq!(
                got, want,
                "{} cell col {}: expected {:?}, got {:?}",
                label, i, want, got
            );
        }
        for x in 0..18u16 {
            let cell = buffer.cell((x, y)).unwrap();
            assert_eq!(
                cell.bg, SELECTION_BG,
                "{} cell col {} must have purple bg, got {:?}",
                label, x, cell.bg
            );
        }
        let caret = buffer.cell((2, y)).unwrap();
        assert_eq!(caret.fg, Color::Yellow, "{} caret must be yellow", label);
        assert!(
            !caret.modifier.contains(Modifier::BOLD),
            "{} caret must not be bold",
            label
        );
    }

    #[test]
    fn test_render_search_selected_caret_layout() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();
        app.search.group_cursor = 0;
        app.search.expanded = false;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        assert_selected_caret_layout(terminal.backend().buffer(), "search group");
    }

    #[test]
    fn test_render_recent_sessions_selected_caret_layout() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        app.recent.filtered = vec![RecentSession {
            session_id: "124e8bc6abcd".to_string(),
            file_path: "/p/sess.jsonl".to_string(),
            project: "claude-code-fullsearch-rust".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 21, 15, 50, 0).unwrap(),
            summary: "triangle layout test".to_string(),
            automation: None,
            branch: Some("main".to_string()),
            message_count: Some(360),
            preview_role: MessageRole::User,
        }];
        app.recent.cursor = 0;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        assert_selected_caret_layout(terminal.backend().buffer(), "recent session");
    }

    /// Pins the two-colour selection contract: purple `SELECTION_BG` across
    /// the preview row (header + preview share the same backdrop), and
    /// `DIM_FG` fg on the preview — so selected rows render as "yellow
    /// header + dim preview on purple", not a monochrome block.
    fn assert_selected_preview_row_has_selection_bg(buffer: &ratatui::buffer::Buffer, label: &str) {
        let preview_y = find_selected_header_y(buffer, label) + 1;

        for x in 0..10u16 {
            let cell = buffer.cell((x, preview_y)).unwrap();
            assert_eq!(
                cell.bg, SELECTION_BG,
                "{} preview col {} must have purple bg, got {:?}",
                label, x, cell.bg
            );
        }
        for x in 5..10u16 {
            let cell = buffer.cell((x, preview_y)).unwrap();
            assert_eq!(
                cell.fg, DIM_FG,
                "{} preview col {} must use DIM_FG (theme.dim), got {:?}",
                label, x, cell.fg
            );
        }
    }

    #[test]
    fn test_render_search_selected_preview_row_has_selection_bg() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = make_test_app_with_groups();
        app.search.group_cursor = 0;
        app.search.expanded = false;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        assert_selected_preview_row_has_selection_bg(terminal.backend().buffer(), "search group");
    }

    #[test]
    fn test_render_recent_sessions_selected_preview_row_has_selection_bg() {
        use crate::recent::RecentSession;
        use chrono::TimeZone;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new(vec!["/test".to_string()]);
        app.recent.loading = false;
        app.recent.load_rx = None;
        app.recent.filtered = vec![RecentSession {
            session_id: "124e8bc6abcd".to_string(),
            file_path: "/p/sess.jsonl".to_string(),
            project: "claude-code-fullsearch-rust".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 21, 15, 50, 0).unwrap(),
            summary: "triangle layout test".to_string(),
            automation: None,
            branch: Some("main".to_string()),
            message_count: Some(360),
            preview_role: MessageRole::User,
        }];
        app.recent.cursor = 0;

        terminal
            .draw(|frame| render(frame, &app.view()))
            .expect("Render should not panic");

        assert_selected_preview_row_has_selection_bg(terminal.backend().buffer(), "recent session");
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
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
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
            branch: None,
            message_count: None,
            preview_role: crate::session::record::MessageRole::User,
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
        let dim = Style::default().fg(DIM_FG);
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
        let dim = Style::default().fg(DIM_FG);
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
        let dim = Style::default().fg(DIM_FG);
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
        let dim = Style::default().fg(DIM_FG);
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
        let dim = Style::default().fg(DIM_FG);
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

    fn hint_texts(hints: &[HintItem<'_>]) -> Vec<String> {
        hints
            .iter()
            .map(|h| {
                h.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn render_ai_hints_shows_rank_when_no_ranked_count() {
        let texts = hint_texts(&build_ai_hints(None));
        assert!(
            texts.iter().any(|t| t == "[Enter] AI Rank"),
            "expected [Enter] AI Rank when ranked_count is None, got {:?}",
            texts
        );
        assert!(
            !texts.iter().any(|t| t == "[Enter] Resume"),
            "must not show [Enter] Resume when ranked_count is None, got {:?}",
            texts
        );
    }

    #[test]
    fn render_ai_hints_shows_resume_when_ranked_count_set() {
        let texts = hint_texts(&build_ai_hints(Some(3)));
        assert!(
            texts.iter().any(|t| t == "[Enter] Resume"),
            "expected [Enter] Resume when ranked_count is Some, got {:?}",
            texts
        );
        assert!(
            !texts.iter().any(|t| t == "[Enter] AI Rank"),
            "must not show [Enter] AI Rank when ranked_count is Some, got {:?}",
            texts
        );
    }

    #[test]
    fn render_ai_hints_keeps_navigation_and_cancel_labels() {
        for ranked in [None, Some(1), Some(42)] {
            let texts = hint_texts(&build_ai_hints(ranked));
            assert!(
                texts.iter().any(|t| t == "[↑↓] Navigate"),
                "navigation hint must be present for ranked={:?}, got {:?}",
                ranked,
                texts
            );
            assert!(
                texts.iter().any(|t| t == "[Esc/Ctrl+G] Cancel"),
                "cancel hint must be present for ranked={:?}, got {:?}",
                ranked,
                texts
            );
        }
    }
}
