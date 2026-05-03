# Show total session message count in search results

## Overview
- Add total message count to search result headers: `(3/42 matches)` for normal sessions, `(3/42+ matches)` for compacted sessions
- Counts loaded lazily via background thread after search results arrive, so initial display is instant

## Context (from discovery)
- `SessionGroup` struct in `src/search/group.rs:7-12` — groups search results by session
- `build_group_header_text()` in `src/tui/render_search.rs:636-646` — renders `(N matches)` header
- `SearchState` in `src/tui/state.rs:489-512` — holds search state with existing mpsc channel pattern
- `handle_search_result()` in `src/tui/state.rs:1030-1070` — processes background search results
- `apply_groups_filter()` in `src/tui/state.rs:1082` — re-clones `all_groups` into `groups`
- Compaction is a first-class concept: `compact_boundary` in `src/dag/mod.rs`
- ~25 `SessionGroup { ... }` literals across test files need new fields

## Development Approach
- **Testing approach**: Regular — unit tests for count function and async poll path
- Complete each task fully before moving to the next
- CRITICAL: write `message_count` only to `all_groups`, never to `groups` directly — `apply_groups_filter()` handles propagation via `.clone()`

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Add fields to SessionGroup and count function
- [x] In `src/search/group.rs`, add `pub message_count: Option<usize>` and `pub message_count_compacted: bool` fields to `SessionGroup` struct (lines 7-12)
- [x] Add `message_count: None, message_count_compacted: false` to the `SessionGroup` constructor in `group_by_session()` (line 50)
- [x] Add `pub fn count_session_messages(file_path: &str) -> (usize, bool)` function that reads JSONL via BufReader, counts lines with `"type":"user"` or `"type":"assistant"` using `serde_json::Value` (only parse `type` field), detects `"type":"summary"` or `"type":"compact_boundary"` for the compacted flag, returns `(0, false)` on I/O error
- [x] Add re-export in `src/search/mod.rs`: `pub use group::count_session_messages;`
- [x] Add unit tests for `count_session_messages` in `src/search/group.rs`: (a) NamedTempFile with 3 user + 2 assistant + 1 summary → assert `(5, true)`, (b) NamedTempFile with 2 user + 1 assistant → assert `(3, false)`
- [x] Fix all ~25 `SessionGroup { ... }` literals in test files to include `message_count: None, message_count_compacted: false` — files: `src/search/group.rs`, `src/tui/render_search.rs`, `src/tui/state.rs`, `src/tui/search_mode.rs`, `tests/render_snapshots.rs`
- [x] Verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 2: Background counting thread and state management
- [x] In `src/tui/state.rs`, add to `SearchState` struct: `pub(crate) message_count_rx: Option<Receiver<(String, usize, bool)>>` and `pub(crate) message_count_cancel: Option<Arc<AtomicBool>>` — initialize both as `None`
- [x] In `handle_search_result()`, after `self.search.all_groups = groups;` (line 1052): cancel previous thread via `if let Some(flag) = self.search.message_count_cancel.take() { flag.store(true, Relaxed); }`, collect unique file_paths from groups, create `mpsc::channel` and `Arc<AtomicBool>` cancel flag, spawn `std::thread` that iterates files checking cancel flag between iterations, calls `count_session_messages()`, sends `(file_path, count, compacted)`, breaks on `Err` from `send()`; store rx and cancel flag
- [x] In `tick()`, add poll section: `try_recv` loop draining all available results, apply `message_count = Some(count)` and `message_count_compacted = compacted` ONLY to entries in `all_groups` (by file_path match). Call `self.apply_groups_filter()` once after drain if any records were received. NEVER write to `groups` directly
- [x] In `reset_search_state()`, add: cancel flag `store(true, Relaxed)` + `take()`, then `message_count_rx = None`
- [x] Add unit test `test_message_count_poll_updates_groups` in `src/tui/state.rs`: construct App with one group, send count through channel, call tick(), assert all_groups[0].message_count == Some(count), call apply_groups_filter(), assert groups[0].message_count == Some(count)
- [x] Verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 3: Update rendering
- [x] In `src/tui/render_search.rs`, in `build_group_header_text()` (line 636-646), replace `group.matches.len()` with a conditional format: `Some(total)` → `"{}/{}{}"` with matches.len(), total, and "+" suffix if `message_count_compacted`, `None` → just matches.len(). The word "matches" stays unchanged
- [x] Verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 4: Final verification
- [x] Run `cargo clippy --all-targets --all-features -- -D warnings`
- [x] Run `cargo test` — all tests must pass
- [x] Run `cargo run` and verify visually: search → "(N matches)" initially → updates to "(N/M matches)" or "(N/M+ matches)"

### Task 5: Commit and push
- [x] Commit changes with message: `feat: show total session message count in search results`
- [x] Push to current branch

## Post-Completion
- Verify the feature works with real session data including compacted sessions
- Check that rapid typing doesn't cause thread accumulation (cancel flag should prevent this)
- Monitor for any performance regression with large session directories
