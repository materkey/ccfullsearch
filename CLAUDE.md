# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

**ccfullsearch** (`ccs`) — a TUI and CLI tool for searching and browsing Claude Code CLI and Claude Desktop session history. Built in Rust with ratatui (TUI), ripgrep (search), crossterm (terminal), clap (CLI parsing), and rayon (parallelism). Requires `rg` (ripgrep) in PATH at runtime.

## Build & Development Commands

```bash
cargo build                    # Debug build
cargo build --release          # Release build
cargo run                      # Run TUI mode
cargo run -- search "query"    # CLI search subcommand
cargo run -- list              # List all sessions
cargo run -- update            # Self-update to latest release
cargo run -- --tree <file|id>  # Open tree mode directly for a session
cargo run -- pick              # Pick a session interactively (key-value output)
cargo run -- pick "query"      # Pick with pre-filled search query
cargo run -- --overlay         # Overlay mode: resume as child, return to TUI

cargo fmt --check              # Check formatting
cargo clippy --all-targets --all-features -- -D warnings  # Lint
cargo test                     # Run all tests
cargo test tree_parsing        # Run a single test module
cargo test test_search_finds   # Run a single test by name
```

CI runs: `cargo fmt --check` → `cargo clippy` → `cargo test` (see `.github/workflows/ci.yml`).

## Architecture

```
src/
├── main.rs           # CLI parsing (clap), panic handler, run_tui() lifecycle, TuiOutcome-based outer loop
├── lib.rs            # Module re-exports + get_search_paths()
├── cli.rs            # Non-interactive subcommands (search, list, show), --host merging with remote replies
├── project_match.rs  # Path rules shared by Ctrl+A and `ccs serve`: path_is_within_project, home_relative, remote_project_matches
├── recent.rs         # RecentSession struct, parallel scanning (rayon), summary extraction via SessionDag/SessionRecord
├── agent_search.rs   # Ctrl+G agent search: build_agent_args()/system_prompt() for a lean `claude -p` limited to `<ccs> search`/`<ccs> show`, parse_stream_line() over stream-json, spawn_agent_search() → AgentSearchHandle (watchdog + reader threads, timeout, synchronous kill on cancel/Drop), AgentEvent::{Progress, Done}, resolve_hits() → FoundSession, resolve_ccs_path()
├── config.rs         # ~/.config/ccs/config.toml ($XDG_CONFIG_HOME aware): [agent_search] model/effort
├── update.rs         # Self-update: GitHub release download, Homebrew detection, version comparison
├── remote/
│   ├── mod.rs        # Host (Local | Remote(name)), SessionRef { host, file_path } with local_path(), HostSelection + select_hosts() for --host
│   ├── config.rs     # HostConfig, `[[hosts]]` of config.toml / CCS_HOSTS → hosts (resolve_hosts, load_hosts)
│   ├── ssh.rs        # ssh_command(host, SshMode::{Serve, Command, Resume}) — the only place ssh argv is built; CCS_SSH overrides the program
│   ├── wire.rs       # `ccs serve` protocol structs (WireHello, WireRequest/Response, WireSearch/Recent/Show/Tree), PROTOCOL_VERSION, golden test
│   ├── server.rs     # Serving side: search/recent/show/tree computed where the files are (snippets, counts, automation, on_latest_chain, project scope)
│   ├── serve.rs      # `ccs serve --stdio` loop: hello, one thread per request, cancel registry, catch_unwind, EOF cancels all
│   ├── client.rs     # HostClient + HostLink trait: one ssh per host, HostState machine, hello deadline, per-request timeouts, exit classification
│   └── oneshot.rs    # query_hosts(): one request to several hosts with a deadline, for CLI commands
├── session/
│   ├── mod.rs        # SessionSource enum (ClaudeCodeCLI | ClaudeDesktop), shared field extractors, resolve_parent_session, extract_message_content
│   └── record.rs     # SessionRecord enum (Message, Summary, CustomTitle, etc.), ContentBlock, ContentMode, MessageRole, parse_content_blocks() (handles text, tool_use, tool_result, thinking, image, document, redacted_thinking, server_tool_use, connector_text), render_content() — unified JSONL parsing and content rendering
├── dag/
│   └── mod.rs        # SessionDag — unified DAG engine: TipStrategy (LastAppended | MaxTimestamp), DisplayFilter, chain_from(), from_file()
├── search/
│   ├── mod.rs        # Module re-exports for search API surface
│   ├── ripgrep.rs    # Spawns `rg --json`, parses matches, post-filters content, extracts project names
│   ├── message.rs    # Parses JSONL lines into Message structs (content via Full mode, text_content via TextOnly for preview), delegates to SessionRecord
│   └── group.rs      # Groups RipgrepMatch by session_id, sorts by timestamp
├── tree/
│   └── mod.rs        # DagNode/TreeRow/SessionTree rendering model, uses SessionDag for chain building and SessionRecord for content
├── resume/
│   ├── mod.rs        # Resume orchestration (CLI exec vs Desktop open, resume_child for overlay)
│   ├── path_codec.rs # Encodes/decodes filesystem paths to Claude's dash-separated format
│   ├── fork.rs       # Creates forked JSONL files for branch-aware resume, uses SessionDag for chain logic
│   └── launcher.rs   # Process exec (Unix) / spawn (Windows), resume_cli_child() for overlay, safe path fallback (decode→parent→$HOME→/tmp), atomic session index
└── tui/
    ├── hosts_state.rs# HostsState (links, pending requests, per-host search buffers, status), HostFilter (Ctrl+L), test FakeLink
    ├── state.rs      # App struct (with InputState, SearchState, TreeState, AiState sub-structs), ResumeTarget, AppOutcome, BackgroundSearchResult, SearchHandle (per-request cancel token), spawn-per-request search with cooperative + forced (Child::kill) cancellation, Ctrl+G agent search lifecycle
    ├── dispatch.rs   # KeyAction enum (incl. EnterAiMode/ExitAiMode/CancelAiSearch), KeyContext struct (incl. ai_mode/ai_searching/ai_has_hits), classify_key() — maps key events to semantic actions
    ├── view.rs       # AppView<'a> — read-only projection of App for pure rendering
    ├── search_mode.rs# Search navigation, filtering, input handling, recent sessions navigation
    ├── tree_mode.rs  # Tree mode enter/exit, DAG navigation
    ├── render_search.rs # Search results + preview rendering (takes &AppView, not &mut App); defines HintItem/build_help_line() for adaptive width-aware help bars, used by render_tree.rs; recent_session_header_item() shared with render_ai.rs
    ├── render_ai.rs  # Ctrl+G results list (session header + `↳ why`), AI status line (progress queries) and hints
    └── render_tree.rs   # Tree DAG rendering with graph symbols (takes &AppView, not &mut App); imports HintItem/build_help_line from render_search.rs

.claude-plugin/
├── plugin.json                         # Claude Code plugin manifest (name, version, skills path)
└── skills/ccs/
    ├── SKILL.md                        # Skill definition with CLI and overlay picker modes
    └── scripts/launch-ccs.sh           # Shell launcher: tmux/kitty/wezterm overlay for ccs pick
```

### Key data flow

1. **Search**: spawn-per-request with cooperative + forced cancellation. `SearchHandle { seq, cancel }` in `src/tui/state.rs` is the single source of truth for in-flight state; `is_searching()` is a derived predicate (`self.search.current.is_some()`), not a stored flag. User types → 300 ms debounce → `start_search` increments `search_seq`, cancels the prior handle via `SearchHandle.cancel.store(true)`, and installs a fresh handle. The per-request thread streams `rg --json --glob="*.jsonl"` via `Command::spawn()` + `BufReader::lines()`, checking `cancel.load()` on every line; on cancel: `child.kill()` + `child.wait()` + `Err("cancelled")`. A dedicated stderr-drain thread keeps the pipe buffer from filling and surfaces stderr text in any error message. On completion the thread sends `BackgroundSearchResult` only if `!cancel.load()`, suppressing zombie sends from superseded requests. `App.tick()` polls `result_rx` and routes by `seq == current.seq` (stale results are dropped). On match: clears `current`, parses matches into `Message` (Full mode for content, TextOnly mode for clean preview), post-filters so the query actually appears in message *content* (not metadata), groups by `session_id`, sorts by timestamp desc. If any file hit the per-file match limit (1000), `truncated` is flagged and a warning surfaces in the status bar.
2. **Tree mode**: Load full JSONL file → `SessionDag::from_file(path, Standard)` builds DAG in single pass (parses via `SessionRecord::from_jsonl`, filters sidechains, bridges `compact_boundary` via `logicalParentUuid`) → `dag.tip(MaxTimestamp)` picks latest terminal → `dag.chain_from(tip)` walks backward → mark branch points (nodes with >1 child) → flatten to `TreeRow` list. Content rendered via `SessionRecord::render_content(blocks, Preview { max_chars: 120 })`
3. **Resume**: On Enter, find `claude` binary via `which` → resolve project working directory from session path (only use decoded path if it exists on disk; fall back to session file parent → `$HOME` → `/tmp`) → if selected message is NOT on latest chain, create a forked JSONL file: `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from(tip)` determine the chain, then write filtered records → exec/spawn `claude --resume <file-path>` (absolute `.jsonl` path for cross-project support). Session index uses per-process temp files for atomic writes.
4. **TUI lifecycle**: `main()` calls `run_tui()` → key events go through `classify_key()` → `KeyAction` enum → `App::handle_action()`. Returns `TuiOutcome` (Quit, Resume, Pick). `run_tui_loop` keeps one `App` across passes (`run_tui(&mut app)`), so a return reopens the picker in the exact state it was left in: query, filters, cursor, tree, AI search results. `take_outcome` settles pending search work first (`suspend_for_resume`), and `return_from_resume` marks the session that ran as `[opened]`. Terminal sessions (Claude CLI, Codex, Opencode, i.e. everything but Claude Desktop, see `resume::runs_in_terminal`) run as a child via `resume_child()` in both modes; while a child runs, `wait_child` ignores SIGINT/SIGQUIT in ccs, stops ccs when the child stops itself (Ctrl+Z) and flushes tty input after it exits. In overlay mode (`--overlay`) the picker always comes back. In normal mode it comes back only after a quick exit (< `QUICK_EXIT_WINDOW`, 60 s) — likely the wrong session — and a longer session ends ccs; Claude Desktop and a direct `--tree` + `--resume-uuid` resume still `exec`. An error exit within `BOOTSTRAP_WINDOW` (10 s) of launch from any child counts as a failed resume — `ccs` prints it, waits for Enter and reopens the picker without marking the session. In tree mode, Enter on the tip the tree was loaded with resumes without a uuid (`SessionTree::tip_uuid`), so records a quick `/exit` appended do not turn it into a fork. `Pick` writes `PickedSession` key-value output and exits 0/1. Rendering uses `AppView` (read-only projection) — no mutation during draw.
5. **Recent sessions**: App starts → background thread walks search dirs for `*.jsonl` (skip `agent-*` and `subagents/`) → sort by mtime → take top 100 → rayon parallel: `SessionDag::from_file` + `dag.tip(LastAppended)` + `dag.chain_from(tip)` for latest chain, then scan JSONL using `SessionRecord` match arms for title extraction (priority: agentName > customTitle > aiTitle > summary > lastPrompt > firstUserMessage) → deduplicate by session_id (keep newest) → sort by **content timestamp** desc (max `timestamp`/`_audit_timestamp` from JSONL lines; falls back to file mtime) → send via mpsc to TUI → render in empty-state view. For Codex rollouts (`~/.codex/sessions/...`) the scanner also fills `RecentSession.cwd` from `session_meta.payload.cwd` via `read_codex_session_cwd`, canonicalized through `std::fs::canonicalize` to match macOS `/var → /private/var` and `~/projects` symlink farms; Claude sessions encode the project in the file path itself so `cwd` stays `None`.
6. **Project filter (Ctrl+A) — Claude path + Codex cwd union**: `App::new` captures `current_cwd` once via `std::fs::canonicalize(env::current_dir())` (fallback to raw on failure). `Ctrl+A` calls `toggle_project_filter`: if both `current_project_paths` and `current_cwd` are empty/`None`, early-return; otherwise flip `project_filter` and either pin `search_paths = current_project_paths` (Claude paths exist) or fall back to `all_search_paths` (Codex-only projects — ripgrep needs a non-empty scope). `RecentState::apply_filter` then builds a **union** source: `self.project` (Claude sessions from `start_project_load`) ∪ Codex sessions from `self.all` (deduped by `session_id`). The union exists only when `self.project.is_some()` — before the project load finishes, `self.all` already contains Codex sessions so unioning would double-count. Each candidate passes `session_matches_project(session, project_paths, current_cwd)`: matches if any `project_path` is a prefix of `session.file_path` (Claude convention) OR `path_is_within_project(session.cwd, current_cwd)` — asymmetric, session.cwd must lie inside (or equal) current_cwd. A previous symmetric variant was reverted because Codex sessions recorded at `$HOME` falsely matched every project under `$HOME`. `recent.total_count` uses the same union math so `recent_sessions_status_text` keeps `"N recent (M hidden by filter)"` honest. When `project_filter && current_cwd.is_some()`, status appends `· Codex: ≤100 by recency` — `start_project_load` scans only Claude paths, so Codex sessions are visible to the filter only while they fit in the global top-`RECENT_SESSIONS_LIMIT` (100) by mtime.
7. **AI search (Ctrl+G)**: Press Ctrl+G → `EnterAiMode` → `AiState.active = true`, input switches to the AI query (magenta). `AiState` has three phases: input (no agent, no hits), searching (`ai.search: Some(AgentSearchHandle)`), results (`ai.hits: Some(Vec<FoundSession>)`). Enter → `submit_ai_query()` reads `config::load_default_config()` (on every search, so config edits apply without restart), resolves `claude` via `which`, and `start_agent_search()` calls `agent_search::spawn_agent_search(query, claude, App.ccs_path, cfg)`. `App.ccs_path` is resolved once in `App::new` (`resolve_ccs_path`: `current_exe()`, or `/proc/<pid>/exe` when Linux reports `... (deleted)` after an in-place upgrade), so the agent always calls the running ccs version. The agent is `claude -p` with lean flags (configured model, default `claude-haiku-5-5`; optional `--effort`; `--tools Bash` with `--allowedTools "Bash(<ccs> search:*)" "Bash(<ccs> show:*)"`; `--strict-mcp-config --setting-sources "" --disable-slash-commands --no-session-persistence --output-format stream-json --verbose`; stdin null; cwd `$TMPDIR/ccs-agent-search`). `--no-session-persistence` is load-bearing: a persisted agent session quotes the query and wins every later search. A watchdog thread enforces cancel and the 120 s `AGENT_TIMEOUT`; `AgentSearchHandle::cancel()` (also on Drop) kills the child synchronously. The reader parses stream-json: Bash `tool_use` commands with `<ccs> search "<q>"` become `AgentEvent::Progress` (status bar shows `searching: "q1", "q2"`); the `result` line becomes the answer (errors from `result`, `errors` or `subtype`; a received result wins over a later timeout); no result → `claude exited with ...` plus stderr and the last stdout lines. Hits are parsed from the first JSON array (≤5) and resolved via `recent::extract_summary` (missing files and Opencode `#` paths dropped; none left → `AGENT_NO_MATCH_MSG`); hits in one session merge into one row whose `FoundSession.whys` keeps every reason. `tick()` drains events; a channel closed without `Done` shows `agent stopped unexpectedly`. Key handling (`handle_ai_list_action`): Esc while searching → `CancelAiSearch` (`KeyContext.ai_searching`), Ctrl+G always `ExitAiMode`; while searching Enter/Up/Down/Ctrl+B are no-ops; with results Up/Down move `hit_cursor`, Enter resumes the hit at its latest point (`search_mode::session_outcome`, no uuid), Right/Ctrl+B open its tree (exiting the tree returns to the hits; `classify_ctrl_b` routes Ctrl+B to AI mode via `KeyContext.ai_has_hits`/`ai_searching` even when the hidden list is empty). Tab/Ctrl+V do nothing in every AI phase and arriving hits close any preview, so the hidden recent/search list is never acted on. Any query edit stops the agent and clears hits/progress/error (`AiState::reset_search`). The recent and search lists are never reordered; toggles (Ctrl+R/A/H) leave AI state alone. `suspend_for_resume` drops a running agent but keeps hits for the return. Hits render in `render_ai.rs` with the shared `recent_session_header_item` (so `[opened]` marks work) and one `↳ why` line per reason. The status line fits the progress queries to its width: the newest stay, older ones fold into `+N`.
8. **Remote hosts**: `main` loads `[[hosts]]` from `config.toml` / `CCS_HOSTS` (`remote::config::load_hosts`) and builds `App::new(paths).with_remote_hosts(hosts)`; `App::new` itself never reads the config, so tests stay local. Each host gets a `HostClient` running `ssh -o BatchMode=yes … <host> ccs serve --stdio` (own process group). The client is polled from `App::tick` → `poll_hosts` → `handle_remote_event`; `Ready` triggers a `recent` request and, when the shown query matches the input, a `search` for the current `search_seq`. `start_search` sends the search to every allowed `Ready` host; replies are merged by `seq`: the first answer for a new seq (local or remote) resets the cursor via `begin_results_for`, later ones merge into `all_groups` (`rebuild_all_groups`, cursor anchored by `(host, session_id)`). Remote groups carry `host`, `project_label`, `message_count`, automation and `on_latest_chain` flags from the server; anything that reads a session file goes through `SessionRef::local_path()` and skips remote rows. `reset_search_state` / `suspend_for_resume` cancel remote searches (cancelled ids are dropped by the client). Remote recent rows live in `RecentState.remote` per host and are merged after the local filter (`merge_remote`); Ctrl+A re-asks hosts with `project` = cwd relative to `$HOME`. Previews and trees of remote sessions come through `show` / `tree` requests started from `tick` (`request_preview_if_needed`), never from rendering. Enter on a remote session runs `ssh -t <host> ccs resume --file … [--uuid …]` as a child (`resume::launcher::resume_remote_child`; exit 255 → connection error).

### Dual format support

The tool handles two session formats with different field names:
- **Claude Code CLI** (`~/.claude/projects/`): `sessionId`, `timestamp`, has `branch`/`gitBranch`
- **Claude Desktop** (`~/Library/Application Support/Claude/local-agent-mode-sessions/` on macOS, `~/.config/Claude/local-agent-mode-sessions/` on Linux): `session_id`, `_audit_timestamp`, no branch info

The `SessionSource` enum in `session/mod.rs` drives format-specific parsing throughout. The `SessionRecord` enum in `session/record.rs` provides unified JSONL line parsing across both formats.

## Testing

- **Unit tests**: Inline `#[cfg(test)]` modules in source files
- **Integration tests**: `tests/` directory using `assert_cmd` for binary invocation
- **Fixtures**: `tests/fixtures/*.jsonl` — representative session files (linear, branched, compaction, ANSI, desktop audit)
- Error type convention: `Result<T, String>` (no custom error type)

## Environment

- `CCFS_SEARCH_PATH` — override default search paths (see `lib.rs:get_search_paths()`)
- `CLAUDE_CONFIG_DIR` — override `~/.claude` as Claude config root (matches Claude Code's own env var)
- `CCS_POPUP_WIDTH` — override overlay popup width in launch-ccs.sh (default `90%`)
- `CCS_POPUP_HEIGHT` — override overlay popup height in launch-ccs.sh (default `90%`)
- `XDG_CONFIG_HOME` — location of `ccs/config.toml` (default `~/.config`); `[agent_search]` sets the Ctrl+G agent's `model` and `effort`; `[[hosts]]` (`name`, optional `ccs_path`) lists remote hosts
- `CCS_HOSTS` — comma-separated remote hosts, overriding `[[hosts]]` in `config.toml`; empty disables remote hosts. With `CCFS_SEARCH_PATH` set, remote hosts are off unless `CCS_HOSTS` is set
- `CCS_SSH` — program used instead of `ssh` (tests point it at `tests/fixtures/fake-ssh.sh`)

## Release

Uses **cargo-dist** for multi-platform builds triggered by version tags. Homebrew tap at `materkey/homebrew-ccs`. Crates.io publish runs from `.github/workflows/publish-crates.yml` on the `release: published` event (needs repo secret `CARGO_REGISTRY_TOKEN`). See `.claude/skills/release-ccs/SKILL.md` for the full flow.
