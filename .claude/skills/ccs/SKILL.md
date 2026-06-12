---
name: ccs
description: Search across all Claude Code, Claude Desktop, Codex, and Opencode sessions. Find past conversations by content, list recent sessions, locate specific discussions. Use when user asks to find something in previous sessions, recall past conversations, or list Claude sessions.
argument-hint: 'optional: search query'
allowed-tools: [Bash, Read, Grep, Glob]
---

# Claude Session Search

Full-text search across Claude Code CLI, Claude Desktop, Codex, and Opencode sessions using `ccs`.

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

The retrieval flow is **search → show → answer**.

#### Step 1 — search

```bash
ccs search "<query>" --limit 20 [--regex]
```

Output is JSONL: one `{"type":"match",...}` line per result, then always a final `{"type":"summary",...}` line.

Match fields:

| Field | Description |
|-------|-------------|
| `session_id` | Session ID |
| `project` | Project name extracted from path |
| `provider` | `Claude`, `Codex`, or `Opencode` |
| `source` | `CLI` or `Desktop` |
| `file_path` | Path to the session file (for Opencode: `<db>#<session_id>`) |
| `line_number` | 1-based line in the JSONL file; `null` for Opencode |
| `message_uuid` | Message UUID; for Opencode this is the SQLite message id |
| `timestamp` | ISO 8601 timestamp of the message |
| `role` | `user` or `assistant` |
| `content` | Snippet around the match (~200 chars each side) |

Summary line:

```json
{"type":"summary","shown":20,"total_matches":2721,"sessions":24,"truncated":true}
```

`shown: 0` means no matches. `truncated: true` means the result set is incomplete.

#### Step 2 — show (drill down)

The snippet identifies the session; to answer the user's question, read the surrounding messages:

```bash
ccs show <file_path> --line <line_number> [--context K] [--max-chars M]
```

For Opencode matches `line_number` is `null` — anchor by uuid instead:

```bash
ccs show <file_path> --uuid <message_uuid>
```

Output is JSONL: `{"type":"message",...}` rows in file order (the target carries `is_target: true`, truncated rows carry `content_truncated: true`), then a final `{"type":"summary",...}` with `session_id`, `provider`, `project`. Defaults: 3 messages before/after, 2000 chars per message — a few KB total.

Note: near a fork of a branched session the window can mix branches; `parent_uuid` on each row shows the actual chain.

#### Step 3 — answer

Cite `project` and `timestamp` from the match. Say if the result set was truncated.

#### Fallbacks

- `shown: 0` — try 2-3 query variants: a shorter phrase, a synonym, or `--regex` for patterns like `"OOM|OutOfMemory"`.
- `truncated: true` — narrow the query (more specific phrase) before raising `--limit`.
- Window too small — raise `--context` / `--max-chars`, or use the Read tool on `file_path` with `offset` near `line_number`.
- `--full-content` on search is a last resort: tool outputs can make single results hundreds of KB.

#### List all sessions

```bash
ccs list [--limit N]
```

Default limit: 50 sessions, sorted by last activity (newest first). Fields: `session_id`, `project`, `provider`, `source`, `file_path`, `last_active`, `message_count`.

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

## Usage Patterns

### Answer "what did we decide about X?"

```bash
ccs search "configuration cache" --limit 20
# pick the most relevant match, then:
ccs show /path/to/session.jsonl --line 381 --context 3
```

### Resume a found session

After finding a session_id from search/list, resume it:
```bash
claude --resume <session_id>
```

## Tips

- Search is case-insensitive by default
- Results are grouped by session, sorted by most recent first
- Tool use inputs and results are also searchable
- The `project` field helps identify which project a session belongs to
