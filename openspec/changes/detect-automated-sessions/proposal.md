## Why

Ralphex and similar automation tools create dozens of Claude Code sessions that are indistinguishable from manually created ones. This makes it hard to find your own sessions in the recent sessions list and search results. Sessions need to be tagged at scan time and filterable in the TUI.

## What Changes

- Detect automated sessions during JSONL scanning by looking for tool-specific markers in user message content (e.g., `<<<RALPHEX:` signals)
- Add `automation: Option<String>` field to `RecentSession` and propagate detection to `SessionGroup`
- Add a three-state filter (All / Manual only / Auto only) toggled via `Ctrl+H` in the TUI
- Visually mark automated sessions with a dim `[A]` indicator in both recent sessions and search result views
- Show active filter mode in the search title bar (e.g., `Search [Manual]`)

## Capabilities

### New Capabilities
- `automation-detection`: Detect whether a session was created by an automation tool (ralphex, etc.) by scanning user message content for known markers
- `automation-filter`: Three-state TUI filter (All / Manual / Auto) with `Ctrl+H` toggle, visual `[A]` indicators, and search title integration

### Modified Capabilities

## Impact

- `src/recent.rs`: `RecentSession` struct gains `automation` field; `extract_summary` checks user content for markers during existing scan passes
- `src/search/group.rs`: `SessionGroup` gets automation detection from grouped matches
- `src/search/ripgrep.rs`: `RipgrepMatch` or `Message` may carry automation flag
- `src/tui/state.rs`: `App` gains `automation_filter` state and `all_recent_sessions` filtering logic
- `src/tui/search_mode.rs`: `Ctrl+H` handler, filter cycling, `apply_recent_sessions_filter` update
- `src/tui/render_search.rs`: `[A]` indicator rendering, dimmed style for automated sessions, filter indicator in title bar and help bar
- `src/main.rs`: Key binding for `Ctrl+H`
