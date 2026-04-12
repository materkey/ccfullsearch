# AI-powered session re-ranking via Ctrl+G

## Overview
- Users see 20+ sessions with useless summaries ("давай", "Tool loaded.", "commit") and can't find the right one
- New feature: press Ctrl+G, type a natural language query, AI re-ranks visible sessions by relevance
- Backend: `claude -p` (Claude CLI one-shot mode) with first 3 user messages from each session as context
- Keybinding is Ctrl+G (not Ctrl+I which conflicts with Tab in terminals without Kitty protocol)

## Context (from discovery)
- Background tasks use `std::thread` + `mpsc` channels polled via `try_recv()` in `App::tick()`
- One-shot pattern: `Option<Receiver<T>>` set to `None` after receive (see `RecentState::spawn_load`)
- Key dispatch: `KeyAction` enum → `classify_key()` → `handle_action()` match
- Rendering: 5-row layout (header/input/status/list/help), `AppView` deref to `&App`
- Session data: `session::extract_record_type`, `session::record::render_text_content` are public APIs
- Claude binary: `which::which("claude")` pattern in `src/resume/launcher.rs`
- No async runtime — pure `std::thread` + `mpsc`

## Development Approach
- **Testing approach**: Regular — write tests alongside implementation
- Complete each task fully before moving to the next
- Each task ends with `cargo clippy && cargo test`

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Create `src/ai.rs` — standalone AI module
- [x] DONE — `src/ai.rs` created (353 lines), `pub mod ai;` added to `src/lib.rs`, all 8 tests pass, clippy clean
- [x] NOTE: `src/ai.rs` is untracked — needs `git add src/ai.rs` before commit
- [x] NOTE: `spawn_ai_rank` returns `Result<Receiver<AiRankResult>, String>` (not bare Receiver) — error on missing claude binary is synchronous

### Task 2: Update `src/tui/dispatch.rs` — key actions and Ctrl+G mapping
- [x] add `EnterAiMode` and `ExitAiMode` variants to `KeyAction` enum (in the toggles section)
- [x] add `pub ai_mode: bool` field to `KeyContext` struct
- [x] in `classify_search_key`: add Ctrl+G handler after Ctrl+E block — returns `ExitAiMode` if `ctx.ai_mode`, else `EnterAiMode`
- [x] update Esc handling in `classify_search_key`: check `ctx.ai_mode` first → `ExitAiMode`, then `ctx.preview_mode` → `ExitPreview`, else `Quit`
- [x] update `search_ctx()` in tests to include `ai_mode: false`
- [x] add tests: `test_ctrl_g_enters_ai_mode`, `test_ctrl_g_in_ai_mode_exits`, `test_esc_in_ai_mode_exits_ai`
- [x] verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 3: Update `src/tui/state.rs` — AiState struct and App integration
- [x] define `AiState` struct after `TreeState`: fields `active: bool`, `query: InputState`, `thinking: bool`, `result_rx: Option<Receiver<ai::AiRankResult>>`, `error: Option<String>`, `ranked_count: Option<usize>`, `original_recent_order: Option<Vec<RecentSession>>`, `original_groups_order: Option<Vec<SessionGroup>>`
- [x] add `pub ai: AiState` field to `App` struct
- [x] initialize `ai` in `App::new()` Self block with all defaults
- [x] add `ai_mode: self.ai.active` to `key_context()` method
- [x] add AI input routing guard at top of `handle_action()` BEFORE the match: when `self.ai.active`, route InputChar/Backspace/Delete/ClearInput/word-movement/cursor-movement/Enter to `self.ai.query` methods and `submit_ai_query()`, fall through for Up/Down/Esc/Ctrl+G
- [x] add `KeyAction::EnterAiMode => self.enter_ai_mode()` and `KeyAction::ExitAiMode => self.exit_ai_mode()` arms to the match
- [x] implement `enter_ai_mode()`: set active=true, clear query/error/ranked_count
- [x] implement `exit_ai_mode()`: set active=false/thinking=false, drop result_rx, restore original order from saved vecs if present, clear all ai state
- [x] implement `submit_ai_query()`: if query empty or thinking return; collect SessionContext from recent.filtered or search.groups; build_prompt; set thinking=true; spawn_ai_rank and store receiver
- [x] implement `handle_ai_result(result)`: on error set ai.error; build rank HashMap; save original order on first rank; sort filtered/groups by rank; reset cursor to 0; set ranked_count
- [x] add AI result polling in `tick()` after tree load polling: `try_recv()` on `ai.result_rx`, call `handle_ai_result` on success
- [x] verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 4: Update `src/tui/render_search.rs` — AI mode UI
- [x] input box: change title to "AI" when `app.ai.active`, keep existing filter flags appended
- [x] input box: render `app.ai.query.text()` and use `app.ai.query.cursor_pos()` when ai.active, else use `app.input`
- [x] input box style: magenta when ai.active
- [x] status bar: add AI cases at TOP of priority chain — `ai.thinking` → magenta "AI thinking...", `ai.error` → red, `ai.ranked_count` → green bold "AI: N ranked", `ai.active` idle → magenta "Type query, Enter to rank"
- [x] help bar: add AI mode branch before preview_mode — show "[Enter] Rank  [↑↓] Navigate  [Esc/Ctrl+G] Cancel"
- [x] help bar: add `[Ctrl+G] AI` hint to existing recent-sessions and search-results help bars
- [x] verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 5: Guard `on_enter` in `src/tui/search_mode.rs`
- [x] add `if self.ai.active { return; }` at top of `on_enter()` as safety net
- [x] verify: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 6: Final verification
- [x] run full CI check: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
- [x] verify no warnings, no test failures

## Post-Completion
- Manual smoke test: `cargo run`, Ctrl+G, type query, Enter → verify AI ranking works
- Test without claude binary: verify error message in status bar
- Test Esc to restore original order
