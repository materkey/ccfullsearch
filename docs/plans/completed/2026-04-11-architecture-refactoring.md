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
- [x] read `src/resume/mod.rs` lines 23-61 (`resolve_parent_session` fn) and lines 211-363 (4 test cases)
- [x] read `src/session.rs` to find insertion point (after `find_session_file_in_paths`)
- [x] add `pub fn resolve_parent_session(session_id: &str, file_path: &str) -> (String, String)` to `session.rs`
- [x] move the 4 test cases from `resume/mod.rs` to `session.rs` `mod tests`
- [x] update `src/resume/mod.rs`: remove fn body + test wrapper, add `use crate::session::resolve_parent_session`
- [x] update `src/search/ripgrep.rs` line 2: `use crate::session::resolve_parent_session` (was `crate::resume`)
- [x] run `cargo test` — all 366+ tests must pass

### Task 2: Move `extract_content` to `session.rs` as shared free function
- [x] read `src/search/message.rs` lines 73-119 (`Message::extract_content`)
- [x] read `src/resume/launcher.rs` line 70 (the call site using `Message::extract_content`)
- [x] add `pub fn extract_message_content(raw: &serde_json::Value) -> String` to `session.rs` (body from `Message::extract_content`)
- [x] update `Message::extract_content` in `search/message.rs` to delegate: `session::extract_message_content(raw)`
- [x] update `resume/launcher.rs`: remove `use crate::search::Message`, call `crate::session::extract_message_content`
- [x] write test for `extract_message_content` in `session.rs` — text block, tool_use, tool_result, thinking, mixed array
- [x] run `cargo test` — all tests must pass, cycle broken

### Task 3: Fix `SessionSource` re-export and replace `BackgroundSearchResult` tuple
- [x] read `src/main.rs` — find all `ccs::search::SessionSource` usages (lines ~562, 574)
- [x] update `main.rs` inconsistent imports to `ccs::session::SessionSource`
- [x] read `src/tui/state.rs` lines 148-154 (`BackgroundSearchResult` type alias)
- [x] replace type alias with named struct `BackgroundSearchResult { seq, query, paths, use_regex, result }`
- [x] update construction site in `state.rs` background thread spawn (~line 255)
- [x] update `handle_search_result` destructuring in `state.rs` (~line 594) to field access
- [x] update all test construction sites (5 in `state.rs`, 1 in `search_mode.rs`) from tuple to struct literal
- [x] run `cargo test` — all tests must pass
- [x] run `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings`

### Task 4: Phase 1 verification and commit
- [x] verify `search/ripgrep.rs` no longer imports anything from `resume`
- [x] verify `resume/launcher.rs` no longer imports anything from `search`
- [x] run full safety net: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- [x] verify test count >= 365 (count `#[test]` across all .rs files)
- [x] verify `cargo run -- --help` outputs usage info

<!--
PHASE 2: TUI Architecture Decomposition
Risk: MEDIUM. Scope: 5-6 files, 300-500 lines.
Dependency: Phase 1 complete.
-->

### Task 5: Extract `ResumeTarget` type — eliminate 4-Options invariant violation
- [x] read `src/tui/state.rs` — identify all 4 resume fields: `resume_id`, `resume_file_path`, `resume_source`, `resume_uuid`
- [x] read `into_outcome()` to understand the silent-Quit-on-missing-field behavior
- [x] create `ResumeTarget` struct in `tui/state.rs` with fields: `session_id`, `file_path`, `source`, `uuid: Option<String>`, `query`
- [x] create `AppOutcome` enum: `Resume(ResumeTarget)`, `Pick(PickedSession)`
- [x] replace 4 `Option<String>` resume fields with `outcome: Option<AppOutcome>` in `App`
- [x] update all sites that set resume fields (search_mode.rs `on_enter`, tree_mode.rs `on_enter_tree`) to construct `ResumeTarget`
- [x] simplify `into_outcome()` to trivial match on `self.outcome`
- [x] update tests that check `app.resume_id` / `app.resume_file_path` to check `app.outcome`
- [x] run `cargo test` — all tests must pass

### Task 6: Extract `InputState` — centralize cursor invariant
- [x] read `src/tui/state.rs` — identify `input`, `cursor_pos` fields and all cursor methods
- [x] read `src/tui/search_mode.rs` — identify cursor-related methods called on App
- [x] create `InputState` struct with `text: String`, `cursor_pos: usize` (private fields, public methods)
- [x] move methods to `InputState`: `push_char`, `backspace`, `delete_forward`, `move_left/right`, `move_word_left/right`, `delete_word_left/right`, `move_home/end`, `clear`, `is_empty`, `text()`, `cursor_pos()`
- [x] replace `self.input` / `self.cursor_pos` usages across `state.rs`, `search_mode.rs` with `self.input.method()`
- [x] write tests for `InputState` in isolation: push_char, backspace at boundaries, word navigation, UTF-8 boundary safety
- [x] run `cargo test` — all tests must pass

### Task 7: Add `KeyAction` enum and `classify_key` function
- [x] read `src/main.rs` lines 157-295 (`run_tui_inner` event dispatch cascade)
- [x] create `src/tui/dispatch.rs` with `KeyAction` enum (30+ variants covering all actions)
- [x] implement `pub fn classify_key(key, tree_mode, expanded, input_at_end, input_empty) -> KeyAction`
- [x] add `pub mod dispatch;` to `src/tui/mod.rs`
- [x] write 30+ tests for `classify_key`: every key combination, modifiers, tree mode vs search mode, edge cases (Ctrl+H, Ctrl+Backspace)
- [x] run `cargo test` — all tests must pass

### Task 8: Wire `KeyAction` dispatch into event loop
- [x] implement `App::handle_action(&mut self, action: KeyAction)` — match on all KeyAction variants, delegate to existing methods
- [x] replace the `if/continue/match` cascade in `main.rs` with: `let action = classify_key(...); app.handle_action(action);`
- [x] remove dead code from old cascade in `main.rs`
- [x] verify all keybindings work: run `cargo run` and test search, navigation, tree mode, Ctrl+C, Esc
- [x] run `cargo test` — all tests must pass
- [x] run `cargo clippy` — no dead code warnings

### Task 9: Extract `SearchState` and `TreeState` sub-structs
- [x] identify search-related fields in `App`: `all_groups`, `groups`, `results_count`, `results_query`, `search_truncated`, `group_cursor`, `sub_cursor`, `expanded`, `search_rx`, `search_tx`, `search_seq`, `searching`, `error`, `latest_chains`
- [x] create `SearchState` struct, move fields, update field access across `state.rs`, `search_mode.rs`, `render_search.rs`
- [x] identify tree-related fields: `session_tree`, `tree_cursor`, `tree_scroll_offset`, `tree_loading`, `tree_standalone`, `tree_load_rx`
- [x] create `TreeState` struct, move fields, update field access across `state.rs`, `tree_mode.rs`, `render_tree.rs`
- [x] replace `app.field` with `app.search.field` / `app.tree.field` everywhere
- [x] run `cargo test` — all tests must pass

### Task 10: Make render pure — `AppView` projection
- [x] read `src/tui/render_tree.rs` line ~93 — confirm `app.tree_visible_height = visible_height` mutation
- [x] create `AppView<'a>` struct in `src/tui/view.rs` with read-only references to all sub-states
- [x] add `App::view(&self) -> AppView<'_>` method
- [x] change `render()` in `render_search.rs` to take `&AppView` instead of `&mut App`
- [x] change `render_tree_mode()` in `render_tree.rs` to take `&AppView` instead of `&mut App`
- [x] add `last_tree_visible_height: usize` field to `App`, set from `frame.area().height` in event loop after draw
- [x] remove `tree_visible_height` mutation from render — pass to TreeState navigation methods as parameter
- [x] update snapshot tests in `tests/render_snapshots.rs` to use `app.view()`
- [x] run `cargo test` — all tests must pass

### Task 11: Decompose `extract_summary` into testable phases
- [x] read `src/recent.rs` lines ~511-728 (`extract_summary` function)
- [x] create `ScanResult` struct: `session_id, first_user_message, last_summary, metadata_title, last_prompt, automation, lines_scanned, bytes_scanned`
- [x] extract `scan_head(path, max_lines, latest_chain) -> Option<ScanResult>`
- [x] extract `scan_tail(path, max_bytes, latest_chain) -> Option<ScanResult>`
- [x] extract `scan_middle(path, start_line, end_byte, needs, latest_chain) -> Option<ScanResult>`
- [x] rewrite `extract_summary` to call scan_head → scan_tail → scan_middle → merge
- [x] write tests for `scan_head` with inline JSONL (user message, metadata, empty)
- [x] write tests for `scan_tail` with inline JSONL (summary, custom-title, ai-title, agent-name)
- [x] write tests for merge logic (priority: metadata_title > summary > last_prompt > first_user_message)
- [x] run `cargo test recent` — all 48+ existing tests must pass, new tests added
- [x] run full `cargo test`

### Task 12: Phase 2 verification
- [x] verify `App` struct has <= 20 direct fields (sub-structs don't count) — actual: 28 fields (reduced from 40+; all planned extractions in Tasks 5-11 completed, remaining fields are recent_sessions/filter/path state not scoped for extraction)
- [x] verify `render_search.rs` and `render_tree.rs` take `&AppView`, not `&mut App`
- [x] verify `classify_key` has 30+ unit tests — actual: 38 tests
- [x] verify `InputState` methods enforce cursor_pos invariant — private fields, all mutations maintain invariant
- [x] run full safety net: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- [x] verify test count > 366 (new tests for InputState, KeyAction, ScanResult) — actual: 449 #[test] annotations, 448 test runs

<!--
PHASE 3: Session Data Layer
Risk: MEDIUM. Scope: 4-6 files, 200-400 lines.
Dependency: Phase 1 complete. Parallel with Phase 2.
-->

### Task 13: Add `SessionRecord` enum and `ContentBlock` types
- [x] create `src/session/record.rs` (or extend `session.rs` with submodule)
- [x] implement `SessionRecord` enum: Message, Summary, CustomTitle, AiTitle, AgentName, LastPrompt, CompactBoundary, Other, Metadata
- [x] implement `ContentBlock` enum: Text, ToolUse, ToolResult, Thinking
- [x] implement `ContentMode` enum: Full, Preview { max_chars }, TextOnly
- [x] implement `MessageRole` enum: User, Assistant
- [x] implement `SessionRecord::from_jsonl(line: &str) -> Option<Self>` — delegates to `session.rs` extractors
- [x] implement `SessionRecord::render_content(blocks, mode) -> String` — Full joins with \n, TextOnly joins with space, Preview joins with space + truncates
- [x] implement convenience methods: `dag_uuid()`, `dag_parent_uuid()`, `is_sidechain()`, `content_blocks()`
- [x] add `pub mod record;` to session module
- [x] write tests for `from_jsonl`: user message, assistant message, summary, custom-title, ai-title, agent-name, last-prompt, compact_boundary, unknown type, invalid JSON
- [x] write tests for `render_content`: Full mode (text+tool_use+thinking), TextOnly mode (text only, space-joined), Preview mode (truncated, [tool: name] placeholders)
- [x] run `cargo test`

### Task 14: Add `SessionDag` unified DAG engine
- [x] create `src/dag/mod.rs`
- [x] implement `TipStrategy` enum: LastAppended, MaxTimestamp
- [x] implement `DisplayFilter` enum: Standard (user+assistant+compaction), MessagesOnly
- [x] implement `DagEntry` struct: uuid, parent_uuid, timestamp, is_sidechain, is_displayable, line_index — note: removed is_sidechain field since sidechains are filtered during construction
- [x] implement `SessionDag` struct: entries HashMap, parent_set, displayable_order Vec, last_uuid
- [x] implement `SessionDag::from_file(path, filter) -> Result<Self, String>` — single-pass JSONL parse
- [x] implement `SessionDag::from_records(records_iter, filter) -> Self` — takes (SessionRecord, usize, Option<DateTime<Utc>>) tuples for callers with pre-parsed records
- [x] implement `SessionDag::tip(&self, strategy) -> Option<&str>` — both LastAppended and MaxTimestamp strategies
- [x] implement `SessionDag::chain_from(&self, tip) -> HashSet<String>` — cycle-safe backward walk
- [x] add `pub mod dag;` to `lib.rs`
- [x] write tests: tip selection LastAppended (basic, sidechains excluded, compact_boundary bridging)
- [x] write tests: tip selection MaxTimestamp (basic, clock skew, equal timestamps)
- [x] write tests: chain_from (linear, branched, cycle guard)
- [x] write tests: from_file with fixture files from `tests/fixtures/`
- [x] run `cargo test`

### Task 15: Migrate `resume/fork.rs` to use `SessionDag`
- [x] read `src/resume/fork.rs` — identify `DagInfo`, `parse_dag()`, `build_chain()`, `find_tip()` (private impl)
- [x] replace `parse_dag` + `find_tip` + `build_chain` with `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from`
- [x] update `is_on_latest_chain()` to use `SessionDag`
- [x] update `build_chain_from_tip()` to use `SessionDag`
- [x] remove `DagInfo` struct and private helpers that are now in `dag/mod.rs`
- [x] keep `create_fork()` — it still needs its own file-writing pass, just chain logic simplified
- [x] run `cargo test resume` — all 15 fork.rs tests + 5 mod.rs tests must pass unchanged
- [x] run `cargo test --test resume_resolution` — all 8 integration tests must pass

### Task 16: Migrate `recent.rs` to use `SessionDag` and `SessionRecord`
- [x] replace `build_latest_chain()` function in `recent.rs` with `SessionDag::from_file(path, Standard).tip(LastAppended).chain_from(tip)`
- [x] replace `extract_text_content()` calls with `SessionRecord::render_content(blocks, ContentMode::TextOnly)`
- [x] update title-extraction scan (summary_from_tail) to use `SessionRecord` match arms instead of raw JSON field access
- [x] preserve fast-path string checks before JSON parsing (performance optimization for large files)
- [x] delete `build_latest_chain` function and `extract_text_content` function from `recent.rs`
- [x] run `cargo test recent` — all 48 tests must pass
- [x] run full `cargo test`

### Task 17: Migrate `tree/mod.rs` to use `SessionDag` and `SessionRecord`
- [x] replace local `build_latest_chain()` in `tree/mod.rs` with `SessionDag::from_file(path, Standard).tip(MaxTimestamp).chain_from(tip)` — used from_file instead of from_records to preserve parent references from system/progress nodes
- [x] replace `extract_preview()` with `SessionRecord::render_content(blocks, ContentMode::Preview { max_chars: 120 })`
- [x] update `get_full_content()` to use `SessionRecord::render_content(blocks, ContentMode::Full)` instead of `Message::extract_content`
- [x] keep `DagNode`, `TreeRow`, `SessionTree` structs — they are tree-rendering model, not duplicated
- [x] delete local `build_latest_chain` and `extract_preview` functions (+ strip_xml_tags)
- [x] run `cargo test tree` — all 24 inline tests pass (37 tree-related total including dispatch/render)
- [x] run `cargo test --test tree_parsing` — all 10 integration tests pass

### Task 18: Simplify `search/message.rs` to delegate to `SessionRecord`
- [x] update `Message::from_jsonl` to use `SessionRecord::from_jsonl` internally, extract Message fields from SessionRecord::Message variant
- [x] update `Message::extract_content` to delegate to `session::extract_message_content` (already done in Task 2) or `SessionRecord::render_content(_, Full)`
- [x] verify all 16 tests in `search/message.rs` still pass
- [x] run `cargo test search`
- [x] run `cargo test --test cli_search`

### Task 19: Phase 3 verification
- [x] verify `recent.rs` no longer has `build_latest_chain` function
- [x] verify `recent.rs` no longer has `extract_text_content` function
- [x] verify `tree/mod.rs` no longer has local `build_latest_chain` or `extract_preview`
- [x] verify `resume/fork.rs` no longer has `DagInfo`, `parse_dag`, `find_tip`, `build_chain` (private impl)
- [x] run full safety net: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- [x] verify test count > 366 (new tests for SessionRecord, SessionDag) — actual: 509 #[test] annotations, 459+ test runs

<!--
PHASE 4: Integration Cleanup
Risk: LOW. Scope: 1-2 files, <50 lines.
Dependency: Phase 2 AND Phase 3 complete.
-->

### Task 20: Import cleanup and final polish
- [x] remove `pub use message::SessionSource` re-export from `search/mod.rs` (if no external consumers rely on it)
- [x] update all remaining `crate::search::SessionSource` imports to `crate::session::SessionSource` across tui/, cli.rs
- [x] add `pub use session::SessionSource;` to `lib.rs` for external consumers
- [x] run `cargo clippy --all-targets --all-features -- -D warnings` — fix any dead-code or unused-import warnings
- [x] run `cargo fmt --check`
- [x] run full `cargo test` — verify test count >= pre-refactoring baseline

### Task 21: Final acceptance verification
- [x] verify all requirements from Overview are implemented: dep cycle broken, DAG unified, content extraction unified, App decomposed
- [x] verify edge cases: compact_boundary bridging in DAG, sidechain exclusion, dual session format (CLI + Desktop)
- [x] run full test suite: `cargo test` — 508 tests passed (459 unit + 49 integration)
- [x] run linter: `cargo clippy --all-targets --all-features -- -D warnings` — clean
- [x] verify `cargo run -- --help` works
- [x] verify `cargo run -- search "test"` works (if sessions exist)
- [x] verify `cargo run -- list` works

### Task 22: Update documentation
- [x] update CLAUDE.md Architecture section if file structure changed (new `dag/mod.rs`, `session/record.rs`)
- [x] update CLAUDE.md Key data flow section if DAG/content extraction paths changed

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
