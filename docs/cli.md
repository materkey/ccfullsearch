# CLI reference

## `ccs search`

```bash
ccs search "docker build" --limit 10
ccs search "OOM|OutOfMemory" --regex --limit 20
```

`ccs search` writes JSONL, one `{"type":"match",...}` object per matching message. `--limit` defaults to 100.

Fields:

- `type`: always `match` on result rows.
- `host`: `local`, or the remote host the session lives on (see [Remote hosts](remote-hosts.md)).
- `session_id`: session UUID or provider-specific session ID.
- `project`: project name extracted from the transcript path or metadata.
- `provider`: transcript owner, for example `Claude` or `Codex`.
- `source`: session source, for example `CLI` or `Desktop`.
- `file_path`: full path to the JSONL transcript (for Opencode: `<db>#<session_id>`).
- `line_number`: 1-based line of the message in the JSONL transcript; `null` for Opencode, whose messages live in SQLite.
- `message_uuid`: message UUID when the transcript has one; for Opencode this is the SQLite message id.
- `timestamp`: message timestamp in RFC 3339 format.
- `role`: message role.
- `content`: snippet around the match (about 200 characters of context on each side). Pass `--full-content` to output the entire message text instead.

The last line is always a summary record, even with zero matches, so consumers can tell whether the result set is complete:

```json
{"type":"summary","shown":100,"total_matches":2721,"sessions":24,"truncated":true,"hosts":[{"name":"local","status":"ok","shown":100}]}
```

- `shown`: number of result lines emitted.
- `total_matches`: matches found before applying `--limit`.
- `sessions`: distinct sessions among the shown results.
- `truncated`: `true` when results are incomplete (the limit cut them off or a per-file match cap was hit).
- `hosts`: how each host answered: `name`, `status` (`ok`, `down`, `incompatible`, `timeout` or `error`), `shown`, and `error` with the reason when there is one.

Example:

```bash
ccs search "connection pool timeout" --regex --limit 10
```

## `ccs show`

`ccs show` prints the messages around a search hit — the drill-down step after `ccs search`. Pass `file_path` plus `--line` (the `line_number` of a match) or `--uuid` (its `message_uuid`; the only valid anchor for Opencode):

```bash
ccs show /path/to/session.jsonl --line 381 --context 3 --max-chars 2000
ccs show "/path/to/opencode.db#ses_abc" --uuid msg_123
```

Without `--line` and `--uuid` it prints the end of the session: the last `--tail` messages (default 10) of its current line — the latest branch of a Claude session. The target can also be a session id instead of a path; for a Codex thread that is its current rollout file:

```bash
ccs show 01a178da-2e05-7720-b6ba-6b4edd17fc27 --tail 10
```

Output is JSONL: `{"type":"message",...}` rows in file order, then a final `{"type":"summary",...}`. Message fields: `file_path`, `line_number`, `message_uuid`, `parent_uuid`, `role`, `timestamp`, `content`, plus `is_target: true` on the anchor row and `content_truncated: true` when `--max-chars` (default 2000) cut the content. `--context` (default 3) counts messages on each side; interleaved non-message records do not shrink the window. Near a fork of a branched session the window follows file order, not the chain — use `parent_uuid` to see the actual structure. In tail mode the summary has `"mode": "tail"` and `total_messages`.

Codex keeps a reverted or forked thread in a new rollout file that points at a prefix of an earlier one (`session_meta.history_base`) instead of copying it. `ccs` reads such a session as that prefix plus the file's own records: `list`, message counts, `show` and `--tail` cover the whole history, and a `show` row whose `file_path` differs from the summary's comes from the inherited part. A reverted thread shows up once, as its newest rollout.

## `ccs list`

`ccs list` writes JSONL, one object per recent session. `--limit` defaults to 50, and there is no summary line, so the output stops silently at the limit.

Fields:

- `host`
- `session_id`
- `project`
- `provider`
- `source`
- `file_path`
- `last_active`
- `message_count`

Example:

```bash
ccs list --limit 20
```

## `ccs ai`

`ccs ai` opens the TUI already in AI search, as if you had pressed `Ctrl+G`. With a query it starts the search as soon as the TUI is up. It helps where `Ctrl` shortcuts are awkward, such as a terminal on a phone. Quotes around the query are optional, and `--overlay` works before or after `ai`.

```bash
ccs ai
ccs ai where I fixed the flaky docker build
ccs ai --overlay
```

## `ccs pick`

`ccs pick` opens the same TUI, but prints the selected session as key-value output. It exits with code `0` after a selection and `1` on cancel. The TUI draws on stdout, so a script that captures the output needs `--output`.

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
host: local
```

`message_uuid` is included when the selected row maps to a concrete message, such as a search result or tree selection.

## Overlay mode

```bash
ccs --overlay
```

Overlay mode resumes the selected session as a child process. When the resumed tool exits, `ccs` always returns to the picker in the state you left it, however long the session ran.

This is useful when you want to review or resume several related sessions without relaunching the picker each time.

## Tree view from the command line

Open the tree view directly:

```bash
ccs --tree <session-id-or-path>
```

Resume from a specific message UUID:

```bash
ccs --overlay --tree /path/to/session.jsonl --resume-uuid <message-uuid>
```

This form resumes once and exits; it does not open the picker.
