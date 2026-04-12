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
├── update.rs         # Self-update: GitHub release download, Homebrew detection, version comparison
├── session/
│   ├── mod.rs        # SessionSource enum (ClaudeCodeCLI | ClaudeDesktop), shared field extractors, resolve_parent_session, extract_message_content
│   └── record.rs     # SessionRecord enum (Message, Summary, CustomTitle, etc.), ContentBlock, ContentMode, MessageRole, parse_content_blocks(), render_text_content() — unified JSONL parsing and content rendering
├── dag/
│   └── mod.rs        # SessionDag — unified DAG engine: TipStrategy (LastAppended | MaxTimestamp), DisplayFilter, chain_from(), from_file()
├── search/
│   ├── mod.rs        # Module re-exports for search API surface
│   ├── ripgrep.rs    # Spawns `rg --json`, parses matches, post-filters content, extracts project names
│   ├── message.rs    # Parses JSONL lines into Message structs, delegates content extraction to SessionRecord
│   └── group.rs      # Groups RipgrepMatch by session_id, sorts by timestamp
├── tree/
│   └── mod.rs        # DagNode/TreeRow/SessionTree rendering model, uses SessionDag for chain building and SessionRecord for content
├── resume/
│   ├── mod.rs        # Resume orchestration (CLI exec vs Desktop open, resume_child for overlay)
│   ├── path_codec.rs # Encodes/decodes filesystem paths to Claude's dash-separated format
│   ├── fork.rs       # Creates forked JSONL files for branch-aware resume, uses SessionDag for chain logic
│   └── launcher.rs   # Process exec (Unix) / spawn (Windows), resume_cli_child() for overlay, safe path fallback (decode→parent→$HOME→/tmp), atomic session index
└── tui/
    ├── state.rs      # App struct (with InputState, SearchState, TreeState sub-structs), ResumeTarget, AppOutcome, BackgroundSearchResult, debounced async search
    ├── dispatch.rs   # KeyAction enum, KeyContext struct, classify_key() — maps key events to semantic actions
    ├── view.rs       # AppView<'a> — read-only projection of App for pure rendering
    ├── search_mode.rs# Search navigation, filtering, input handling, recent sessions navigation
    ├── tree_mode.rs  # Tree mode enter/exit, DAG navigation
    ├── render_search.rs # Search results + preview rendering (takes &AppView, not &mut App)
    └── render_tree.rs   # Tree DAG rendering with graph symbols (takes &AppView, not &mut App)

.claude-plugin/
├── plugin.json                         # Claude Code plugin manifest (name, version, skills path)
└── skills/ccs/
    ├── SKILL.md                        # Skill definition with CLI and overlay picker modes
    └── scripts/launch-ccs.sh           # Shell launcher: tmux/kitty/wezterm overlay for ccs pick
```

### Key data flow

1. **Search**: User types query → 300ms debounce → background thread spawns `rg --json --glob="*.jsonl"` → parse JSON output → parse each JSONL line into `Message` (delegates content extraction to `SessionRecord`) → **post-filter** to ensure query matches message *content* (not metadata) → group by `session_id` → sort by timestamp desc → if any file hit the per-file match limit (1000), flag `truncated` and show warning in status bar
2. **Tree mode**: Load full JSONL file → `SessionDag::from_file(path, Standard)` builds DAG in single pass (parses via `SessionRecord::from_jsonl`, filters sidechains, bridges `compact_boundary` via `logicalParentUuid`) → `dag.tip(MaxTimestamp)` picks latest terminal → `dag.chain_from(tip)` walks backward → mark branch points (nodes with >1 child) → flatten to `TreeRow` list. Content rendered via `SessionRecord::render_content(blocks, Preview { max_chars: 120 })`
3. **Resume**: On Enter, find `claude` binary via `which` → resolve project working directory from session path (only use decoded path if it exists on disk; fall back to session file parent → `$HOME` → `/tmp`) → if selected message is NOT on latest chain, create a forked JSONL file: `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from(tip)` determine the chain, then write filtered records → exec/spawn `claude --resume <file-path>` (absolute `.jsonl` path for cross-project support). Session index uses per-process temp files for atomic writes.
4. **TUI lifecycle**: `main()` calls `run_tui()` → key events go through `classify_key()` → `KeyAction` enum → `App::handle_action()`. Returns `TuiOutcome` (Quit, Resume, Pick). In overlay mode (`--overlay`), `Resume` spawns Claude as child via `resume_cli_child()` and loops back. In normal mode, `Resume` calls `resume()` (exec, replaces process). `Pick` writes `PickedSession` key-value output and exits 0/1. Rendering uses `AppView` (read-only projection) — no mutation during draw.
5. **Recent sessions**: App starts → background thread walks search dirs for `*.jsonl` (skip `agent-*` and `subagents/`) → sort by mtime → take top 100 → rayon parallel: `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from(tip)` for latest chain, then scan JSONL using `SessionRecord` match arms for title extraction (priority: agentName > customTitle > aiTitle > summary > lastPrompt > firstUserMessage) → deduplicate by session_id (keep newest) → sort by timestamp desc → send via mpsc to TUI → render in empty-state view

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
