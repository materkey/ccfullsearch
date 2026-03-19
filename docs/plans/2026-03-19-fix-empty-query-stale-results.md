# Fix: Stale search results when query is deleted to empty

## Overview
- When a user backspaces to an empty query, previous search results and status remain visible
- The TUI should reset to its initial idle state (no results, no status) when query becomes empty
- Both the debounce path (gradual backspace) and explicit clear path (Ctrl-C / Escape) must be fixed

## Context (from discovery)
- **Bug location**: `src/tui/state.rs:320` — debounce check requires `!self.input.is_empty()`, so empty query never triggers state update
- **Second bug**: `clear_input()` at line 194 clears `input` and `last_query` but does NOT clear `results`, `groups`, or `results_query`
- **Stale rendering**: `src/tui/render_search.rs:98` — checks `!app.results_query.is_empty()` to show "No matches found", which stays true after query is cleared
- **Existing tests**: `test_clear_input_resets_state` at line 416 does NOT assert that `results`, `groups`, `results_query` are cleared

## Development Approach
- **Testing approach**: TDD (tests first)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**

## Testing Strategy
- **Unit tests**: inline `#[cfg(test)]` in `src/tui/state.rs`
- No e2e/integration tests needed — this is purely internal state management

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Write failing tests for empty-query state reset

TDD: write tests that assert the correct behavior BEFORE fixing the code.

- [ ] Add test `test_clear_input_clears_results_and_groups` — set up `App` with non-empty `results`, `groups`, `results_query`, call `clear_input()`, assert all three are empty
- [ ] Add test `test_tick_clears_state_when_query_becomes_empty` — set up `App` with `input=""`, `last_query="hello"`, non-empty `results`/`groups`/`results_query`, simulate debounce expiry via `tick()`, assert results/groups/results_query are cleared
- [ ] Run tests — expected: both new tests FAIL (confirms bug exists)

### Task 2: Fix `clear_input()` to reset result state

Fix the explicit clear path (Ctrl-C / Escape).

- [ ] In `src/tui/state.rs:clear_input()` (line 194), add: `self.results.clear()`, `self.groups.clear()`, `self.results_query.clear()`, reset `self.group_cursor = 0`, `self.sub_cursor = 0`, `self.expanded = false`
- [ ] Run tests — `test_clear_input_clears_results_and_groups` must now PASS

### Task 3: Fix debounce `tick()` to handle empty query transition

Fix the gradual-backspace path.

- [ ] In `src/tui/state.rs:tick()` (line 320), change the debounce condition to also handle empty input: when `query_changed` is true and `self.input.is_empty()`, clear `results`, `groups`, `results_query`, update `last_query` to empty, reset cursors — instead of calling `start_search()`
- [ ] Run tests — `test_tick_clears_state_when_query_becomes_empty` must now PASS

### Task 4: Verify acceptance criteria
- [ ] Verify: backspace-to-empty clears all results and shows idle state
- [ ] Verify: Ctrl-C/Escape clears all results and shows idle state
- [ ] Verify: typing a new query after clearing still works (debounce → search → results)
- [ ] Run full test suite (`cargo test`)
- [ ] Run linter (`cargo clippy --all-targets --all-features -- -D warnings`)
- [ ] Run formatter check (`cargo fmt --check`)

## Technical Details

### State fields that must be cleared on empty query
| Field | Type | Reset value |
|---|---|---|
| `results` | `Vec<RipgrepMatch>` | `vec![]` |
| `groups` | `Vec<SessionGroup>` | `vec![]` |
| `results_query` | `String` | `""` |
| `group_cursor` | `usize` | `0` |
| `sub_cursor` | `usize` | `0` |
| `expanded` | `bool` | `false` |
| `latest_chains` | `HashMap` | `.clear()` |
| `searching` | `bool` | `false` |
| `error` | `Option<String>` | `None` |

### Two code paths to fix
1. **`clear_input()`** (line 194) — called by Ctrl-C / Escape when input is non-empty
2. **`tick()` debounce** (line 320) — triggered after 300ms when user stops typing; currently skips empty queries

## Post-Completion

**Manual verification:**
- Launch TUI with `cargo run`, type a query, see results, backspace to empty — results should disappear
- Type a query, press Ctrl-C — results should disappear
- After clearing, type a new query — search should work normally
