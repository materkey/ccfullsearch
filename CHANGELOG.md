# Changelog

## v0.11.0 - 2026-04-22

### New Features
- TUI redesign — unified recent-sessions and search-result grammar (`▶ [src] date | project | branch | sid (count)` + role-prefixed preview line)
- Inline query highlighting in collapsed previews and expanded sub-matches

### Fixed
- Resume selected session on Enter after AI re-rank instead of re-submitting the query
- Strip `\n`/`\t`/`\r` from single-line list previews so wrapped text no longer overwrites the next header
- Centre collapsed preview around the first query match instead of always front-aligning
- Invalidate AI rank when toggling regex / project / automation filters (Ctrl+R, Ctrl+A, Ctrl+H) so Enter resumes the visible list, not the stale ranking
- Caret glyph clipping on selected rows (drop `Modifier::BOLD` so Iosevka renders `▶` at full width)

### Changed
- Scale list rendering with visible rows on scroll — drops 30-event scroll burst from ~970 ms to ~88 ms in debug builds
- Skip filter rebuild on background `message_count` updates (no `SessionGroup` cloning per tick)
- Align selection colours with design handoff (purple selection extends to preview row, dim text uses `#6b7180` instead of `Color::DarkGray`)

## v0.10.0 - 2026-04-19

### New Features
- Detect claude-mem observer sessions as automation (path-based `~/.claude-mem/observer-sessions/` + content marker `<observed_from_primary_session>`)
- Hide automation sessions by default — new `AutomationFilter::Manual` default (toggle Ctrl+H: Manual → Auto → All)
- Per-class cap in `collect_recent_sessions` — Manual filter is no longer crowded out when auto sessions dominate mtime
- AI-powered session re-ranking via Ctrl+G (invokes `claude -p` in background to sort visible sessions by relevance to a query)
- Adaptive help bar — hints drop based on terminal width
- Preview line for search results with correct scroll behavior on ListState
- Show total session message count in each search result group
- Expanded preview content parsing — tool_use, tool_result, thinking, image, document, redacted_thinking, server_tool_use, connector_text
- Sort recent sessions by content timestamp (max `timestamp`/`_audit_timestamp` from JSONL) instead of file mtime

### Fixed
- Anchor claude-mem content marker to message start — sessions discussing the marker no longer mis-classify as automation
- Clamp `InputState` cursor to UTF-8 char boundary (prevents panic on multi-byte input editing)
- Ripgrep search: validate regex before spawn, capture stderr, align `--ignore-case` with post-filter
- Clarify search result count label — "matches" instead of "messages"
- Fix flaky ripgrep tests by moving PATH-mutating test to integration tests

### Changed
- Grey `[Ctrl+H] Manual` hint (was green+bold) to match the new default state; Auto stays magenta+bold
- Extract shared `collect_session_jsonl_files` walker — single skip policy for `subagents/` and `agent-*.jsonl`
- Try cheap decode strategies before filesystem walk in `path_codec` (perf)

## v0.8.0 - 2026-04-05

### New Features
- Add `ccs pick` subcommand for machine-readable session selection (key-value output with exit code 0/1)
- Add `--overlay` flag for TUI loop mode: resume sessions as child processes, return to TUI after exit
- Add `[PICK]` status bar indicator when running in picker mode
- Add `resume_cli_child()` for spawning Claude as a child process instead of exec()
- Add shell launcher script (`launch-ccs.sh`) with tmux/kitty/wezterm overlay support
- Add Claude Code plugin structure (`.claude-plugin/`) with overlay picker skill

### Changed
- Refactor TUI lifecycle into `run_tui()` returning `TuiOutcome` enum (Quit/Resume/Pick)
- Extract `PickedSession` struct with `to_key_value()` serialization

## v0.7.0 - 2026-04-02

### New Features
- Support `CLAUDE_CONFIG_DIR` env var for custom Claude config directory
- Add Linux Desktop session path (`~/.config/Claude/local-agent-mode-sessions/`)
- Extract session titles from JSONL metadata (custom-title, ai-title, agent-name) with Claude Code priority: agentName > customTitle > aiTitle > summary > lastPrompt > firstUserMessage
- Include thinking blocks in search content extraction
- Filter agent/subagent files from search results and recent sessions
- Deduplicate sessions across git worktrees by session_id

### Fixed
- Fix leaf finding: use terminal messages instead of last uuid in file
- Add `isSidechain` filtering — subagent messages no longer hijack latest chain
- Add `compact_boundary` handling — tree and fork no longer break at compaction points
- Add `logicalParentUuid` fallback — tree stays connected across compaction boundaries
- Skip metadata-only lines (summary, tag, custom-title) in fork output
- Detect ralphex automation markers in assistant messages, not just first user message
- Detect `<<<RALPHEX:` markers anywhere in message text (remove instruction-cue requirement)

### Changed
- Remove dead synthetic linearization code (-1062 lines)
- Cross-project resume via decoded project path + session index registration
- Deduplicate `logicalParentUuid` fallback into `session::extract_parent_uuid_or_logical()`
- Eliminate triple JSON parsing in `create_fork` (single-pass with stored parsed values)

## v0.6.1 - 2026-03-30

### Fixed
- Stop marking manual sessions as automated when they only discuss or quote `ralphex`/`<scheduled-task>` markers
- Resume branched sessions directly from the original session file instead of forcing a synthetic linear copy from recent-session view

## v0.6.0 - 2026-03-30

### New Features
- Show recent sessions on TUI startup so session browsing works before typing a query
- Detect and filter automated sessions in both recent-session and search views, including scheduled-task runs
- Add `ccs update` for self-updating installed builds

### Changed
- Add Left/Right cursor movement in the search input
- Simplify resume analysis and session metadata extraction to better handle compacted transcripts

### Fixed
- Resume and branch handling for subagent, auxiliary, compacted, and non-latest sessions
- Recent-session summaries and automation metadata for malformed tails, older sessions, and abandoned branches
- Project/path display and filtering edge cases, including sibling-project prefix collisions and non-message UUID records

## v0.5.0 - 2026-03-20

### Fixed
- Fix stale search results remaining visible when query is deleted to empty

## v0.4.0 - 2026-03-09

### New Features
- Homebrew tap: `brew install materkey/ccs/ccs`
- Shell installer for macOS/Linux
- Published to crates.io as `ccfullsearch`
- cargo-binstall support

### Changed
- Rename package to `ccfullsearch` (binary stays `ccs`)
- Replace release workflow with cargo-dist v0.31.0
- Add Linux arm64 and musl targets (6 platforms total)
- Add MIT license

## v0.3.0 - 2026-03-06

### New Features
- CLI argument parsing with clap (`search`, `list` subcommands, `--tree` flag)
- Render snapshot tests
- Split TUI into separate modules (state, search_mode, tree_mode, render)

### Fixed
- Project filter search scope

## v0.2.0 - 2026-03-05

### New Features
- CLI mode with `search` and `list` subcommands (JSONL output)
- Claude Code skill for automatic session search
- Branch Tree Explorer mode for navigating transcript branches
- Branch-aware resume with automatic fork for non-latest chains
- Claude Desktop sessions support
- Regex search mode toggle
- Word-level cursor movement and deletion
- Ctrl-C support (clear input or quit)
- GitHub release workflow (macOS arm64/x86_64, Linux x86_64, Windows)

### Fixed
- Session resume for paths with spaces, parens, underscores
- Content overflow artifacts in tree view
- Search not finding messages with plain string content
- Desktop session format parsing
- UTF-8 boundary panics in preview and context extraction
- Terminal rendering glitches
- Windows build (conditional compilation for unix exec)

## v0.1.0 - 2026-03-02

### New Features
- Initial implementation of Claude Code session search TUI
- Full-text search across session JSONL files using ripgrep
- Session grouping with timestamps and project context
- Interactive preview with match navigation
- Session resume from search results
