# ccs Use Cases

Practical scenarios showing how `ccs` fits into daily development workflows. Each use case starts with a real problem and shows the fastest path to a solution.

## Quick Reference

| I want to... | Command / Action |
|---|---|
| Browse recent sessions | `ccs` (launch TUI) |
| Search past conversations | `ccs search "query"` or type in TUI |
| Resume a session | `Enter` on any result |
| See conversation branches | `Ctrl+B` in TUI |
| Resume from a specific branch | Select message in tree view, `Enter` |
| Filter by current project | `Ctrl+A` in TUI |
| Hide automated sessions | `Ctrl+H` in TUI |
| Use regex patterns | `Ctrl+R` in TUI or `--regex` in CLI |
| Pick a session for scripting | `ccs pick` |
| Search from inside Claude Code | `/ccs` or "find in my sessions" |

---

## Finding Past Conversations

### "What did I decide about X?"

You discussed an architecture approach last week but can't remember the details.

**TUI**: Launch `ccs`, start typing your query. Results appear after 300ms, grouped by session with timestamps and project context. Navigate with arrow keys, press `Tab` to expand the match list within a session.

**CLI**: Get structured output for further processing:

```bash
ccs search "dependency injection" --limit 5
```

### "Where did I fix that bug?"

You solved a similar issue before and want to reuse the approach.

```bash
ccs search "connection pool timeout" --regex --limit 10
```

In the TUI, press `Enter` on any result to resume that session and continue where you left off.

### "What sessions touched this file?"

Find all conversations where a specific file was read or edited. Tool use inputs (Read, Edit, Write) are indexed, so file paths are searchable.

```bash
ccs search "src/auth/middleware.rs"
```

### "Show me sessions from this project only"

Press `Ctrl+A` in the TUI to toggle the project filter. When active, only sessions from the current working directory's project are shown — both in the recent sessions list and search results.

### "Hide automated agent runs"

Press `Ctrl+H` to cycle through automation filters: **All** -> **Manual** -> **Auto**. This separates interactive sessions from automated agent runs (sessions started by patrol, cron, or other automation tools).

### "Complex search patterns"

Toggle regex mode with `Ctrl+R` in the TUI, or use `--regex` in CLI:

```bash
ccs search "error.*timeout|timeout.*error" --regex
```

---

## Browsing Recent Sessions

### See what you worked on recently

Launch `ccs` with no arguments. The startup screen shows your most recent sessions (up to 100), sorted by last activity. Each entry displays:

- Session summary (extracted from agent name, custom title, AI-generated title, or first user message)
- Project name
- Relative timestamp

Navigate with `Up`/`Down`, press `Enter` to resume, or start typing to switch to search mode.

### Return to recent sessions after searching

Clear the search input with `Ctrl+C` (when input is non-empty) to return to the recent sessions list. Your cursor position is preserved.

---

## Resuming Work

### Pick up where you left off

Navigate to any session in the recent list or search results and press `Enter`. For Claude Code CLI sessions, this runs `claude --resume <session>`. For Claude Desktop sessions, it opens the Claude Desktop app.

### Resume from a specific branch point

Conversations with Claude can branch when you retry, edit, or fork. The tree view shows the full DAG:

1. Select a session and press `Ctrl+B` to open tree view
2. See the conversation structure: messages on the latest chain, branch points (marked with graph symbols), and context compaction boundaries
3. Use `Left`/`Right` to jump between branch points, `Tab` to toggle the preview pane
4. Select a specific message and press `Enter`

If the selected message is **not** on the latest chain, `ccs` automatically creates a forked JSONL file — extracting only the branch from root to your selected point — and resumes from that fork. This is branch-aware resume that Claude Code's own `--resume` flag doesn't support.

### Open tree view directly for a session

```bash
ccs --tree <session-id-or-path>
```

Opens the tree view immediately, skipping the search screen. Useful when you already know which session to inspect.

### Resume in overlay mode

In tmux, kitty, or wezterm, `ccs --overlay` runs as a session picker loop. After selecting a session and resuming, Claude runs as a child process. When you exit Claude, you return to the `ccs` picker to select another session — no need to relaunch.

```bash
ccs --overlay
```

The search query is preserved between resume cycles, so you can keep working through related sessions.

---

## Previewing Content

### Peek at a message before resuming

Press `Tab` (or `Ctrl+V`) on a search result or tree view message to toggle the preview pane. The preview shows the full message content alongside the result list. Press `Tab` again or `Esc` to close the preview.

Note: preview is available in search results (when a match is selected) and in tree view, but not on the recent sessions list.

---

## Claude Code Plugin

### Session search from inside Claude

With the plugin installed (auto-discovered from `.claude-plugin/`), use `/ccs` or say "find in my sessions" to launch the overlay picker directly from a Claude Code conversation.

After picking a session, you get two options:

- **Read here** — the session JSONL is read and summarized in the current conversation, so you can ask follow-up questions without leaving
- **Enter session** — resumes the selected session in a terminal overlay popup (tmux/kitty/wezterm)

### Pre-fill search from Claude

```
/ccs docker build errors
```

Opens the picker with "docker build errors" pre-filled as the search query.

---

## Scripting and Automation

### Build a session picker into your workflow

The `pick` subcommand opens the TUI picker and outputs structured key-value data on selection:

```bash
ccs pick "deploy"
```

Output (exit code 0 on selection, 1 on cancel):

```
session_id: abc-123
file_path: /path/to/session.jsonl
source: CLI
project: my-project
message_uuid: def-456
```

Write output to a file instead of stdout:

```bash
ccs pick --output /tmp/session.txt
```

### Export search results as JSONL

Both `search` and `list` commands output one JSON object per line:

```bash
# Find all sessions mentioning OOM errors, filter to user messages
ccs search "OOM|OutOfMemory" --regex | jq 'select(.role == "user")'

# List recent sessions as a table
ccs list --limit 20 | jq -r '[.project, .session_id, .last_active] | @tsv'
```

### Direct branch-aware resume from scripts

Skip the TUI entirely and resume from a specific message UUID:

```bash
ccs --overlay --tree /path/to/session.jsonl --resume-uuid abc-123
```

This is used by the Claude Code plugin when the picker already captured a branch selection.

---

## Cross-Platform Support

### Claude Code CLI sessions

Default location: `~/.claude/projects/`. Override with `CLAUDE_CONFIG_DIR` environment variable.

### Claude Desktop sessions

Automatically detected at:
- **macOS**: `~/Library/Application Support/Claude/local-agent-mode-sessions/`
- **Linux**: `~/.config/Claude/local-agent-mode-sessions/`

Both formats are searched simultaneously. The `source` field in results indicates `CLI` or `Desktop`.

### Custom search paths

Override all default paths with a single environment variable:

```bash
CCFS_SEARCH_PATH=/custom/sessions/dir ccs
```

---

## Self-Update

### Update to the latest release

```bash
ccs update
```

Downloads the latest release from GitHub, verifies the SHA-256 checksum, and replaces the binary with rollback on failure. If installed via Homebrew, it tells you to use `brew upgrade ccs` instead.

---

## Debugging

### Debug resume behavior

When resume opens the wrong session or fork logic seems off:

```bash
CCS_DEBUG=1 ccs 2>/tmp/ccs-debug.log
```

The log traces the full chain: TUI selection -> `resolve_parent_session` (handles subagent files, mismatched filenames) -> fork decision (is selected message on latest chain?) -> final `claude --resume` command.

### Overlay popup sizing

Control the overlay popup dimensions (for `launch-ccs.sh`):

```bash
CCS_POPUP_WIDTH=80% CCS_POPUP_HEIGHT=85% ccs --overlay
```

---

## Keybinding Summary

### Recent Sessions (empty input)

| Key | Action |
|---|---|
| `Up` / `Down` | Navigate sessions |
| `Enter` | Resume selected session |
| `Ctrl+B` | Open tree view |
| Type anything | Switch to search mode |

### Search Mode

| Key | Action |
|---|---|
| `Up` / `Down` | Navigate session groups |
| `Left` / `Right` | Move cursor in input / expand or collapse match list |
| `Tab` / `Ctrl+V` | Toggle preview pane |
| `Enter` | Resume selected session |
| `Ctrl+A` | Toggle project filter |
| `Ctrl+H` | Cycle automation filter (All / Manual / Auto) |
| `Ctrl+R` | Toggle regex search |
| `Ctrl+B` | Open tree view for selected session |
| `Ctrl+C` | Clear input (or quit if empty) |
| `Ctrl+W` | Delete word left |
| `Alt+Left` / `Alt+Right` | Move cursor by word |
| `Home` | Move cursor to start |
| `End` / `Ctrl+E` | Move cursor to end |
| `Delete` | Delete character at cursor |
| `Alt+Backspace` | Delete word left |
| `Alt+D` | Delete word right |
| `Ctrl+Left` / `Ctrl+Right` | Move cursor by word (alternative) |
| `Esc` | Quit (or close preview) |

### Tree Mode

| Key | Action |
|---|---|
| `Up` / `Down` | Navigate messages |
| `Left` / `Right` | Jump to previous/next branch point |
| `Tab` | Toggle preview pane |
| `Enter` | Resume from selected message |
| `Esc` / `b` / `Ctrl+C` | Back to search |
| `q` | Quit |
