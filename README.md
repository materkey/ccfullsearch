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

Or build from source:

```bash
cargo build --release
# Binary will be at target/release/ccs
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
| `b` / `Esc` | Back to search |
| `q` | Quit |

## How it works

1. Searches JSONL session files using `ripgrep` for speed
2. Parses matched lines as Claude session messages (user, assistant, tool calls)
3. Groups results by session with metadata (project name, timestamps)
4. Tree view parses the full session DAG to show conversation branches and the latest chain
