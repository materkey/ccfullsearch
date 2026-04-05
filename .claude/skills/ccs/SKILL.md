---
name: ccs
description: Search across all Claude Code and Claude Desktop sessions. Find past conversations by content, list recent sessions, locate specific discussions. Use when user asks to find something in previous sessions, recall past conversations, or list Claude sessions.
argument-hint: 'optional: search query'
allowed-tools: [Bash, Read, Grep, Glob]
---

# Claude Session Search

Full-text search across Claude Code CLI and Claude Desktop sessions using `ccs`.

## Prerequisites

Binary `ccs` must be in PATH (installed via `cargo install ccfullsearch` or Homebrew `brew install materkey/ccs/ccs`).

## Activation Triggers

- "find in my sessions" / "search sessions"
- "what session did I discuss X"
- "list my sessions" / "show recent sessions"
- "recall past conversation about X"
- "find where we talked about X"
- "pick a session" / "choose session"

## Mode Selection

Choose the mode based on context:

### Overlay Picker Mode (preferred in interactive terminals)

Use when the user wants to visually browse and pick a session. Launches `ccs` TUI in a terminal overlay popup.

```bash
${CLAUDE_PLUGIN_ROOT}/.claude/skills/ccs/scripts/launch-ccs.sh [query]
```

The script:
- Detects available terminal (tmux -> kitty -> wezterm -> fallback)
- Launches `ccs pick` in an overlay popup
- Captures selection output (key-value format)
- Prints to stdout; empty if cancelled

Output format on selection:
```
session_id: <uuid>
file_path: <absolute path to .jsonl>
source: CLI|Desktop
project: <project name>
message_uuid: <uuid>       # present for search results and tree view selections
```

Empty output (exit 1) means user cancelled.

#### After Selection

If output is non-empty, parse the key-value pairs. Use AskUserQuestion to offer the user a choice:

**Option 1: "Read here"** — read the session JSONL file in the current conversation context:
```bash
# Read the session file and summarize the conversation
```
Use the Read tool on `file_path` from the picker output. Parse the JSONL and present a summary of the conversation (participants, topics, key decisions). The user can then ask follow-up questions about the session content without leaving the current conversation.

**Option 2: "Enter session (overlay)"** — resume the session in a terminal overlay:
```bash
# Determine project_dir from file_path:
# .claude/projects/-Users-foo-myproject/session.jsonl → /Users/foo/myproject
# (replace leading dash with /, then convert all dashes-between-path-segments to /)
```
Then launch in overlay:
```bash
${SKILL_DIR}/scripts/launch-resume.sh <session_id> --cwd <project_dir>
```
This opens `claude --resume` in a tmux popup / kitty overlay / wezterm split-pane. Blocks until claude exits, then returns control here.

For **Desktop** sessions (`source: Desktop`): overlay resume is not available, only "Read here" and `open -a Claude` are options.

### CLI Mode (for scripted/non-interactive use)

#### Search sessions by content

```bash
ccs search "<query>" [--regex] [--limit N]
```

Default limit: 100 matches. Add `--regex` for regex patterns.

#### List all sessions

```bash
ccs list [--limit N]
```

Default limit: 50 sessions. Sorted by last activity (newest first).

#### Pick a session (non-interactive output)

```bash
ccs pick [query] [--output=/path/to/file]
```

Opens TUI picker, outputs selection in key-value format. Exit 0 on selection, exit 1 on cancel.

### Interactive TUI

```bash
ccs
```

Launches full interactive TUI with search, navigation, tree view, and session resume.

## Output Format (CLI search/list)

Both `search` and `list` commands output JSONL (one JSON object per line).

### Search output fields

| Field | Description |
|-------|-------------|
| `session_id` | UUID of the session |
| `project` | Project name extracted from path |
| `source` | `CLI` or `Desktop` |
| `file_path` | Full path to the .jsonl session file |
| `timestamp` | ISO 8601 timestamp of the message |
| `role` | `user` or `assistant` |
| `content` | Message text content |

### List output fields

| Field | Description |
|-------|-------------|
| `session_id` | UUID of the session |
| `project` | Project name |
| `source` | `CLI` or `Desktop` |
| `file_path` | Full path to the .jsonl session file |
| `last_active` | ISO 8601 timestamp of last message |
| `message_count` | Total number of messages |

## Usage Patterns

### Find sessions where a topic was discussed

```bash
ccs search "docker build" --limit 10
```

### Find sessions with regex

```bash
ccs search "OOM|OutOfMemory" --regex --limit 20
```

### List recent sessions

```bash
ccs list --limit 10
```

### Resume a found session

After finding a session_id from search/list, resume it:
```bash
claude --resume <session_id>
```

## Tips

- Search is case-insensitive by default
- Results are grouped by session, sorted by most recent first
- Content includes both user messages and assistant responses
- Tool use inputs and results are also searchable
- The `project` field helps identify which project a session belongs to
- Use `jq` to filter JSONL output: `ccs search "error" | jq 'select(.role == "user")'`
