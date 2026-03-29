# Recent Sessions on Startup

## Overview
- Show a list of recent sessions in the TUI when the search input is empty (replacing the current blank screen)
- Each row displays: timestamp, project name, and the **first user message** as a summary
- Users can navigate with arrow keys, Enter to resume, seamless transition to search when typing begins
- Inspired by `claude-history`'s session list, but showing first message (not last) as summary

## Context (from discovery)
- **Files involved**:
  - `src/tui/state.rs` — App struct, needs new fields for recent sessions + background loader
  - `src/tui/render_search.rs` — render function, needs empty-state rendering of recent sessions list
  - `src/tui/search_mode.rs` — key handling, needs navigation for recent sessions list
  - `src/cli.rs` — `extract_session_metadata()` already reads JSONL files, can be extended
  - `src/search/message.rs` — `Message::from_jsonl()` already parses user messages
  - `src/session.rs` — shared field extractors (session_id, timestamp, record_type)
  - `src/lib.rs` — `get_search_paths()` provides search directories
- **Patterns found**:
  - Background search uses mpsc channel (`search_tx`/`search_rx`) with debounced async results
  - `SessionGroup` and `RipgrepMatch` are the existing display models for search results
  - `Message::from_jsonl()` skips non-user/assistant types and handles both CLI and Desktop formats
  - `extract_session_metadata()` in `cli.rs` reads full file to count messages — can be optimized to stop after first user message for summary
- **Dependencies**: `rayon` needed for parallel file scanning (not currently in deps — need to add)
- **Scale**: ~183 project dirs, ~4474 JSONL files total

## Development Approach
- **Testing approach**: TDD (tests first)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility with existing search flow

## Testing Strategy
- **Unit tests**: test summary extraction from JSONL, session collection, sorting
- **Integration tests**: extend `tests/cli_list.rs` patterns for new functionality
- **Fixtures**: reuse existing `tests/fixtures/*.jsonl` files

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Add `rayon` dependency and `RecentSession` struct
- [x] write test for `RecentSession` creation from known JSONL data
- [x] add `rayon` to `[dependencies]` in `Cargo.toml`
- [x] define `RecentSession` struct in new `src/recent.rs` module: `{ session_id, file_path, project, source, timestamp, summary, message_count }`
- [x] add `pub mod recent;` to `src/lib.rs`
- [x] run tests — must pass before next task

### Task 2: Extract first user message as summary
- [x] write test: `extract_summary()` returns first non-meta user message text from a JSONL file
- [x] write test: `extract_summary()` prefers `type=summary` record over first user message
- [x] write test: `extract_summary()` returns `None` for empty/agent-only files
- [x] write test: `extract_summary()` truncates long messages to ~100 chars
- [x] write test: `extract_summary()` handles Desktop format (`session_id`, `_audit_timestamp`)
- [x] implement `extract_summary(path: &Path) -> Option<RecentSession>` in `src/recent.rs`
  - read JSONL line by line
  - priority 1: `type=summary` → use `.summary` field
  - priority 2: first `type=user, !isMeta` → extract text content
  - stop reading after finding summary (don't read full file)
  - extract session_id, timestamp, project name from path
- [x] run tests — must pass before next task

### Task 3: Parallel session collection with rayon
- [x] write test: `collect_recent_sessions()` finds sessions across multiple project dirs
- [x] write test: `collect_recent_sessions()` skips `agent-*.jsonl` files
- [x] write test: `collect_recent_sessions()` sorts by mtime descending
- [x] write test: `collect_recent_sessions()` respects limit parameter
- [x] implement `collect_recent_sessions(search_paths: &[String], limit: usize) -> Vec<RecentSession>` in `src/recent.rs`
  - walk directories, find `*.jsonl` files (skip `agent-*`)
  - sort by filesystem mtime descending
  - take top `limit` files
  - use `rayon::par_iter()` to extract summaries in parallel
  - sort final results by timestamp descending
- [x] run tests — must pass before next task

### Task 4: Wire background loading into App state
- [x] write test: App initializes with empty `recent_sessions` vec
- [x] write test: App receives recent sessions from background channel
- [x] add fields to `App` struct in `src/tui/state.rs`:
  - `recent_sessions: Vec<RecentSession>` — loaded sessions
  - `recent_cursor: usize` — navigation cursor
  - `recent_loading: bool` — loading indicator
  - `recent_load_rx: Option<Receiver<Vec<RecentSession>>>` — channel from background loader
- [x] in `App::new()`: spawn background thread that calls `collect_recent_sessions()` and sends result via mpsc
- [x] in `App::tick()`: check `recent_load_rx` for results, populate `recent_sessions`
- [x] run tests — must pass before next task

### Task 5: Render recent sessions in empty state
- [x] in `render_search.rs` `render_groups()`: when `app.input.is_empty() && app.groups.is_empty()`, render recent sessions list instead of empty area
- [x] each row format: `YYYY-MM-DD HH:MM  project-name  First user message...` with colors:
  - timestamp: DarkGray
  - project: Cyan
  - summary: White (selected row highlighted)
- [x] show "Loading recent sessions..." while `recent_loading` is true
- [x] show "No recent sessions found" if loaded but empty
- [x] update status bar text for recent sessions mode
- [x] update help bar: show relevant keybindings for recent sessions navigation
- [x] run `cargo build` — must compile. Manual visual verification.
- [x] run tests — must pass before next task

### Task 6: Navigation and resume from recent sessions list
- [x] in `src/tui/search_mode.rs`: when input is empty and recent sessions are shown:
  - Up/Down arrows navigate `recent_cursor`
  - Enter on a session triggers resume (set `resume_id`, `resume_file_path`, `resume_source`)
  - Ctrl+B enters tree mode for selected session
  - Any character input switches to search mode (existing behavior)
- [x] handle edge cases: cursor bounds, empty list, loading state
- [x] run tests — must pass before next task

### Task 7: Verify acceptance criteria
- [x] verify: TUI shows recent sessions on startup with first user message
- [x] verify: navigation works (Up/Down/Enter/Ctrl+B)
- [x] verify: typing starts search seamlessly (recent list disappears)
- [x] verify: Ctrl+C on empty input still quits
- [x] verify: existing search functionality unchanged
- [x] run full test suite: `cargo test`
- [x] run linter: `cargo clippy --all-targets --all-features -- -D warnings`
- [x] run formatter check: `cargo fmt --check`

### Task 8: [Final] Update documentation
- [x] update README.md with new feature description
- [x] update CLAUDE.md architecture section if needed (new `recent.rs` module)

## Technical Details

### `RecentSession` struct
```rust
pub struct RecentSession {
    pub session_id: String,
    pub file_path: String,
    pub project: String,
    pub source: SessionSource,
    pub timestamp: DateTime<Utc>,
    pub summary: String,        // first user message, truncated to ~100 chars
    pub message_count: usize,   // optional, 0 if not counted
}
```

### Summary extraction priority
1. `type=summary` record → use `.summary` field (Claude Code generates these)
2. First `type=user` where `isMeta` is not true → extract text from `.message.content`
3. Skip `<system-reminder>` prefixed content (meta messages)
4. Truncate to 100 chars with `...` suffix

### File scanning flow
```
get_search_paths() → walk dirs → find *.jsonl (skip agent-*) → stat() for mtime
→ sort by mtime desc → take top 50 → rayon par_iter → extract_summary each
→ sort by timestamp desc → send to TUI via mpsc
```

### TUI state transitions
```
App starts → recent_loading=true → background thread scans
           → tick() receives results → recent_sessions populated, recent_loading=false
           → render shows recent list

User types → input non-empty → render shows search results (existing flow)
User clears → input empty → render shows recent sessions again
```

## Post-Completion

**Manual verification**:
- Launch `ccs` with no arguments — verify recent sessions appear quickly
- Navigate up/down through sessions
- Press Enter to resume a session
- Start typing to verify seamless search transition
- Test with `CCFS_SEARCH_PATH` override pointing to test fixtures

**Performance check**:
- Measure startup time with ~4.5k JSONL files
- If >2s, consider adding bincode cache as follow-up task
