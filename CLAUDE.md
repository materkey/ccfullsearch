# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**ccfullsearch** (`ccs`) — a TUI and CLI tool for searching and browsing Claude Code CLI and Claude Desktop session history. Built in Rust with ratatui (TUI), ripgrep (search), crossterm (terminal), clap (CLI parsing), and rayon (parallelism). Requires `rg` (ripgrep) in PATH at runtime.

## Build & Development Commands

```bash
cargo build                    # Debug build
cargo build --release          # Release build
cargo run                      # Run TUI mode
cargo run -- search "query"    # CLI search subcommand
cargo run -- list              # List all sessions
cargo run -- update            # Self-update to latest release
cargo run -- --tree <file|id>  # Open tree mode directly for a session
cargo run -- pick              # Pick a session interactively (key-value output)
cargo run -- pick "query"      # Pick with pre-filled search query
cargo run -- --overlay         # Overlay mode: resume as child, return to TUI

cargo fmt --check              # Check formatting
cargo clippy --all-targets --all-features -- -D warnings  # Lint
cargo test                     # Run all tests
cargo test tree_parsing        # Run a single test module
cargo test test_search_finds   # Run a single test by name
```

CI runs: `cargo fmt --check` → `cargo clippy` → `cargo test` (see `.github/workflows/ci.yml`).

## Architecture

```
src/
├── main.rs           # CLI parsing (clap), panic handler, run_tui() lifecycle, TuiOutcome-based outer loop
├── lib.rs            # Module re-exports + get_search_paths()
├── cli.rs            # Non-interactive subcommands (search, list)
├── recent.rs         # RecentSession struct, parallel scanning (rayon), summary extraction via SessionDag/SessionRecord
├── ai.rs             # AI session re-ranking: SessionContext, AiRankResult, collect_session_context(), build_prompt(), parse_ai_response(), spawn_ai_rank() — invokes `claude -p` in background thread via std::thread + mpsc
├── update.rs         # Self-update: GitHub release download, Homebrew detection, version comparison
├── session/
│   ├── mod.rs        # SessionSource enum (ClaudeCodeCLI | ClaudeDesktop), shared field extractors, resolve_parent_session, extract_message_content
│   └── record.rs     # SessionRecord enum (Message, Summary, CustomTitle, etc.), ContentBlock, ContentMode, MessageRole, parse_content_blocks() (handles text, tool_use, tool_result, thinking, image, document, redacted_thinking, server_tool_use, connector_text), render_content() — unified JSONL parsing and content rendering
├── dag/
│   └── mod.rs        # SessionDag — unified DAG engine: TipStrategy (LastAppended | MaxTimestamp), DisplayFilter, chain_from(), from_file()
├── search/
│   ├── mod.rs        # Module re-exports for search API surface
│   ├── ripgrep.rs    # Spawns `rg --json`, parses matches, post-filters content, extracts project names
│   ├── message.rs    # Parses JSONL lines into Message structs (content via Full mode, text_content via TextOnly for preview), delegates to SessionRecord
│   └── group.rs      # Groups RipgrepMatch by session_id, sorts by timestamp
├── tree/
│   └── mod.rs        # DagNode/TreeRow/SessionTree rendering model, uses SessionDag for chain building and SessionRecord for content
├── resume/
│   ├── mod.rs        # Resume orchestration (CLI exec vs Desktop open, resume_child for overlay)
│   ├── path_codec.rs # Encodes/decodes filesystem paths to Claude's dash-separated format
│   ├── fork.rs       # Creates forked JSONL files for branch-aware resume, uses SessionDag for chain logic
│   └── launcher.rs   # Process exec (Unix) / spawn (Windows), resume_cli_child() for overlay, safe path fallback (decode→parent→$HOME→/tmp), atomic session index
└── tui/
    ├── state.rs      # App struct (with InputState, SearchState, TreeState, AiState sub-structs), ResumeTarget, AppOutcome, BackgroundSearchResult, SearchHandle (per-request cancel token), spawn-per-request search with cooperative + forced (Child::kill) cancellation, AI re-ranking lifecycle
    ├── dispatch.rs   # KeyAction enum (incl. EnterAiMode/ExitAiMode), KeyContext struct (incl. ai_mode), classify_key() — maps key events to semantic actions
    ├── view.rs       # AppView<'a> — read-only projection of App for pure rendering
    ├── search_mode.rs# Search navigation, filtering, input handling, recent sessions navigation
    ├── tree_mode.rs  # Tree mode enter/exit, DAG navigation
    ├── render_search.rs # Search results + preview rendering (takes &AppView, not &mut App); defines HintItem/build_help_line() for adaptive width-aware help bars, used by render_tree.rs
    └── render_tree.rs   # Tree DAG rendering with graph symbols (takes &AppView, not &mut App); imports HintItem/build_help_line from render_search.rs

.claude-plugin/
├── plugin.json                         # Claude Code plugin manifest (name, version, skills path)
└── skills/ccs/
    ├── SKILL.md                        # Skill definition with CLI and overlay picker modes
    └── scripts/launch-ccs.sh           # Shell launcher: tmux/kitty/wezterm overlay for ccs pick
```

### Key data flow

1. **Search**: User types query → 300ms debounce → `start_search` increments `search_seq` and cancels the prior request via `SearchHandle.cancel.store(true)` (dropping the previous handle), then spawns a fresh per-request thread with its own `Arc<AtomicBool>` cancel token → that thread streams `rg --json --glob="*.jsonl"` via `Command::spawn()` + `BufReader::lines()`, checking `cancel.load()` on every line; on cancel it calls `child.kill()` + `child.wait()` and returns `Err("cancelled")` → on completion the thread only sends a `BackgroundSearchResult` if `!cancel.load()` (preventing zombie sends from a request that has been superseded) → `App.tick()` polls `result_rx` and discriminates by `seq == current.seq` (drops stale results); on a match it clears `current` (which makes `is_searching()` return `false`) and processes the matches: parse JSONL into `Message` (each Message carries both `content` via Full mode and `text_content` via TextOnly mode for clean preview display) → **post-filter** to ensure query matches message *content* (not metadata) → group by `session_id` → sort by timestamp desc → if any file hit the per-file match limit (1000), flag `truncated` and show warning in status bar. `SearchHandle { seq, cancel }` lives in `src/tui/state.rs` and is the single source of truth for in-flight state — `is_searching()` is a derived predicate (`self.search.current.is_some()`), not a stored flag.
2. **Tree mode**: Load full JSONL file → `SessionDag::from_file(path, Standard)` builds DAG in single pass (parses via `SessionRecord::from_jsonl`, filters sidechains, bridges `compact_boundary` via `logicalParentUuid`) → `dag.tip(MaxTimestamp)` picks latest terminal → `dag.chain_from(tip)` walks backward → mark branch points (nodes with >1 child) → flatten to `TreeRow` list. Content rendered via `SessionRecord::render_content(blocks, Preview { max_chars: 120 })`
3. **Resume**: On Enter, find `claude` binary via `which` → resolve project working directory from session path (only use decoded path if it exists on disk; fall back to session file parent → `$HOME` → `/tmp`) → if selected message is NOT on latest chain, create a forked JSONL file: `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from(tip)` determine the chain, then write filtered records → exec/spawn `claude --resume <file-path>` (absolute `.jsonl` path for cross-project support). Session index uses per-process temp files for atomic writes.
4. **TUI lifecycle**: `main()` calls `run_tui()` → key events go through `classify_key()` → `KeyAction` enum → `App::handle_action()`. Returns `TuiOutcome` (Quit, Resume, Pick). In overlay mode (`--overlay`), `Resume` spawns Claude as child via `resume_cli_child()` and loops back. In normal mode, `Resume` calls `resume()` (exec, replaces process). `Pick` writes `PickedSession` key-value output and exits 0/1. Rendering uses `AppView` (read-only projection) — no mutation during draw.
5. **Recent sessions**: App starts → background thread walks search dirs for `*.jsonl` (skip `agent-*` and `subagents/`) → sort by mtime → take top 100 → rayon parallel: `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from(tip)` for latest chain, then scan JSONL using `SessionRecord` match arms for title extraction (priority: agentName > customTitle > aiTitle > summary > lastPrompt > firstUserMessage) → deduplicate by session_id (keep newest) → sort by **content timestamp** desc (max `timestamp`/`_audit_timestamp` from JSONL lines; falls back to file mtime) → send via mpsc to TUI → render in empty-state view
6. **AI re-ranking (Ctrl+G)**: Press Ctrl+G → `EnterAiMode` → `AiState.active = true`, input switches to AI query (magenta). User types query, Enter → `submit_ai_query()` collects `SessionContext` from visible sessions, calls `build_prompt()`, then `spawn_ai_rank()` invokes `claude -p` in background thread. `AiRankResult` arrives via mpsc, polled in `tick()` → `handle_ai_result()` saves original order, re-sorts by rank, and sets `ai.ranked_count = Some(n)` only when at least one visible session matched. If Claude returns `[]`, AI mode surfaces a retryable no-match message and keeps Enter bound to re-rank instead of resume. Enter after a non-empty rank routes to `on_enter_inner()` (bypasses the `if self.ai.active { return; }` guard in the public `on_enter` wrapper) and resumes the selected session; Enter before a rank stays on `submit_ai_query()`. Any query mutation (InputChar/Backspace/Delete/ClearInput/DeleteWordLeft/DeleteWordRight) calls `invalidate_ai_rank()` which clears `error`, `ranked_count`, drops `result_rx`, and resets `thinking`, so the rank is re-applied and a stale in-flight response cannot restore the flag via `handle_ai_result`. Filter/scope toggles while `ai.active` (`Ctrl+R` regex, `Ctrl+A` project, `Ctrl+H` automation) call the same `invalidate_ai_rank()` at the top of `on_toggle_regex` / `toggle_automation_filter` / `toggle_project_filter` — they rebuild the candidate list, so the rank is stale against the new set. Cursor-only movements do not reset. Ctrl+G or Esc → `exit_ai_mode()` restores original order.

### Dual format support

The tool handles two session formats with different field names:
- **Claude Code CLI** (`~/.claude/projects/`): `sessionId`, `timestamp`, has `branch`/`gitBranch`
- **Claude Desktop** (`~/Library/Application Support/Claude/local-agent-mode-sessions/` on macOS, `~/.config/Claude/local-agent-mode-sessions/` on Linux): `session_id`, `_audit_timestamp`, no branch info

The `SessionSource` enum in `session/mod.rs` drives format-specific parsing throughout. The `SessionRecord` enum in `session/record.rs` provides unified JSONL line parsing across both formats.

## Testing

- **Unit tests**: Inline `#[cfg(test)]` modules in source files
- **Integration tests**: `tests/` directory using `assert_cmd` for binary invocation
- **Fixtures**: `tests/fixtures/*.jsonl` — representative session files (linear, branched, compaction, ANSI, desktop audit)
- Error type convention: `Result<T, String>` (no custom error type)

## Environment

- `CCFS_SEARCH_PATH` — override default search paths (see `lib.rs:get_search_paths()`)
- `CLAUDE_CONFIG_DIR` — override `~/.claude` as Claude config root (matches Claude Code's own env var)
- `CCS_POPUP_WIDTH` — override overlay popup width in launch-ccs.sh (default `90%`)
- `CCS_POPUP_HEIGHT` — override overlay popup height in launch-ccs.sh (default `90%`)

## Release

Uses **cargo-dist** for multi-platform builds triggered by version tags. Homebrew tap at `materkey/homebrew-ccs`. Manual `cargo publish` for crates.io.
