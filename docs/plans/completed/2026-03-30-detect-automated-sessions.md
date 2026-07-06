# Detect Automated Sessions

> Consolidated from the former `openspec/changes/detect-automated-sessions/` documents (proposal, design, specs, tasks). Implemented in `408e115`.

## Why

Ralphex and similar automation tools create dozens of Claude Code sessions that are indistinguishable from manually created ones. This makes it hard to find your own sessions in the recent sessions list and search results. Sessions need to be tagged at scan time and filterable in the TUI.

## What Changes

- Detect automated sessions during JSONL scanning by looking for tool-specific markers in user message content (e.g., `<<<RALPHEX:` signals)
- Add `automation: Option<String>` field to `RecentSession` and propagate detection to `SessionGroup`
- Add a three-state filter (All / Manual only / Auto only) toggled via `Ctrl+H` in the TUI
- Visually mark automated sessions with a dim `[A]` indicator in both recent sessions and search result views
- Show active filter mode in the search title bar (e.g., `Search [Manual]`)

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

## Spec: automation-detection

### Requirement: RecentSession carries automation field
`RecentSession` SHALL have an `automation: Option<String>` field. When the session was created by a known automation tool, the field SHALL contain the tool name (e.g., `"ralphex"`). For manual sessions, the field SHALL be `None`.

#### Scenario: Ralphex session detected
- **WHEN** a JSONL file contains a user-type record whose content includes `<<<RALPHEX:`
- **THEN** `extract_summary` SHALL return a `RecentSession` with `automation = Some("ralphex")`

#### Scenario: Manual session not flagged
- **WHEN** a JSONL file contains only user-type records without any automation markers
- **THEN** `extract_summary` SHALL return a `RecentSession` with `automation = None`

#### Scenario: Marker in assistant message ignored
- **WHEN** a JSONL file contains `<<<RALPHEX:` only in assistant-type records (e.g., a conversation discussing ralphex)
- **THEN** `extract_summary` SHALL return `automation = None`

### Requirement: Detection during existing scan passes
The automation detection SHALL NOT open files or read additional bytes beyond what `extract_summary` already reads. Detection SHALL occur by checking user-message content that is already parsed in the head scan (first 30 lines), middle scan, or tail scan.

#### Scenario: Head scan detection
- **WHEN** the first user message (within first 30 lines) contains `<<<RALPHEX:`
- **THEN** the session SHALL be detected as automated

#### Scenario: Tail scan detection
- **WHEN** `extract_summary` reads the tail region and encounters a user-type record containing `<<<RALPHEX:`
- **THEN** the session SHALL be detected as automated even if the head scan found a summary early-return

### Requirement: SessionGroup carries automation field
`SessionGroup` SHALL have an `automation: Option<String>` field, derived from its grouped matches. If any user-role `RipgrepMatch` in the group has message content containing `<<<RALPHEX:`, the group SHALL be marked `automation = Some("ralphex")`.

#### Scenario: Search result group from ralphex session
- **WHEN** search results are grouped and at least one user-role match contains `<<<RALPHEX:`
- **THEN** the `SessionGroup` SHALL have `automation = Some("ralphex")`

#### Scenario: Search result group from manual session
- **WHEN** no user-role match in a group contains any automation marker
- **THEN** the `SessionGroup` SHALL have `automation = None`

### Requirement: Extensible marker registry
Automation markers SHALL be defined as a list of `(pattern, tool_name)` pairs, not hardcoded inline. Adding a new automation tool SHALL require adding one entry to this list.

#### Scenario: Adding a new automation tool
- **WHEN** a new tool (e.g., "aider") uses `<<<AIDER:` markers in user messages
- **THEN** adding `("<<<AIDER:", "aider")` to the marker list SHALL enable detection without other code changes

## Spec: automation-filter

### Requirement: Three-state automation filter
The TUI SHALL support an automation filter with three states cycled by `Ctrl+H`:
1. **All** (default) — show all sessions
2. **Manual** — show only sessions with `automation = None`
3. **Auto** — show only sessions with `automation.is_some()`

#### Scenario: Default state shows all sessions
- **WHEN** the TUI starts
- **THEN** all sessions (manual and automated) SHALL be visible

#### Scenario: Cycling to Manual mode
- **WHEN** the user presses `Ctrl+H` once from All mode
- **THEN** only sessions with `automation = None` SHALL be visible

#### Scenario: Cycling to Auto mode
- **WHEN** the user presses `Ctrl+H` twice from All mode
- **THEN** only sessions with `automation.is_some()` SHALL be visible

#### Scenario: Cycling back to All
- **WHEN** the user presses `Ctrl+H` three times from All mode
- **THEN** all sessions SHALL be visible again

### Requirement: Filter applies to recent sessions
The automation filter SHALL filter the recent sessions list (empty search state). The filter SHALL compose with the project filter (`Ctrl+A`) — both filters apply simultaneously.

#### Scenario: Combined project and automation filter
- **WHEN** project filter is active AND automation filter is set to Manual
- **THEN** only manual sessions from the current project SHALL be visible

#### Scenario: Cursor reset on filter change
- **WHEN** the automation filter changes
- **THEN** the recent sessions cursor SHALL reset to 0

### Requirement: Filter applies to search results
The automation filter SHALL filter search result groups. When set to Manual, groups with `automation.is_some()` SHALL be hidden. When set to Auto, groups with `automation = None` SHALL be hidden.

#### Scenario: Search results filtered to manual
- **WHEN** search results contain 3 manual and 2 automated groups, and filter is Manual
- **THEN** only the 3 manual groups SHALL be displayed

### Requirement: Visual indicator for automated sessions
In All mode, automated sessions SHALL display a `[A]` prefix before the summary text. The `[A]` indicator SHALL be rendered in `Color::DarkGray`. The summary text of automated sessions SHALL be rendered in `Color::Gray` (dimmer than manual sessions' `Color::White`).

#### Scenario: Automated session in recent list
- **WHEN** an automated session is rendered in All mode in the recent sessions list
- **THEN** `[A] ` SHALL appear before the summary, styled `Color::DarkGray`

#### Scenario: Manual session has no indicator
- **WHEN** a manual session is rendered
- **THEN** no `[A]` prefix SHALL appear

#### Scenario: Automated session in search results header
- **WHEN** an automated `SessionGroup` is rendered in the search results list
- **THEN** `[A]` SHALL appear in the group header line

### Requirement: Filter state in search title
The search input title SHALL reflect the active automation filter:
- All mode: no indicator added
- Manual mode: `[Manual]` appended to title
- Auto mode: `[Auto]` appended to title

The indicator SHALL compose with existing indicators (`[Regex]`, `[Project]`).

#### Scenario: Manual filter with regex
- **WHEN** automation filter is Manual and regex mode is on
- **THEN** the title SHALL read `Search [Regex] [Manual]`

#### Scenario: All mode title unchanged
- **WHEN** automation filter is All
- **THEN** no automation indicator SHALL appear in the title

### Requirement: Help bar shows Ctrl+H
The help bar SHALL include `[Ctrl+H] Filter` in all modes where the automation filter is applicable (recent sessions mode and search results mode).

#### Scenario: Help bar in recent sessions mode
- **WHEN** the user is in recent sessions mode
- **THEN** the help bar SHALL include `[Ctrl+H] Filter`

## Tasks

### 1. Automation Detection Core

- [x] 1.1 Add automation marker registry: a `const` list of `(&str, &str)` pairs `[("<<<RALPHEX:", "ralphex")]` in `src/session.rs` and a `detect_automation(content: &str) -> Option<String>` function
- [x] 1.2 Add `automation: Option<String>` field to `RecentSession` in `src/recent.rs`
- [x] 1.3 Wire detection into `extract_summary` head scan: when parsing user-type records in first 30 lines, call `detect_automation` on message content
- [x] 1.4 Wire detection into `extract_summary` tail scan: check user-type records in `find_summary_from_tail` for automation markers
- [x] 1.5 Wire detection into `extract_summary` middle scan: propagate automation flag through pass 3
- [x] 1.6 Unit tests for `detect_automation` (ralphex marker, no marker, marker in non-user content)
- [x] 1.7 Unit test for `extract_summary` returning `automation = Some("ralphex")` from fixture JSONL

### 2. Search Results Detection

- [x] 2.1 Add `automation: Option<String>` field to `SessionGroup` in `src/search/group.rs`
- [x] 2.2 Detect automation during `group_by_session`: check user-role matches for markers
- [x] 2.3 Unit test for `SessionGroup` automation detection from grouped matches

### 3. TUI Filter State

- [x] 3.1 Add `AutomationFilter` enum (All, Manual, Auto) and `automation_filter` field to `App` in `src/tui/state.rs`
- [x] 3.2 Implement `toggle_automation_filter` method cycling All → Manual → Auto → All
- [x] 3.3 Update `apply_recent_sessions_filter` to compose automation filter with project filter
- [x] 3.4 Add `Ctrl+H` key binding in `src/main.rs` calling `toggle_automation_filter`
- [x] 3.5 Filter search result groups by automation state when rendering (or in a filtering pass before render)

### 4. TUI Visual Indicators

- [x] 4.1 Render `[A]` prefix (DarkGray) before summary for automated sessions in `render_recent_sessions`
- [x] 4.2 Dim automated session summary text to `Color::Gray` (vs `Color::White` for manual)
- [x] 4.3 Render `[A]` in search result group headers for automated `SessionGroup`s in `render_groups`
- [x] 4.4 Add `[Manual]` / `[Auto]` indicator to search title bar (compose with `[Regex]` and `[Project]`)
- [x] 4.5 Add `[Ctrl+H] Filter` to help bar in recent sessions and search result modes

### 5. Integration Tests

- [x] 5.1 Add a fixture JSONL file with ralphex markers for testing
- [x] 5.2 Integration test: `extract_summary` on ralphex fixture returns `automation = Some("ralphex")`
- [x] 5.3 Integration test: `extract_summary` on existing manual fixture returns `automation = None`
