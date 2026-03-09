# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**ccfullsearch** (`ccs`) — a TUI and CLI tool for searching and browsing Claude Code CLI and Claude Desktop session history. Built in Rust with ratatui (TUI), ripgrep (search), crossterm (terminal), and clap (CLI parsing). Requires `rg` (ripgrep) in PATH at runtime.

## Build & Development Commands

```bash
cargo build                    # Debug build
cargo build --release          # Release build
cargo run                      # Run TUI mode
cargo run -- search "query"    # CLI search subcommand
cargo run -- list              # List all sessions

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
├── main.rs           # CLI parsing (clap), panic handler, TUI event loop
├── lib.rs            # Module re-exports + get_search_paths()
├── cli.rs            # Non-interactive subcommands (search, list)
├── session.rs        # SessionSource enum (ClaudeCodeCLI | ClaudeDesktop), shared field extractors
├── search/
│   ├── ripgrep.rs    # Spawns `rg --json`, parses matches, post-filters content, extracts project names
│   ├── message.rs    # Parses JSONL lines into Message structs, extracts content from block arrays
│   └── group.rs      # Groups RipgrepMatch by session_id, sorts by timestamp
├── tree/
│   └── mod.rs        # Builds session DAG from uuid/parentUuid, detects branches and latest chain
├── resume/
│   ├── mod.rs        # Resume orchestration (CLI exec vs Desktop open)
│   ├── path_codec.rs # Encodes/decodes filesystem paths to Claude's dash-separated format
│   ├── fork.rs       # Creates forked JSONL files for branch-aware resume
│   └── launcher.rs   # Process exec (Unix) / spawn (Windows)
└── tui/
    ├── state.rs      # App struct, debounced async search (300ms), MPSC channels
    ├── search_mode.rs# Search navigation, filtering, input handling
    ├── tree_mode.rs  # Tree mode enter/exit, DAG navigation
    ├── render_search.rs # Search results + preview rendering
    └── render_tree.rs   # Tree DAG rendering with graph symbols
```

### Key data flow

1. **Search**: User types query → 300ms debounce → background thread spawns `rg --json --glob="*.jsonl"` → parse JSON output → parse each JSONL line into `Message` → **post-filter** to ensure query matches message *content* (not metadata) → group by `session_id` → sort by timestamp desc
2. **Tree mode**: Load full JSONL file → build DAG from `uuid`/`parentUuid` links → detect latest chain (walk backward from last uuid) → mark branch points (nodes with >1 child) → flatten to `TreeRow` list
3. **Resume**: On Enter, find `claude` binary via `which` → if selected message is NOT on latest chain, create a forked JSONL file (trace branch to root, write subset) → exec/spawn `claude --resume <session-id>`

### Dual format support

The tool handles two session formats with different field names:
- **Claude Code CLI** (`~/.claude/projects/`): `sessionId`, `timestamp`, has `branch`/`gitBranch`
- **Claude Desktop** (`~/Library/Application Support/Claude/local-agent-mode-sessions/`): `session_id`, `_audit_timestamp`, no branch info

The `SessionSource` enum in `session.rs` drives format-specific parsing throughout.

## Testing

- **Unit tests**: Inline `#[cfg(test)]` modules in source files
- **Integration tests**: `tests/` directory using `assert_cmd` for binary invocation
- **Fixtures**: `tests/fixtures/*.jsonl` — representative session files (linear, branched, compaction, ANSI, desktop audit)
- Error type convention: `Result<T, String>` (no custom error type)

## Environment

- `CCFS_SEARCH_PATH` — override default search paths (see `lib.rs:get_search_paths()`)

## Release

Uses **cargo-dist** for multi-platform builds triggered by version tags. Homebrew tap at `materkey/homebrew-ccs`. Manual `cargo publish` for crates.io.
