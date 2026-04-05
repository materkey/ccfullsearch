# ccs Use Cases

Practical scenarios showing how `ccs` fits into daily development workflows.

## Finding Past Conversations

### "What did I decide about X?"

You discussed an architecture approach last week but can't remember the details.

```bash
ccs search "dependency injection" --limit 5
```

Or open the TUI and type your query — results are grouped by session with timestamps, so you can quickly spot the right conversation.

### "Where did I fix that bug?"

You solved a similar issue before and want to reuse the approach.

```bash
ccs search "connection pool timeout" --regex --limit 10
```

Press `Enter` on the result to resume that session and continue where you left off.

### "What sessions touched this file?"

Find all conversations where a specific file was read or edited.

```bash
ccs search "src/auth/middleware.rs"
```

Tool use inputs (Read, Edit, Write) are indexed, so file paths are searchable.

## Resuming Work

### Pick up where you left off

Launch `ccs` with no arguments to see recent sessions. Navigate with arrow keys, press `Enter` to resume.

```bash
ccs
```

### Resume from a specific branch point

Open tree view (`Ctrl+B`) on any session to see its conversation DAG — branches, forks, and compaction points. Select a specific message and press `Enter` to resume from that exact point.

### Resume in overlay mode

In tmux/kitty/wezterm, `ccs --overlay` opens as a popup. After selecting a session and resuming, Claude runs as a child process. When you exit Claude, you're back in the ccs picker.

## Scripting and Automation

### Build a session picker into your workflow

```bash
# Pick a session interactively, capture the result
eval "$(ccs pick "deploy" | sed 's/: /=/')"
echo "Selected session: $session_id from project: $project"
```

### Export search results as JSONL

```bash
# Find all sessions mentioning OOM errors, pipe to jq
ccs search "OOM|OutOfMemory" --regex | jq 'select(.role == "assistant")'
```

### List recent sessions for a report

```bash
ccs list --limit 20 | jq -r '[.project, .session_id, .last_active] | @tsv'
```

## Claude Code Plugin

### Session search from inside Claude

With the plugin installed, use `/ccs` or say "find in my sessions" to launch the overlay picker directly from a Claude Code conversation.

After picking a session:
- **Read here** — summarize the session content in the current conversation
- **Enter session** — resume the session in a terminal overlay

### Pre-fill search from Claude

```
/ccs docker build errors
```

Opens the picker with "docker build errors" pre-filled as the search query.

## Filtering

### Filter by project

Press `Ctrl+A` to toggle project filter — shows only sessions from the current project directory.

### Filter by automation

Press `Ctrl+H` to cycle through: All → Manual → Auto. Useful for separating interactive sessions from automated agent runs.

### Regex patterns

Toggle regex mode with `Ctrl+R` for complex queries:

```
ccs search "error.*timeout|timeout.*error" --regex
```

## Debugging with CCS_DEBUG

When resume opens the wrong session, enable debug logging:

```bash
CCS_DEBUG=1 ccs 2>/tmp/ccs-debug.log
```

The log shows the full resume chain: TUI selection → resolve → fork decision → final `claude --resume` command.
