# ccs

A TUI and CLI tool for searching and browsing Claude Code and Claude Desktop session history.

Built with Rust using [ratatui](https://github.com/ratatui/ratatui) and [ripgrep](https://github.com/BurntSushi/ripgrep).

![demo](demo.gif)

## Features

- **Full-text search** across all Claude Code CLI and Claude Desktop sessions
- **Regex search** mode (toggle with `Ctrl+R`)
- **Session grouping** — results grouped by session with timestamps and project context
- **Tree view** — visualize conversation branches, forks, and context compactions (`Ctrl+B`)
- **Session resume** — press `Enter` to resume any session directly from search results
- **Async search** — non-blocking background search with debounce
- **CLI mode** — `search` and `list` subcommands with JSONL output for scripting
- **Cross-platform** — supports both Claude Code CLI (`~/.claude/projects`) and Claude Desktop sessions

## Requirements

- [ripgrep](https://github.com/BurntSushi/ripgrep) (`rg`) must be installed and available in `PATH`
- Rust 1.70+

## Installation

```bash
cargo install --path .
```

This installs the `ccs` binary to `~/.cargo/bin/`. Make sure it's in your `PATH`:

```bash
# fish shell (one-time, persists automatically)
fish_add_path ~/.cargo/bin

# bash/zsh (add to ~/.bashrc or ~/.zshrc)
export PATH="$HOME/.cargo/bin:$PATH"
```

Or build manually:

```bash
cargo build --release
# Binary will be at target/release/ccs
```

## Architecture

```
src/
├── lib.rs              # Crate root — re-exports all modules
├── main.rs             # CLI entry point (clap) + TUI event loop
├── cli.rs              # Non-interactive search/list commands (JSONL output)
├── session.rs          # Shared JSONL parsing primitives (timestamps, UUIDs, sources)
├── search/             # Ripgrep integration, message parsing, result grouping
├── tree/               # Session DAG parsing, branch detection, flattened tree rows
├── resume/
│   ├── path_codec.rs   # Claude path encoding/decoding + filesystem walking
│   ├── fork.rs         # Branch chain detection + fork file creation
│   └── launcher.rs     # Process launching (exec on Unix, spawn on Windows)
└── tui/
    ├── state.rs         # App struct, constructor, input/cursor methods, tick loop
    ├── search_mode.rs   # Search navigation, enter, toggle, search orchestration
    ├── tree_mode.rs     # Tree mode enter/exit, navigation, file lookup
    ├── render_search.rs # Search & preview rendering
    └── render_tree.rs   # Tree mode rendering

tests/
├── fixtures/           # Representative JSONL session files
├── cli_search.rs       # Integration tests for `ccs search`
├── cli_list.rs         # Integration tests for `ccs list`
├── tree_parsing.rs     # Tree parsing via library API
└── render_snapshots.rs # TUI render state verification
```

## Testing

```bash
# Run all tests
cargo test

# Quality checks (same as CI)
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Usage

### Interactive TUI

```bash
# Launch interactive search
ccs

# Open tree view for a specific session
ccs --tree <session-id-or-path>
```

### CLI mode

```bash
# Search sessions (outputs JSONL)
ccs search "docker build" --limit 10

# Search with regex
ccs search "OOM|OutOfMemory" --regex

# List all sessions sorted by last activity
ccs list --limit 20
```

## Keybindings

### Search mode

| Key | Action |
|-----|--------|
| Type | Search query input |
| `Up` / `Down` | Navigate session groups |
| `Left` / `Right` | Navigate matches within a group |
| `Tab` | Expand/collapse match list |
| `Enter` | Resume selected session |
| `Ctrl+A` | Toggle project filter (current project / all sessions) |
| `Ctrl+V` | Toggle preview (same as Tab) |
| `Ctrl+C` | Clear input (or quit if input is empty) |
| `Ctrl+R` | Toggle regex search mode |
| `Ctrl+B` | Open tree view for selected session |
| `Esc` | Quit |

### Tree mode

| Key | Action |
|-----|--------|
| `Up` / `Down` | Navigate messages |
| `Left` / `Right` | Scroll content horizontally |
| `Tab` | Jump to next branch point |
| `Enter` | Resume session at selected message |
| `Ctrl+C` / `b` / `Esc` | Back to search |
| `q` | Quit |

## Claude Code Skill

The repo includes a [Claude Code skill](skill/SKILL.md) so Claude can search your sessions automatically. To install:

```bash
cp -r skill ~/.claude/skills/claude-session-search
```

Then Claude will use `ccs` when you ask things like "find where we discussed docker" or "list my recent sessions".

## How it works

1. Searches JSONL session files using `ripgrep` for speed
2. Parses matched lines as Claude session messages (user, assistant, tool calls)
3. Groups results by session with metadata (project name, timestamps)
4. Tree view parses the full session DAG to show conversation branches and the latest chain
