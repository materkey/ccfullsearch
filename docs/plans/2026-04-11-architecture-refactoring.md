# CCS Architecture Refactoring

## Overview
- Eliminate dependency cycle between `search` and `resume` modules, unify duplicated DAG/content-extraction logic into a typed Session Data Layer, and decompose the 40-field `App` god object into testable sub-states
- Fixes: bidirectional import cycle, 3x duplicated DAG chain-walking, 3x duplicated content extraction, 4-Options invariant violation in resume, untestable key dispatch cascade, render mutation side effect
- All changes are internal refactoring — zero behavioral changes, zero new dependencies

## Context (from discovery)
- Files/components involved: `session.rs`, `search/ripgrep.rs`, `search/message.rs`, `search/mod.rs`, `search/group.rs`, `resume/mod.rs`, `resume/fork.rs`, `resume/launcher.rs`, `recent.rs`, `tree/mod.rs`, `tui/state.rs`, `tui/search_mode.rs`, `tui/tree_mode.rs`, `tui/render_search.rs`, `tui/render_tree.rs`, `main.rs`, `lib.rs`
- Codebase: 16,200 lines, 365 tests (313 unit + 52 integration), 7 integration test files, 6 fixtures
- Dependency: `search/ripgrep.rs` → `resume::resolve_parent_session` (cycle edge A); `resume/launcher.rs` → `search::Message` (cycle edge B)
- DAG duplication: `resume/fork.rs` (parse_dag+build_chain+find_tip), `tree/mod.rs` (build_latest_chain), `recent.rs` (build_latest_chain)
- Content extraction duplication: `search/message.rs` (extract_content), `recent.rs` (extract_text_content), `tree/mod.rs` (extract_preview)
- App god object: 40+ pub fields in `tui/state.rs`, 4 resume Options, render mutates `tree_visible_height`

## Development Approach
- **Testing approach**: Tests-after — existing 366 tests serve as safety net; new tests added for new types/modules
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility — all public APIs preserved

## Testing Strategy
- **Unit tests**: required for every task (existing tests must pass + new tests for new code)
- **Safety net**: `cargo fmt --check` → `cargo clippy -- -D warnings` → `cargo test` after every task
- **Test count invariant**: must be >= 365 after every task (never decrease)
- **Integration tests**: `tree_parsing`, `resume_resolution`, `render_snapshots`, `cli_search` must pass after every task

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Implementation Steps

<!-- 
PHASE 1: Break Dependency Cycle (MUST LAND FIRST)
Risk: LOW. Scope: 4-5 files, <100 lines.
-->

### Task 1: Move `resolve_parent_session` from `resume/mod.rs` to `session.rs`
- [ ] read `src/resume/mod.rs` lines 23-61 (`resolve_parent_session` fn) and lines 211-363 (4 test cases)
- [ ] read `src/session.rs` to find insertion point (after `find_session_file_in_paths`)
- [ ] add `pub fn resolve_parent_session(session_id: &str, file_path: &str) -> (String, String)` to `session.rs`
- [ ] move the 4 test cases from `resume/mod.rs` to `session.rs` `mod tests`
- [ ] update `src/resume/mod.rs`: remove fn body + test wrapper, add `use crate::session::resolve_parent_session`
- [ ] update `src/search/ripgrep.rs` line 2: `use crate::session::resolve_parent_session` (was `crate::resume`)
- [ ] run `cargo test` — all 366+ tests must pass

### Task 2: Move `extract_content` to `session.rs` as shared free function
- [ ] read `src/search/message.rs` lines 73-119 (`Message::extract_content`)
- [ ] read `src/resume/launcher.rs` line 70 (the call site using `Message::extract_content`)
- [ ] add `pub fn extract_message_content(raw: &serde_json::Value) -> String` to `session.rs` (body from `Message::extract_content`)
- [ ] update `Message::extract_content` in `search/message.rs` to delegate: `session::extract_message_content(raw)`
- [ ] update `resume/launcher.rs`: remove `use crate::search::Message`, call `crate::session::extract_message_content`
- [ ] write test for `extract_message_content` in `session.rs` — text block, tool_use, tool_result, thinking, mixed array
- [ ] run `cargo test` — all tests must pass, cycle broken

### Task 3: Fix `SessionSource` re-export and replace `BackgroundSearchResult` tuple
- [ ] read `src/main.rs` — find all `ccs::search::SessionSource` usages (lines ~562, 574)
- [ ] update `main.rs` inconsistent imports to `ccs::session::SessionSource`
- [ ] read `src/tui/state.rs` lines 148-154 (`BackgroundSearchResult` type alias)
- [ ] replace type alias with named struct `BackgroundSearchResult { seq, query, paths, use_regex, result }`
- [ ] update construction site in `state.rs` background thread spawn (~line 255)
- [ ] update `handle_search_result` destructuring in `state.rs` (~line 594) to field access
- [ ] update all test construction sites (5 in `state.rs`, 1 in `search_mode.rs`) from tuple to struct literal
- [ ] run `cargo test` — all tests must pass
- [ ] run `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings`

### Task 4: Phase 1 verification and commit
- [ ] verify `search/ripgrep.rs` no longer imports anything from `resume`
- [ ] verify `resume/launcher.rs` no longer imports anything from `search`
- [ ] run full safety net: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- [ ] verify test count >= 365 (count `#[test]` across all .rs files)
- [ ] verify `cargo run -- --help` outputs usage info

<!--
PHASE 2: TUI Architecture Decomposition
Risk: MEDIUM. Scope: 5-6 files, 300-500 lines.
Dependency: Phase 1 complete.
-->

### Task 5: Extract `ResumeTarget` type — eliminate 4-Options invariant violation
- [ ] read `src/tui/state.rs` — identify all 4 resume fields: `resume_id`, `resume_file_path`, `resume_source`, `resume_uuid`
- [ ] read `into_outcome()` to understand the silent-Quit-on-missing-field behavior
- [ ] create `ResumeTarget` struct in `tui/state.rs` with fields: `session_id`, `file_path`, `source`, `uuid: Option<String>`, `query`
- [ ] create `AppOutcome` enum: `Resume(ResumeTarget)`, `Pick(PickedSession)`
- [ ] replace 4 `Option<String>` resume fields with `outcome: Option<AppOutcome>` in `App`
- [ ] update all sites that set resume fields (search_mode.rs `on_enter`, tree_mode.rs `on_enter_tree`) to construct `ResumeTarget`
- [ ] simplify `into_outcome()` to trivial match on `self.outcome`
- [ ] update tests that check `app.resume_id` / `app.resume_file_path` to check `app.outcome`
- [ ] run `cargo test` — all tests must pass

### Task 6: Extract `InputState` — centralize cursor invariant
- [ ] read `src/tui/state.rs` — identify `input`, `cursor_pos` fields and all cursor methods
- [ ] read `src/tui/search_mode.rs` — identify cursor-related methods called on App
- [ ] create `InputState` struct with `text: String`, `cursor_pos: usize` (private fields, public methods)
- [ ] move methods to `InputState`: `push_char`, `backspace`, `delete_forward`, `move_left/right`, `move_word_left/right`, `delete_word_left/right`, `move_home/end`, `clear`, `is_empty`, `text()`, `cursor_pos()`
- [ ] replace `self.input` / `self.cursor_pos` usages across `state.rs`, `search_mode.rs` with `self.input.method()`
- [ ] write tests for `InputState` in isolation: push_char, backspace at boundaries, word navigation, UTF-8 boundary safety
- [ ] run `cargo test` — all tests must pass

### Task 7: Add `KeyAction` enum and `classify_key` function
- [ ] read `src/main.rs` lines 157-295 (`run_tui_inner` event dispatch cascade)
- [ ] create `src/tui/dispatch.rs` with `KeyAction` enum (30+ variants covering all actions)
- [ ] implement `pub fn classify_key(key, tree_mode, expanded, input_at_end, input_empty) -> KeyAction`
- [ ] add `pub mod dispatch;` to `src/tui/mod.rs`
- [ ] write 30+ tests for `classify_key`: every key combination, modifiers, tree mode vs search mode, edge cases (Ctrl+H, Ctrl+Backspace)
- [ ] run `cargo test` — all tests must pass

### Task 8: Wire `KeyAction` dispatch into event loop
- [ ] implement `App::handle_action(&mut self, action: KeyAction)` — match on all KeyAction variants, delegate to existing methods
- [ ] replace the `if/continue/match` cascade in `main.rs` with: `let action = classify_key(...); app.handle_action(action);`
- [ ] remove dead code from old cascade in `main.rs`
- [ ] verify all keybindings work: run `cargo run` and test search, navigation, tree mode, Ctrl+C, Esc
- [ ] run `cargo test` — all tests must pass
- [ ] run `cargo clippy` — no dead code warnings

### Task 9: Extract `SearchState` and `TreeState` sub-structs
- [ ] identify search-related fields in `App`: `all_groups`, `groups`, `results_count`, `results_query`, `search_truncated`, `group_cursor`, `sub_cursor`, `expanded`, `search_rx`, `search_tx`, `search_seq`, `searching`, `error`, `latest_chains`
- [ ] create `SearchState` struct, move fields, update field access across `state.rs`, `search_mode.rs`, `render_search.rs`
- [ ] identify tree-related fields: `session_tree`, `tree_cursor`, `tree_scroll_offset`, `tree_loading`, `tree_standalone`, `tree_load_rx`
- [ ] create `TreeState` struct, move fields, update field access across `state.rs`, `tree_mode.rs`, `render_tree.rs`
- [ ] replace `app.field` with `app.search.field` / `app.tree.field` everywhere
- [ ] run `cargo test` — all tests must pass

### Task 10: Make render pure — `AppView` projection
- [ ] read `src/tui/render_tree.rs` line ~93 — confirm `app.tree_visible_height = visible_height` mutation
- [ ] create `AppView<'a>` struct in `src/tui/view.rs` with read-only references to all sub-states
- [ ] add `App::view(&self) -> AppView<'_>` method
- [ ] change `render()` in `render_search.rs` to take `&AppView` instead of `&mut App`
- [ ] change `render_tree_mode()` in `render_tree.rs` to take `&AppView` instead of `&mut App`
- [ ] add `last_tree_visible_height: usize` field to `App`, set from `frame.area().height` in event loop after draw
- [ ] remove `tree_visible_height` mutation from render — pass to TreeState navigation methods as parameter
- [ ] update snapshot tests in `tests/render_snapshots.rs` to use `app.view()`
- [ ] run `cargo test` — all tests must pass

### Task 11: Decompose `extract_summary` into testable phases
- [ ] read `src/recent.rs` lines ~511-728 (`extract_summary` function)
- [ ] create `ScanResult` struct: `session_id, first_user_message, last_summary, metadata_title, last_prompt, automation, lines_scanned, bytes_scanned`
- [ ] extract `scan_head(path, max_lines, latest_chain) -> Option<ScanResult>`
- [ ] extract `scan_tail(path, max_bytes, latest_chain) -> Option<ScanResult>`
- [ ] extract `scan_middle(path, start_line, end_byte, needs, latest_chain) -> Option<ScanResult>`
- [ ] rewrite `extract_summary` to call scan_head → scan_tail → scan_middle → merge
- [ ] write tests for `scan_head` with inline JSONL (user message, metadata, empty)
- [ ] write tests for `scan_tail` with inline JSONL (summary, custom-title, ai-title, agent-name)
- [ ] write tests for merge logic (priority: metadata_title > summary > last_prompt > first_user_message)
- [ ] run `cargo test recent` — all 48+ existing tests must pass, new tests added
- [ ] run full `cargo test`

### Task 12: Phase 2 verification
- [ ] verify `App` struct has <= 20 direct fields (sub-structs don't count)
- [ ] verify `render_search.rs` and `render_tree.rs` take `&AppView`, not `&mut App`
- [ ] verify `classify_key` has 30+ unit tests
- [ ] verify `InputState` methods enforce cursor_pos invariant
- [ ] run full safety net: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- [ ] verify test count > 366 (new tests for InputState, KeyAction, ScanResult)

<!--
PHASE 3: Session Data Layer
Risk: MEDIUM. Scope: 4-6 files, 200-400 lines.
Dependency: Phase 1 complete. Parallel with Phase 2.
-->

### Task 13: Add `SessionRecord` enum and `ContentBlock` types
- [ ] create `src/session/record.rs` (or extend `session.rs` with submodule)
- [ ] implement `SessionRecord` enum: Message, Summary, CustomTitle, AiTitle, AgentName, LastPrompt, CompactBoundary, Other, Metadata
- [ ] implement `ContentBlock` enum: Text, ToolUse, ToolResult, Thinking
- [ ] implement `ContentMode` enum: Full, Preview { max_chars }, TextOnly
- [ ] implement `MessageRole` enum: User, Assistant
- [ ] implement `SessionRecord::from_jsonl(line: &str) -> Option<Self>` — delegates to `session.rs` extractors
- [ ] implement `SessionRecord::render_content(blocks, mode) -> String` — Full joins with \n, TextOnly joins with space, Preview joins with space + truncates
- [ ] implement convenience methods: `dag_uuid()`, `dag_parent_uuid()`, `is_sidechain()`, `content_blocks()`
- [ ] add `pub mod record;` to session module
- [ ] write tests for `from_jsonl`: user message, assistant message, summary, custom-title, ai-title, agent-name, last-prompt, compact_boundary, unknown type, invalid JSON
- [ ] write tests for `render_content`: Full mode (text+tool_use+thinking), TextOnly mode (text only, space-joined), Preview mode (truncated, [tool: name] placeholders)
- [ ] run `cargo test`

### Task 14: Add `SessionDag` unified DAG engine
- [ ] create `src/dag/mod.rs`
- [ ] implement `TipStrategy` enum: LastAppended, MaxTimestamp
- [ ] implement `DisplayFilter` enum: Standard (user+assistant+compaction), MessagesOnly
- [ ] implement `DagEntry` struct: uuid, parent_uuid, timestamp, is_sidechain, is_displayable, line_index
- [ ] implement `SessionDag` struct: entries HashMap, parent_set, displayable_order Vec, last_uuid
- [ ] implement `SessionDag::from_file(path, filter) -> Result<Self, String>` — single-pass JSONL parse
- [ ] implement `SessionDag::from_records(records_iter, filter) -> Self` — for callers with pre-parsed records
- [ ] implement `SessionDag::tip(&self, strategy) -> Option<&str>` — both LastAppended and MaxTimestamp strategies
- [ ] implement `SessionDag::chain_from(&self, tip) -> HashSet<String>` — cycle-safe backward walk
- [ ] add `pub mod dag;` to `lib.rs`
- [ ] write tests: tip selection LastAppended (basic, sidechains excluded, compact_boundary bridging)
- [ ] write tests: tip selection MaxTimestamp (basic, clock skew, equal timestamps)
- [ ] write tests: chain_from (linear, branched, cycle guard)
- [ ] write tests: from_file with fixture files from `tests/fixtures/`
- [ ] run `cargo test`

### Task 15: Migrate `resume/fork.rs` to use `SessionDag`
- [ ] read `src/resume/fork.rs` — identify `DagInfo`, `parse_dag()`, `build_chain()`, `find_tip()` (private impl)
- [ ] replace `parse_dag` + `find_tip` + `build_chain` with `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from`
- [ ] update `is_on_latest_chain()` to use `SessionDag`
- [ ] update `build_chain_from_tip()` to use `SessionDag`
- [ ] remove `DagInfo` struct and private helpers that are now in `dag/mod.rs`
- [ ] keep `create_fork()` — it still needs its own file-writing pass, just chain logic simplified
- [ ] run `cargo test resume` — all 15 fork.rs tests + 5 mod.rs tests must pass unchanged
- [ ] run `cargo test --test resume_resolution` — all 8 integration tests must pass

### Task 16: Migrate `recent.rs` to use `SessionDag` and `SessionRecord`
- [ ] replace `build_latest_chain()` function in `recent.rs` with `SessionDag::from_file(path, Standard).tip(LastAppended).chain_from(tip)`
- [ ] replace `extract_text_content()` calls with `SessionRecord::render_content(blocks, ContentMode::TextOnly)`
- [ ] update title-extraction scan (summary_from_tail) to use `SessionRecord` match arms instead of raw JSON field access
- [ ] preserve fast-path string checks before JSON parsing (performance optimization for large files)
- [ ] delete `build_latest_chain` function and `extract_text_content` function from `recent.rs`
- [ ] run `cargo test recent` — all 48 tests must pass
- [ ] run full `cargo test`

### Task 17: Migrate `tree/mod.rs` to use `SessionDag` and `SessionRecord`
- [ ] replace local `build_latest_chain()` in `tree/mod.rs` with `SessionDag::from_records(records, Standard).tip(MaxTimestamp).chain_from(tip)`
- [ ] replace `extract_preview()` with `SessionRecord::render_content(blocks, ContentMode::Preview { max_chars: 120 })`
- [ ] update `get_full_content()` to use `SessionRecord::render_content(blocks, ContentMode::Full)` instead of `Message::extract_content`
- [ ] keep `DagNode`, `TreeRow`, `SessionTree` structs — they are tree-rendering model, not duplicated
- [ ] delete local `build_latest_chain` and `extract_preview` functions
- [ ] run `cargo test tree` — all 24 inline tests must pass
- [ ] run `cargo test --test tree_parsing` — all 8 integration tests must pass

### Task 18: Simplify `search/message.rs` to delegate to `SessionRecord`
- [ ] update `Message::from_jsonl` to use `SessionRecord::from_jsonl` internally, extract Message fields from SessionRecord::Message variant
- [ ] update `Message::extract_content` to delegate to `session::extract_message_content` (already done in Task 2) or `SessionRecord::render_content(_, Full)`
- [ ] verify all 16 tests in `search/message.rs` still pass
- [ ] run `cargo test search`
- [ ] run `cargo test --test cli_search`

### Task 19: Phase 3 verification
- [ ] verify `recent.rs` no longer has `build_latest_chain` function
- [ ] verify `recent.rs` no longer has `extract_text_content` function
- [ ] verify `tree/mod.rs` no longer has local `build_latest_chain` or `extract_preview`
- [ ] verify `resume/fork.rs` no longer has `DagInfo`, `parse_dag`, `find_tip`, `build_chain` (private impl)
- [ ] run full safety net: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- [ ] verify test count > 366 (new tests for SessionRecord, SessionDag)

<!--
PHASE 4: Integration Cleanup
Risk: LOW. Scope: 1-2 files, <50 lines.
Dependency: Phase 2 AND Phase 3 complete.
-->

### Task 20: Import cleanup and final polish
- [ ] remove `pub use message::SessionSource` re-export from `search/mod.rs` (if no external consumers rely on it)
- [ ] update all remaining `crate::search::SessionSource` imports to `crate::session::SessionSource` across tui/, cli.rs
- [ ] add `pub use session::SessionSource;` to `lib.rs` for external consumers
- [ ] run `cargo clippy --all-targets --all-features -- -D warnings` — fix any dead-code or unused-import warnings
- [ ] run `cargo fmt --check`
- [ ] run full `cargo test` — verify test count >= pre-refactoring baseline

### Task 21: Final acceptance verification
- [ ] verify all requirements from Overview are implemented: dep cycle broken, DAG unified, content extraction unified, App decomposed
- [ ] verify edge cases: compact_boundary bridging in DAG, sidechain exclusion, dual session format (CLI + Desktop)
- [ ] run full test suite: `cargo test`
- [ ] run linter: `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] verify `cargo run -- --help` works
- [ ] verify `cargo run -- search "test"` works (if sessions exist)
- [ ] verify `cargo run -- list` works

### Task 22: Update documentation
- [ ] update CLAUDE.md Architecture section if file structure changed (new `dag/mod.rs`, `session/record.rs`)
- [ ] update CLAUDE.md Key data flow section if DAG/content extraction paths changed

## Technical Details

### Content extraction separator mapping
| Caller | Current join | ContentMode | Behavior |
|--------|-------------|-------------|----------|
| `search/message.rs` | `\n` | `Full` | All block types, newline-joined |
| `recent.rs` | `" "` | `TextOnly` | Text blocks only, space-joined |
| `tree/mod.rs` | `" "` + truncate | `Preview { max_chars: 120 }` | All types with placeholders, space-joined, truncated |

### DAG tip strategy mapping
| Caller | Current strategy | TipStrategy | Rationale |
|--------|-----------------|-------------|-----------|
| `resume/fork.rs` | Last displayable not-in-parent-set by append order | `LastAppended` | "Last thing user was working on" — cheap, no timestamp parse |
| `recent.rs` | Same as fork.rs | `LastAppended` | Same use case |
| `tree/mod.rs` | Max timestamp among displayable terminals | `MaxTimestamp` | Handles clock-skew in visual tree rendering |

### Phase dependency graph
```
Task 1-4 (Phase 1) → Task 5-12 (Phase 2, can be parallel with 13-19)
Task 1-4 (Phase 1) → Task 13-19 (Phase 3, can be parallel with 5-12)
Task 5-12 + Task 13-19 → Task 20-22 (Phase 4)
```

## Post-Completion
**Manual verification:**
- Run TUI mode and exercise: search query, navigate results, enter tree mode, exit tree, Ctrl+C, resume session
- Verify overlay mode: `cargo run -- --overlay`
- Verify pick mode: `cargo run -- pick`

**PR strategy:**
- PR-1: Tasks 1-4 (Phase 1: dep-cycle fix) — prerequisite
- PR-2: Tasks 5-12 (Phase 2: TUI decomp) — after PR-1
- PR-3: Tasks 13-19 (Phase 3: session data) — after PR-1, parallel with PR-2
- PR-4: Tasks 20-22 (Phase 4: cleanup) — after PR-2 and PR-3
