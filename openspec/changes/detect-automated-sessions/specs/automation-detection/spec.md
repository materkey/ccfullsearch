## ADDED Requirements

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
