## ADDED Requirements

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
