# AI rank invalidate on filter toggle

## Overview
- Fix the Codex-flagged high-severity bug: when AI re-rank is active (`Ctrl+G`), pressing a filter/scope toggle (`Ctrl+R` regex, `Ctrl+A` project, `Ctrl+H` automation) rebuilds the candidate list but leaves `ai.ranked_count = Some(n)`, so the next `Enter` resumes from the stale "AI-ranked" routing while the UI is showing a freshly filtered list.
- Any action that mutates `recent.filtered` / `search.groups` while `ai.active` must drop the rank so `Enter` re-submits the query instead.

## Context (from discovery)
- `invalidate_ai_rank()` exists at `src/tui/state.rs:1223` — already clears `ranked_count`, `result_rx`, `thinking`. Canonical rank-drop path.
- Currently called only from AI-query text-editing actions (`state.rs:861-891`): `InputChar`, `Backspace`, `Delete`, `ClearInput`, `DeleteWordLeft`, `DeleteWordRight`.
- Enter routing in AI mode: `state.rs:919-926` — `KeyAction::Enter` arm calls `on_enter_inner()` when `ranked_count.is_some()`, otherwise `submit_ai_query()`.
- Toggle methods that must invalidate:
  - `on_toggle_regex` — `src/tui/search_mode.rs:135`
  - `toggle_automation_filter` — `src/tui/search_mode.rs:144` (synchronously calls `apply_recent_sessions_filter` + `apply_groups_filter`)
  - `toggle_project_filter` — `src/tui/search_mode.rs:160` (starts async `recent.start_project_load` + synchronous `apply_recent_sessions_filter`)
- `exit_ai_mode` (`state.rs:1229-1246`) defensively re-applies filters from current `recent.all` / `search.all_groups` — so `ai.original_*_order` can stay untouched by `invalidate_ai_rank`; no need to also clear them.
- Test template: `ai_query_mutation_clears_rank_and_receiver` at `src/tui/state.rs:2484` — iterates six actions, asserts `ranked_count.is_none()`, `result_rx.is_none()`, `!thinking`. Mirror the same assertion triplet for the three new toggle tests.
- `invalidate_ai_rank` is a method on `App`; `search_mode.rs` uses `impl App` in the same crate, so private visibility is sufficient — no `pub(crate)` promotion needed.

## Development Approach
- **Testing approach**: Regular (existing test pattern maps 1:1 to the fix; TDD not required).
- Complete each task fully before moving to the next.
- Red-green not required — the bug is a missing invocation at three known sites; new tests will fail without the three-line fix and pass with it.

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Invalidate rank on filter/scope toggles
- [x] In `src/tui/search_mode.rs`, add `if self.ai.active { self.invalidate_ai_rank(); }` as the first statement of `on_toggle_regex` (line 135 region).
- [x] Same guarded call at the top of `toggle_automation_filter` (line 144 region).
- [x] Same guarded call at the top of `toggle_project_filter` (line 160 region).
- [x] Run `cargo build` — must compile clean. ⚠️ Required promoting `invalidate_ai_rank` from `fn` to `pub(crate) fn` in `state.rs` — plan's Context note was wrong, Rust modules need at least `pub(crate)` to be called from a sibling module even in the same crate.

### Task 2: Regression tests for each toggle
- [x] In `src/tui/state.rs` `#[cfg(test)] mod tests` block (near line 2484), add `ai_toggle_regex_clears_rank` — set `ai.active=true`, `ai.ranked_count=Some(5)`, attach a synthetic `ai.result_rx`, set `ai.thinking=true`, call `app.on_toggle_regex()`, assert `ranked_count.is_none()`, `result_rx.is_none()`, `!thinking`.
- [x] Add `ai_toggle_automation_filter_clears_rank` with the same fixture pattern but calling `app.toggle_automation_filter()`.
- [x] Add `ai_toggle_project_filter_clears_rank` with the same fixture pattern but calling `app.toggle_project_filter()`. Note: `toggle_project_filter` early-returns when `current_project_paths.is_empty()`, so the fixture must seed `app.current_project_paths = vec!["/test/project".to_string()]`.
- [x] Run `cargo test --lib ai_toggle_` — all three new tests must pass.
- [x] Run `cargo test` — full unit + integration suite (529+ tests) must stay green. 533 lib tests pass (530 → 533 with the three new ones), all integration suites green.

### Task 3: Lint + format gate
- [x] `cargo fmt` (auto-apply) then `cargo fmt --check` must succeed.
- [x] `cargo clippy --all-targets --all-features -- -D warnings` must succeed.

### Task 4: Commit and push
- [x] Commit with message `fix(tui): invalidate AI rank on filter/scope toggle` plus a `## Problem / ## Steps to Reproduce / ## Solution` body citing the Codex finding and naming the three toggle entry points.
- [x] Do NOT push (per CLAUDE.md: only push on explicit user request).

## Post-Completion
- Verify manually via `cargo install --path . --locked` then `ccs`:
  - `Ctrl+G` → type query → Enter → see "AI: N sessions ranked" in status + list reordered.
  - `Ctrl+R` → status drops the ranked message, list reverts to natural order for the new regex-mode setting, Enter submits a fresh AI query instead of resuming.
  - Repeat with `Ctrl+A` (project filter) and `Ctrl+H` (All/Manual/Auto cycle).
- Related staleness window not in scope: async search completion (`handle_search_result` at `state.rs:1084-1121`) while `ai.active`. The only way this path is hit during AI mode is via the three toggles above (AI mode does not mutate `search.results_query`), so the fix covers it transitively. No additional invalidation point needed; leave as a follow-up if a regression surfaces.
