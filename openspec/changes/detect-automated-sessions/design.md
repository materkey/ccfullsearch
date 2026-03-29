## Context

ccfullsearch scans `~/.claude/projects/` JSONL files to build recent sessions lists and search results. Ralphex (and potentially other automation tools) create sessions via `claude --print` with prompts containing signal markers like `<<<RALPHEX:ALL_TASKS_DONE>>>`. These markers appear in user-type JSONL records. Currently all sessions appear identical — no metadata distinguishes automated from manual.

The `extract_summary` function already reads the first 30 lines and file tail during session scanning. The detection piggybacks on this existing I/O with negligible overhead.

## Goals / Non-Goals

**Goals:**
- Detect ralphex-created sessions reliably with zero false positives
- Provide TUI filtering to show/hide automated sessions
- Make detection extensible for future automation tools
- Zero additional file I/O — detection during existing scan passes only

**Non-Goals:**
- Detecting automation tools that leave no content markers (would require heuristics)
- Per-tool filtering (e.g., show ralphex but hide another tool) — single auto/manual split is sufficient
- CLI subcommand filtering (`ccs list --manual`) — TUI only for now
- Modifying ralphex or any external tool

## Decisions

### 1. Detection via user message content markers

**Decision**: Scan user-type JSONL records for `<<<RALPHEX:` substring during `extract_summary`.

**Rationale**: These markers are part of ralphex's control protocol (used for signal parsing), making them stable and contractual. They appear in every ralphex session's user messages. No JSONL metadata field distinguishes automated sessions — `userType`, `entrypoint`, `permissionMode` are identical to manual sessions.

**Alternatives considered**:
- First-message heuristics ("External code review", "Read the plan file") — fragile, false positives possible
- External marker files — requires ralphex changes, doesn't cover existing sessions
- Message count heuristics — ralphex sessions can have 12-25 messages, overlapping with manual

### 2. `Option<String>` field naming the tool

**Decision**: Store `automation: Option<String>` (e.g., `Some("ralphex")`) rather than a boolean.

**Rationale**: Extensible — if other tools are added later, the UI can distinguish them. A boolean would need migration. The string value comes from the detector, not from parsing the marker content.

### 3. Three-state filter with Ctrl+H

**Decision**: Cycle All → Manual → Auto on `Ctrl+H`. Default: All.

**Rationale**:
- All as default: nothing hidden unexpectedly, `[A]` indicators immediately visible
- Three states cover all use cases: browse everything, focus on manual work, inspect automated runs
- `Ctrl+H` is free (H = hide automated), consistent with existing `Ctrl+R` (regex) / `Ctrl+A` (project) pattern
- Filter applies to both recent sessions and search results (like project filter)

### 4. Detection in both recent sessions and search results

**Decision**: Detect in `extract_summary` for recent sessions. For search results, detect per-`SessionGroup` by checking if any user-role match in the group contains the marker.

**Rationale**: Both views need the indicator. Search matches already have parsed message content available, so detection is a simple `.contains()` check during grouping.

## Risks / Trade-offs

- **Marker stability** → Ralphex markers are part of its signal protocol; changing them would break ralphex itself. Low risk.
- **False negatives for non-ralphex tools** → Other automation tools without markers won't be detected. Mitigated by extensible detector list — add new patterns as tools are encountered.
- **Tail-only summary sessions** → If `extract_summary` finds a summary in the tail and returns early (before scanning user messages in the head), automation won't be detected. Mitigation: also scan user messages in the tail region for markers.
- **Marker in assistant content** → A manual session discussing ralphex (like the current conversation) could contain `<<<RALPHEX:` in assistant messages. Mitigation: only check user-role records.
