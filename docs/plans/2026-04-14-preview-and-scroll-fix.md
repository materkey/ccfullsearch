# Preview text + scroll fix for search results

## Overview
- Add preview line under each collapsed search result group (like recent sessions show summaries)
- Fix pagination: search results don't scroll — cursor goes off-screen when groups exceed terminal height. Use ratatui `ListState` for automatic scrolling

## Context (from discovery)
- `render_groups()` in `src/tui/render_search.rs:499-526` builds ALL items with no scroll offset
- `render_recent_sessions()` in same file already has windowing pattern (lines 529-604)
- `build_group_header_text()` at lines 607-653 creates the header line
- `render_sub_match()` at lines 673-740 handles expanded sub-matches
- `sanitize_content()` and `truncate_to_width()` already exist (lines 738-773)
- `group_cursor = 0` is already reset in `handle_search_result()` (state.rs:1084)
- No new struct fields needed — `ListState` is created per-draw

## Development Approach
- **Testing approach**: Regular
- Complete each task fully before moving to the next
- Only `src/tui/render_search.rs` needs changes

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Add preview line for collapsed groups
- [x] In `src/tui/render_search.rs`, in `render_groups()` function (around line 499-526), after `items.push(header)` for collapsed (NOT expanded) groups, add a second ListItem showing the first match's content preview:
  ```rust
  if !is_expanded {
      if let Some(first) = group.first_match() {
          if let Some(msg) = &first.message {
              let role_label = if msg.role == "user" { "User" } else { "Claude" };
              let content = sanitize_content(&msg.content);
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
  }
  ```
  The functions `sanitize_content()` and `truncate_to_width()` already exist in render_search.rs
- [x] Verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 2: Replace render_widget with render_stateful_widget for auto-scroll
- [x] In `src/tui/render_search.rs`, in `render_groups()`, replace the current `frame.render_widget(list, area)` call with `ListState`-based stateful rendering. After building the `items` vec, add:
  ```rust
  let mut list_state = ratatui::widgets::ListState::default();
  // Map group_cursor to item index.
  // Each collapsed group = 2 items (header + preview).
  // Only the group at group_cursor can be expanded.
  let mut selected_item_idx = 0;
  for (i, group) in app.search.groups.iter().enumerate() {
      if i == app.search.group_cursor {
          if app.search.expanded {
              selected_item_idx += 1 + app.search.sub_cursor; // header + sub offset
          }
          break;
      }
      // Prior groups are always collapsed (expanded only applies to group_cursor)
      selected_item_idx += 2; // 1 header + 1 preview
  }
  list_state.select(Some(selected_item_idx));
  let list = List::new(items).highlight_style(Style::default()); // no-op highlight — custom styles already on items
  frame.render_stateful_widget(list, area, &mut list_state);
  ```
  This replaces the existing `frame.render_widget(list, area)` — no new fields in SearchState needed, ListState is created per-draw and auto-scrolls to `selected`
- [x] Verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 3: Final verification
- [x] Run `cargo clippy --all-targets --all-features -- -D warnings`
- [x] Run `cargo test` — all tests must pass
- [x] Run `cargo run` and verify visually:
  - Search → each collapsed group shows preview of first match content
  - Many results → arrow keys keep cursor visible, list scrolls
  - Expand/collapse (→/←) → preview hides, sub-matches show, scroll adjusts

### Task 4: Commit and push
- [x] Commit changes with message: `feat: add preview line and fix scroll for search results`
- [x] Push to current branch

## Post-Completion
- Test with real session data — long messages should truncate cleanly
- Test with narrow terminals — preview should degrade gracefully
- Test with expanded groups — scroll should track sub-cursor correctly
