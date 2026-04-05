---
name: claude-session-search
description: Search across all Claude Code and Claude Desktop sessions. Find past conversations by content, list recent sessions, locate specific discussions. Use when user asks to find something in previous sessions, recall past conversations, or list Claude sessions.
---

> **Note:** This skill is also available as a Claude Code plugin with overlay picker support.
> See `.claude-plugin/` for the plugin version with `ccs pick` overlay mode.

# Claude Session Search

Full-text search across Claude Code CLI and Claude Desktop sessions using `ccs`.

## Prerequisites

Binary `ccs` must be in PATH (installed via `cargo install --path` from the project).

## Commands

### Search sessions by content

```bash
ccs search "<query>" [--regex] [--limit N]
```

Default limit: 100 matches. Add `--regex` for regex patterns.

### List all sessions

```bash
ccs list [--limit N]
```

Default limit: 50 sessions. Sorted by last activity (newest first).

### Interactive TUI

```bash
ccs
```

Launches full interactive TUI with search, navigation, tree view, and session resume.

## Output Format (CLI mode)

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

## Trigger Patterns

Use this skill when user asks:
- "find in my sessions" / "search sessions"
- "what session did I discuss X"
- "list my sessions" / "show recent sessions"
- "recall past conversation about X"
- "find where we talked about X"

## Tips

- Search is case-insensitive by default
- Results are grouped by session, sorted by most recent first
- Content includes both user messages and assistant responses
- Tool use inputs and results are also searchable
- The `project` field helps identify which project a session belongs to
- Use `jq` to filter JSONL output: `ccs search "error" | jq 'select(.role == "user")'`
