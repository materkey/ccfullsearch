# ccs

`ccs` is a terminal UI and CLI for finding, browsing, and resuming Claude session history.

It searches Claude Code CLI projects and Claude Desktop local-agent sessions by default. Custom search roots can also include other compatible JSONL transcripts; results carry provider/source metadata such as `Claude`, `Codex`, `CLI`, and `Desktop`.

Built with Rust, [ratatui](https://github.com/ratatui/ratatui), and [ripgrep](https://github.com/BurntSushi/ripgrep).

![demo](demo.gif)

## Features

- Recent sessions on startup, with summaries from session metadata or the first user message.
- Fast full-text and regex search across JSONL session transcripts.
- Search and preview of user/assistant content, tool calls/results, thinking blocks, attachments, and rendered slash commands.
- Session grouping by transcript, with provider/source badges, project, branch, timestamp, match count, and message count.
- Filters for current project (`Ctrl+A`) and manual/automated sessions (`Ctrl+H`). Manual sessions are shown by default.
- Tree view (`Ctrl+B`) for conversation branches, forks, latest-chain markers, and compaction boundaries.
- Resume from selected search results or recent sessions, with branch-aware resume from a selected tree message.
- AI re-ranking (`Ctrl+G`) of visible sessions by natural-language relevance, using the `claude` CLI.
- Non-interactive `search` and `list` commands with JSONL output.
- Interactive `pick` command for scripts and Claude Code plugin integration.
- Overlay mode (`--overlay`) that resumes Claude as a child process and returns to the picker afterwards.
- Self-update command for non-Homebrew installs.

## Installation

### Homebrew

```bash
brew install materkey/ccs/ccs
```

### Shell Installer

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/materkey/ccfullsearch/releases/latest/download/ccfullsearch-installer.sh | sh
```

### Cargo

```bash
cargo install ccfullsearch --locked
```

### cargo-binstall

```bash
cargo binstall ccfullsearch
```

### Requirements

- `rg` from [ripgrep](https://github.com/BurntSushi/ripgrep) must be available in `PATH`. The Homebrew formula installs it automatically.
- `claude` must be available in `PATH` for Claude Code resume and AI re-ranking.

## Search Paths

By default, `ccs` searches:

- Claude Code CLI sessions under `~/.claude/projects/`.
- `CLAUDE_CONFIG_DIR/projects/` when `CLAUDE_CONFIG_DIR` is set.
- Claude Desktop local-agent sessions on macOS: `~/Library/Application Support/Claude/local-agent-mode-sessions/`.
- Claude Desktop local-agent sessions on Linux: `~/.config/Claude/local-agent-mode-sessions/`.

Override the defaults with one custom root:

```bash
CCFS_SEARCH_PATH=/custom/sessions/dir ccs
```

## Usage

### Interactive TUI

```bash
# Open recent sessions. Start typing to search.
ccs

# Open tree view directly for a session ID or JSONL file path.
ccs --tree <session-id-or-path>

# Resume from a specific message UUID without opening the picker.
ccs --overlay --tree /path/to/session.jsonl --resume-uuid <message-uuid>
```

### CLI

```bash
# Search sessions. Output is JSONL.
ccs search "docker build" --limit 10

# Search with regex.
ccs search "OOM|OutOfMemory" --regex --limit 20

# List sessions by last activity. Output is JSONL.
ccs list --limit 20

# Update a non-Homebrew installation.
ccs update
```

`search` output fields:

| Field | Description |
|---|---|
| `session_id` | Session UUID |
| `project` | Project name extracted from the transcript path |
| `provider` | Transcript owner, for example `Claude` or `Codex` |
| `source` | Session source, for example `CLI` or `Desktop` |
| `file_path` | Full path to the JSONL transcript |
| `timestamp` | Message timestamp in RFC 3339 format |
| `role` | Message role |
| `content` | Extracted searchable content |

`list` output fields:

| Field | Description |
|---|---|
| `session_id` | Session UUID |
| `project` | Project name |
| `provider` | Transcript owner |
| `source` | Session source |
| `file_path` | Full path to the JSONL transcript |
| `last_active` | Last message timestamp in RFC 3339 format |
| `message_count` | Number of parsed messages |

### Picker

`ccs pick` opens the TUI and prints the selected session as key-value output. It exits with code `0` on selection and `1` on cancel.

```bash
ccs pick
ccs pick "docker"
ccs pick --output /tmp/session.txt
```

Example output:

```text
session_id: abc-123
file_path: /path/to/session.jsonl
source: CLI
project: my-project
message_uuid: def-456
```

`message_uuid` is present when the selected row maps to a concrete message, including search results and tree selections.

### Overlay Mode

```bash
ccs --overlay
```

In overlay mode, selecting a Claude Code session launches `claude --resume` as a child process. When Claude exits, `ccs` returns to the picker and restores the current search query.

## Keybindings

### Recent Sessions

| Key | Action |
|---|---|
| `Up` / `Down` | Navigate sessions |
| `Enter` | Resume selected session |
| `Ctrl+B` | Open tree view |
| `Ctrl+A` | Toggle current-project filter |
| `Ctrl+H` | Cycle automation filter: Manual / Auto / All |
| `Ctrl+G` | Enter AI re-ranking mode |
| Type | Start searching |

### Search

| Key | Action |
|---|---|
| Type | Edit search query |
| `Up` / `Down` | Navigate session groups |
| `Left` / `Right` | Move cursor, or expand/collapse match list when navigating results |
| `Tab` / `Ctrl+V` | Toggle preview |
| `Enter` | Resume selected session |
| `Ctrl+A` | Toggle current-project filter |
| `Ctrl+H` | Cycle automation filter: Manual / Auto / All |
| `Ctrl+R` | Toggle regex mode |
| `Ctrl+B` | Open tree view |
| `Ctrl+G` | Enter AI re-ranking mode |
| `Ctrl+C` | Clear query, or quit when query is empty |
| `Esc` | Close preview, or quit |

Text editing also supports `Home`, `End` / `Ctrl+E`, `Delete`, `Ctrl+W`, `Alt+Backspace`, `Alt+D`, `Alt+B` / `Alt+Left`, `Alt+F` / `Alt+Right`, and `Ctrl+Left` / `Ctrl+Right`.

### AI Mode

| Key | Action |
|---|---|
| Type | Edit AI query |
| `Enter` | Rank visible sessions, or resume the selected ranked session after ranking |
| `Up` / `Down` | Navigate ranked sessions |
| `Ctrl+C` | Clear AI query, or exit AI mode when the AI query is empty |
| `Esc` / `Ctrl+G` | Exit AI mode and restore the previous order |

Editing the AI query or toggling `Ctrl+R`, `Ctrl+A`, or `Ctrl+H` invalidates the applied rank, so the next `Enter` ranks again. If Claude returns no relevant sessions, AI mode stays open for query refinement.

### Tree

| Key | Action |
|---|---|
| `Up` / `Down` | Navigate messages |
| `Left` / `Right` | Jump to previous/next branch point |
| `Tab` | Toggle preview |
| `Enter` | Resume at selected message |
| `Ctrl+C` / `b` / `Esc` | Back to search |
| `q` | Quit |

## Claude Code Plugin

The repo includes a Claude Code plugin under `.claude-plugin/` and the `ccs` skill under `.claude/skills/ccs/`.

The skill supports:

- CLI mode through `ccs search` and `ccs list`.
- Overlay picker mode through `.claude/skills/ccs/scripts/launch-ccs.sh`.
- Overlay resume through `.claude/skills/ccs/scripts/launch-resume.sh`.

With the plugin installed, Claude Code can use `ccs` for requests such as "find where we discussed docker", "list my recent sessions", or "resume a previous conversation".

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Useful docs:

- [Use cases](docs/use-cases.md)
- [Changelog](CHANGELOG.md)

## Release

1. Bump `version` in `Cargo.toml`.
2. Update `CHANGELOG.md`.
3. Commit and push to `main`.
4. Publish to crates.io: `cargo publish`.
5. Tag and push: `git tag v<VERSION> && git push origin v<VERSION>`.

The tag push triggers cargo-dist, which builds macOS/Linux archives, shell installers, checksums, a GitHub Release, and the Homebrew formula in `materkey/homebrew-ccs`.
