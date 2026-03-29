## 1. Automation Detection Core

- [x] 1.1 Add automation marker registry: a `const` list of `(&str, &str)` pairs `[("<<<RALPHEX:", "ralphex")]` in `src/session.rs` and a `detect_automation(content: &str) -> Option<String>` function
- [x] 1.2 Add `automation: Option<String>` field to `RecentSession` in `src/recent.rs`
- [x] 1.3 Wire detection into `extract_summary` head scan: when parsing user-type records in first 30 lines, call `detect_automation` on message content
- [x] 1.4 Wire detection into `extract_summary` tail scan: check user-type records in `find_summary_from_tail` for automation markers
- [x] 1.5 Wire detection into `extract_summary` middle scan: propagate automation flag through pass 3
- [x] 1.6 Unit tests for `detect_automation` (ralphex marker, no marker, marker in non-user content)
- [x] 1.7 Unit test for `extract_summary` returning `automation = Some("ralphex")` from fixture JSONL

## 2. Search Results Detection

- [x] 2.1 Add `automation: Option<String>` field to `SessionGroup` in `src/search/group.rs`
- [x] 2.2 Detect automation during `group_by_session`: check user-role matches for markers
- [x] 2.3 Unit test for `SessionGroup` automation detection from grouped matches

## 3. TUI Filter State

- [x] 3.1 Add `AutomationFilter` enum (All, Manual, Auto) and `automation_filter` field to `App` in `src/tui/state.rs`
- [x] 3.2 Implement `toggle_automation_filter` method cycling All → Manual → Auto → All
- [x] 3.3 Update `apply_recent_sessions_filter` to compose automation filter with project filter
- [x] 3.4 Add `Ctrl+H` key binding in `src/main.rs` calling `toggle_automation_filter`
- [x] 3.5 Filter search result groups by automation state when rendering (or in a filtering pass before render)

## 4. TUI Visual Indicators

- [x] 4.1 Render `[A]` prefix (DarkGray) before summary for automated sessions in `render_recent_sessions`
- [x] 4.2 Dim automated session summary text to `Color::Gray` (vs `Color::White` for manual)
- [x] 4.3 Render `[A]` in search result group headers for automated `SessionGroup`s in `render_groups`
- [x] 4.4 Add `[Manual]` / `[Auto]` indicator to search title bar (compose with `[Regex]` and `[Project]`)
- [x] 4.5 Add `[Ctrl+H] Filter` to help bar in recent sessions and search result modes

## 5. Integration Tests

- [x] 5.1 Add a fixture JSONL file with ralphex markers for testing
- [x] 5.2 Integration test: `extract_summary` on ralphex fixture returns `automation = Some("ralphex")`
- [x] 5.3 Integration test: `extract_summary` on existing manual fixture returns `automation = None`
