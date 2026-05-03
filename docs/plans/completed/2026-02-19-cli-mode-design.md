# CLI Mode for Skill Integration

## Goal

Add non-interactive CLI mode to `claude-code-fullsearch` so it can be wrapped as a Claude Code skill for searching sessions programmatically.

## Commands

### `search <query> [--regex] [--limit N]`

Full-text search across Claude sessions. Output: JSONL (one JSON object per matching message).

```json
{"session_id":"abc123","project":"myapp","source":"CLI","file_path":"/path/to/session.jsonl","timestamp":"2025-01-09T10:00:00Z","role":"user","content":"Hello Claude"}
```

Fields: `session_id`, `project`, `source` (CLI/Desktop), `file_path`, `timestamp`, `role`, `content`.

Default limit: 100 matches. `--regex` enables regex search mode.

### `list [--limit N]`

List all sessions with metadata. Output: JSONL (one JSON object per session).

```json
{"session_id":"abc123","project":"myapp","source":"CLI","file_path":"/path/to/session.jsonl","last_active":"2025-01-09T10:05:00Z","message_count":42}
```

Default limit: 50 sessions. Sorted by last_active descending.

## CLI Activation

```bash
claude-search search "fix bug"        # CLI mode
claude-search search "fix bug" --regex # CLI mode with regex
claude-search list                     # CLI mode
claude-search list --limit 10          # CLI mode with limit
claude-search                          # TUI mode (unchanged)
claude-search --tree <id>              # TUI tree mode (unchanged)
```

## Architecture

- `src/main.rs` — match on `args[1]` for subcommands, dispatch to CLI or TUI
- `src/cli.rs` — new module with `cli_search()` and `cli_list()` functions
- Reuses existing `search_multiple_paths`, `group_by_session`, `extract_project_from_path`
- No new dependencies

## Implementation Approach

Hand-rolled arg parsing (consistent with existing code). No clap.
