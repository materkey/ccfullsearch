# Ctrl+G Agent Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Ctrl+G re-ranking with a `claude -p` agent that finds sessions across the whole history by a natural-language description.

**Architecture:** A new `src/agent_search.rs` spawns `claude -p` (lean flags, haiku by default) restricted to `<ccs> search` / `<ccs> show`, streams `stream-json`, reports the agent's search queries as progress and returns up to 5 resolved sessions. Model and effort come from a new `~/.config/ccs/config.toml` (`src/config.rs`). The TUI shows the hits as a separate view in AI mode; the old rank code (`src/ai.rs`, separator, snapshots) is deleted.

**Tech Stack:** Rust, ratatui, crossterm, serde/serde_json, `toml` (new), regex, std threads + mpsc.

**Spec:** `docs/superpowers/specs/2026-10-22-ctrl-g-agent-search-design.md`

## Global Constraints

- Agent command line (exact): `claude -p <query> --model <model> [--effort <effort>] --system-prompt <prompt> --tools Bash --allowedTools "Bash(<ccs> search:*)" "Bash(<ccs> show:*)" --strict-mcp-config --setting-sources "" --disable-slash-commands --no-session-persistence --output-format stream-json --verbose`, stdin = null.
- `<ccs>` = `agent_search::resolve_ccs_path()`, resolved once in `App::new` and stored in `App.ccs_path` (not per submit): `current_exe()`, and on Linux, if that path ends with `" (deleted)"` (binary replaced in place by `cargo install` / `ccs update`), `/proc/<std::process::id()>/exe`, which still execs the running version. The same path appears in the system prompt and the allowlist. On macOS an in-place upgrade still makes the agent run the new binary (no path to the old inode exists); accepted.
- Agent cwd = `std::env::temp_dir().join("ccs-agent-search")`, created before spawn.
- `--bare` is NOT used (subscription auth fails under it).
- Default model `claude-haiku-5-5`; no `--effort` unless configured.
- Config path: `$XDG_CONFIG_HOME/ccs/config.toml`, else `~/.config/ccs/config.toml` (macOS too). Read on every search start. Missing file = defaults. Unknown top-level tables are ignored (room for future sections). Parse error / unknown key inside `[agent_search]` / bad effort → status-bar error `config <path>: <error>`, agent not started.
- Effort values: `low | medium | high | xhigh | max`.
- Agent timeout 120 s → kill, error `agent timed out`.
- Max 5 hits; each has `session_id`, `file_path`, `why`.
- Resume from a hit always at the latest point: `ResumeTarget { uuid: None, .. }`, `PickedSession { message_uuid: None, .. }`.
- Error type convention: `Result<T, String>`.
- Commits: imperative subject, no ticket prefix, no attribution lines.

## Review Focus

1. `claude` missing from PATH → status-bar error, AI mode stays usable, no thread spawned (test in Task 3).
2. Agent reports a hit whose file is gone, or an Opencode `"<db>#<id>"` path → the hit is dropped, others still shown; all dropped → "nothing found" message (test in Task 2).
3. Esc (cancel, stay in AI mode) / Ctrl+G (exit) / query edit / resume while the agent runs → child killed synchronously by `cancel()`, a late `Done` never lands on screen; reader dying without `Done` → "agent stopped unexpectedly" (tests in Task 2 and Task 3).
4. Agent finishes with `is_error: true` (e.g. "Not logged in"), an error subtype (`error_max_turns`, `error_during_execution` with `errors`), or non-JSON prose → that text shown as the error, not a crash or empty list; exit without any `result` → stderr plus the last 3 stdout lines (test in Task 2).
5. Query containing quotes, `$`, or newlines → passed verbatim as one argv element, never through a shell (test in Task 2).
6. `ccs` upgraded in place while the TUI runs → on Linux the agent still runs the old binary via `/proc/<pid>/exe` (test of the pure helper in Task 2).
7. In the results state Ctrl+B / Tab / Ctrl+V never act on the hidden list; Ctrl+B while searching is a no-op (tests in Task 3).

---

### Task 1: Config file for agent search

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `pub mod config;`), `Cargo.toml` (add `toml = "0.8"`)
- Modify: `README.md` (new "Configuration" section after "AI ranking")

**Interfaces:**
- Produces:
  - `pub const DEFAULT_AGENT_MODEL: &str = "claude-haiku-5-5";`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)] #[serde(rename_all = "lowercase")] pub enum Effort { Low, Medium, High, Xhigh, Max }` with `pub fn as_str(self) -> &'static str`
  - `#[derive(Debug, Clone, PartialEq, Eq, Deserialize)] #[serde(deny_unknown_fields, default)] pub struct AgentSearchConfig { pub model: String, pub effort: Option<Effort> }` — `Default` gives `DEFAULT_AGENT_MODEL`, `None`
  - `#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)] #[serde(default)] pub struct Config { pub agent_search: AgentSearchConfig }` — no `deny_unknown_fields` on the root, so tables added by newer ccs versions do not break older ones
  - `pub fn config_path(xdg_config_home: Option<String>, home: Option<PathBuf>) -> Option<PathBuf>` (pure, env passed in — same pattern as `build_search_paths` in `src/lib.rs`)
  - `pub fn load_config(path: &Path) -> Result<Config, String>`
  - `pub fn load_default_config() -> Result<Config, String>` (reads `XDG_CONFIG_HOME`, `dirs::home_dir()`; no path → `Ok(Config::default())`)

- [ ] **Step 1: Write failing tests in `src/config.rs` `#[cfg(test)] mod tests`**

```rust
#[test]
fn xdg_set_config_path_uses_xdg_dir() {
    assert_eq!(config_path(Some("/x".into()), Some("/h".into())), Some(PathBuf::from("/x/ccs/config.toml")));
}
#[test]
fn xdg_empty_config_path_falls_back_to_home_dot_config() {
    assert_eq!(config_path(Some(String::new()), Some("/h".into())), Some(PathBuf::from("/h/.config/ccs/config.toml")));
}
#[test]
fn no_xdg_no_home_config_path_is_none() { assert_eq!(config_path(None, None), None); }
#[test]
fn missing_file_load_config_returns_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = load_config(&dir.path().join("config.toml")).unwrap();
    assert_eq!(cfg.agent_search.model, "claude-haiku-5-5");
    assert_eq!(cfg.agent_search.effort, None);
}
#[test]
fn model_and_effort_set_load_config_reads_both() {
    // file: [agent_search]\nmodel = "claude-sonnet-5-5"\neffort = "high"\n
    // assert model == "claude-sonnet-5-5", effort == Some(Effort::High), Effort::High.as_str() == "high"
}
#[test]
fn only_effort_set_load_config_keeps_default_model() { /* effort = "low" → model == DEFAULT_AGENT_MODEL */ }
#[test]
fn unknown_effort_load_config_errors_with_path() {
    // effort = "turbo" → Err(e) where e.starts_with(&format!("config {}:", path.display()))
}
#[test]
fn unknown_key_load_config_errors() { /* [agent_search]\nmodle = "x" → Err */ }
#[test]
fn unknown_top_level_table_load_config_ignores_it() { /* [future]\nx = 1\n[agent_search]\neffort = "low" → Ok, effort == Some(Effort::Low) */ }
#[test]
fn broken_toml_load_config_errors() { /* "[agent_search" → Err */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test config::tests`
Expected: compile error — module `config` / `load_config` not defined.

- [ ] **Step 3: Implement `src/config.rs`, add `pub mod config;` to `src/lib.rs`, add `toml = "0.8"` to `[dependencies]`**

`load_config`: `NotFound` → `Ok(Config::default())`; other read errors and `toml::from_str` errors → `Err(format!("config {}: {}", path.display(), e))`. `config_path`: empty `XDG_CONFIG_HOME` is treated as unset.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test config::tests`
Expected: 10 passed.

- [ ] **Step 5: Add README "Configuration" section**

Path rule, the TOML example from the spec ("Файл конфигурации"), defaults, that edits apply on the next search.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no output, exit 0.

```bash
git add src/config.rs
git commit -m "Read agent search model and effort from ~/.config/ccs/config.toml" -- src/config.rs src/lib.rs Cargo.toml Cargo.lock README.md
```

---

### Task 2: Agent runner

**Files:**
- Create: `src/agent_search.rs`
- Modify: `src/lib.rs` (add `pub mod agent_search;`)

The old `src/ai.rs` stays untouched in this task (deleted in Task 3), so the crate keeps building.

**Interfaces:**
- Consumes: `crate::config::AgentSearchConfig`, `crate::recent::{extract_summary, RecentSession}`.
- Produces:
  - `pub const AGENT_TIMEOUT: Duration = Duration::from_secs(120);`
  - `pub const MAX_HITS: usize = 5;`
  - `pub const AGENT_NO_MATCH_MSG: &str = "nothing found; edit the query and press Enter";`
  - `#[derive(Debug, Clone, PartialEq, Eq, Deserialize)] pub struct AgentHit { pub session_id: String, pub file_path: String, pub why: String }`
  - `#[derive(Debug, Clone)] pub struct FoundSession { pub session: RecentSession, pub why: String }`
  - `pub enum AgentEvent { Progress(Vec<String>), Done(Result<Vec<FoundSession>, String>) }` — `Progress` carries the full, de-duplicated list of search queries so far, in first-seen order.
  - `pub struct AgentSearchHandle { pub rx: Receiver<AgentEvent>, cancel: Arc<AtomicBool>, child: Option<Arc<Mutex<Child>>> }` with `pub fn cancel(&self)` — sets the flag, then locks `child` and calls `kill()` synchronously (errors ignored), so `Drop` has killed the agent before ccs can exit — and `impl Drop` calling `cancel()`. `child` is `None` when spawn failed.
  - `pub fn resolve_ccs_path() -> Result<PathBuf, String>` — `current_exe()` then `ccs_path_or_proc_exe(exe, std::process::id())` on every OS (only Linux produces the suffix; calling it unconditionally keeps the helper from being dead code on macOS); error text `cannot locate ccs binary: …`.
  - `pub fn system_prompt(ccs: &Path) -> String` — the spec's prompt with `<ccs>` replaced by `ccs.display()` and `--limit 10`.
  - `pub fn build_agent_args(query: &str, ccs: &Path, cfg: &AgentSearchConfig) -> Vec<String>`
  - `pub fn spawn_agent_search(query: String, claude: PathBuf, ccs: PathBuf, cfg: AgentSearchConfig) -> AgentSearchHandle`
  - private, tested: `enum StreamEvent { Searches(Vec<String>), Final(Result<String, String>) }`, `fn parse_stream_line(line: &str) -> Option<StreamEvent>`, `fn parse_hits(text: &str) -> Result<Vec<AgentHit>, String>`, `fn resolve_hits(hits: Vec<AgentHit>) -> Vec<FoundSession>`, `fn ccs_path_or_proc_exe(exe: PathBuf, pid: u32) -> PathBuf` (pure, not cfg-gated so it is tested everywhere: path string ends with `" (deleted)"` → `/proc/{pid}/exe`, else `exe`).

- [ ] **Step 1: Write failing pure tests**

```rust
#[test]
fn default_config_build_agent_args_has_lean_flags_and_no_effort() {
    let args = build_agent_args("q", Path::new("/bin/ccs"), &AgentSearchConfig::default());
    assert_eq!(&args[..2], ["-p", "q"]);
    for flag in ["--no-session-persistence", "--strict-mcp-config", "--disable-slash-commands", "--verbose"] {
        assert!(args.iter().any(|a| a == flag), "{flag}");
    }
    assert!(args.windows(2).any(|w| w == ["--model", "claude-haiku-5-5"]));
    assert!(args.windows(2).any(|w| w == ["--setting-sources", ""]));
    assert!(args.windows(2).any(|w| w == ["--output-format", "stream-json"]));
    assert!(args.windows(2).any(|w| w == ["--tools", "Bash"]));
    assert!(args.windows(3).any(|w| w == ["--allowedTools", "Bash(/bin/ccs search:*)", "Bash(/bin/ccs show:*)"]));
    assert!(!args.iter().any(|a| a == "--effort" || a == "--bare"));
}
#[test]
fn effort_configured_build_agent_args_passes_effort() { /* effort Some(High) → windows ["--effort","high"] */ }
#[test]
fn query_with_quotes_and_newline_build_agent_args_keeps_it_verbatim() {
    let q = "it's \"kotlin\" $HOME\nline2";
    assert_eq!(build_agent_args(q, Path::new("/bin/ccs"), &AgentSearchConfig::default())[1], q);
}
#[test]
fn ccs_path_system_prompt_names_absolute_binary_and_limit_10() {
    let p = system_prompt(Path::new("/opt/ccs"));
    assert!(p.contains("/opt/ccs search") && p.contains("--limit 10") && p.contains("/opt/ccs show"));
    assert!(p.contains("file_path contains '#'"));
}
#[test]
fn assistant_tool_use_line_parse_stream_line_extracts_search_queries() {
    // {"type":"assistant","message":{"content":[
    //   {"type":"tool_use","name":"Bash","input":{"command":"/opt/ccs search \"raycast kitty\" --limit 10"}},
    //   {"type":"tool_use","name":"Bash","input":{"command":"/opt/ccs search 'kotlin.*2\\.4' --regex --limit 10"}},
    //   {"type":"tool_use","name":"Bash","input":{"command":"/opt/ccs show /a.jsonl --line 3"}}]}}
    // → Some(StreamEvent::Searches(vec!["raycast kitty", "kotlin.*2\\.4"]))
}
#[test]
fn success_result_line_parse_stream_line_returns_final_text() {
    // {"type":"result","subtype":"success","is_error":false,"result":"[]"} → Final(Ok("[]"))
}
#[test]
fn error_result_line_parse_stream_line_returns_error_text() {
    // {"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"} → Final(Err(that text))
}
#[test]
fn error_subtype_result_line_parse_stream_line_returns_errors_joined() {
    // {"type":"result","subtype":"error_during_execution","is_error":true,"errors":["e1","e2"]} → Final(Err("e1; e2"))
}
#[test]
fn error_subtype_without_errors_parse_stream_line_returns_subtype() {
    // {"type":"result","subtype":"error_max_turns","is_error":true} → Final(Err("error_max_turns"))
}
#[test]
fn deleted_exe_path_ccs_path_or_proc_exe_uses_proc_pid_exe() {
    assert_eq!(ccs_path_or_proc_exe("/h/.cargo/bin/ccs (deleted)".into(), 42), PathBuf::from("/proc/42/exe"));
}
#[test]
fn live_exe_path_ccs_path_or_proc_exe_keeps_it() {
    assert_eq!(ccs_path_or_proc_exe("/h/.cargo/bin/ccs".into(), 42), PathBuf::from("/h/.cargo/bin/ccs"));
}
#[test]
fn system_and_tool_result_lines_parse_stream_line_ignored() { /* type system / user / rate_limit_event / non-JSON → None */ }
#[test]
fn prose_around_array_parse_hits_extracts_hits() {
    // "Here:\n[{\"session_id\":\"a\",\"file_path\":\"/p\",\"why\":\"w\"}]\nDone" → 1 hit
}
#[test]
fn more_than_five_parse_hits_truncates_to_max() { /* 7 objects → 5 */ }
#[test]
fn prose_only_parse_hits_errors_with_sample() { /* "I could not find it" → Err containing "no JSON array" and the text */ }
#[test]
fn missing_and_opencode_paths_resolve_hits_drops_them() {
    // hits: tests/fixtures/linear_session.jsonl (via env!("CARGO_MANIFEST_DIR")), "/nope.jsonl", "/db.sqlite#ses_1"
    // → 1 FoundSession, session.session_id == "sess-linear-001", why preserved
}
```

Search-query extraction (algorithm the tests do not fully pin): match each Bash `command` against `(?:^|/)ccs\s+search\s+("(?:[^"\\]|\\.)*"|'[^']*'|\S+)` (compile once, `LazyLock` or `OnceLock`), strip the surrounding quotes, unescape `\"` only. `parse_hits`: reuse the current bracket-matching approach from `src/ai.rs::parse_ai_response` + `format_parse_error` (move those two helpers here, adapted to `Vec<AgentHit>`). `resolve_hits`: drop paths containing `'#'`, and paths for which `extract_summary` returns `None`. `system_prompt`: the spec's text plus the line `Skip matches whose file_path contains '#' (Opencode sessions cannot be opened here).` `parse_stream_line`: every `type:"result"` line becomes `Final`; `Ok(result)` only when `is_error` is false and `result` is a string, otherwise `Err` with text from `result` (string), else `errors` (array of strings joined with `"; "`), else `subtype`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test agent_search::tests`
Expected: compile error — module `agent_search` not defined.

- [ ] **Step 3: Implement the pure functions above, add `pub mod agent_search;` to `src/lib.rs`**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test agent_search::tests`
Expected: all pass.

- [ ] **Step 5: Write failing process tests (`#[cfg(unix)]`, stub `claude` = shell script in a tempdir, mode 0o755)**

Stubs that must stay alive end with `exec sleep 30` as the last command, so the pid the runner kills is the process holding the stdout pipe (a plain `sleep` would survive the kill as an orphan and keep the reader blocked).

```rust
#[test]
fn stub_prints_search_then_result_spawn_emits_progress_then_hits() {
    // stub: printf '%s\n' '<assistant tool_use line with /x/ccs search "kitty">' '<result line with hit to linear_session.jsonl>'
    // recv order: Progress(vec!["kitty"]), Done(Ok(hits)) with hits.len()==1
}
#[test]
fn stub_returns_empty_array_spawn_reports_no_match() { /* Done(Err(AGENT_NO_MATCH_MSG)) */ }
#[test]
fn stub_exits_without_result_spawn_reports_stderr() {
    // stub: echo partial; echo boom >&2; exit 3 → Done(Err(e)) with e.contains("boom") && e.contains("partial")
}
#[test]
fn cancel_during_silent_stub_kills_child_and_sends_no_done() {
    // stub: exec sleep 30. cancel() after 200 ms; then rx.recv_timeout(2 s) must be Err(Disconnected) — no Done, sender dropped within 2 s
}
#[test]
fn dropping_handle_kills_child() { /* same stub; drop(handle) → stub pid gone within 2 s (stub writes $$ to a file first) */ }
```

The timeout path (120 s) is tested through a private `spawn_with_timeout(.., timeout: Duration)` that `spawn_agent_search` calls with `AGENT_TIMEOUT`:

```rust
#[test]
fn silent_stub_past_timeout_spawn_reports_timed_out() { /* timeout 300 ms, stub exec sleep 30 → Done(Err("agent timed out")) */ }
#[test]
fn result_then_silent_stub_past_timeout_spawn_reports_hits_not_timeout() {
    // timeout 300 ms; stub prints the result line with a hit to linear_session.jsonl, then exec sleep 30
    // → Done(Ok(hits)) with hits.len()==1 (a received Final wins over timed_out)
}
```

- [ ] **Step 6: Run to verify they fail**

Run: `cargo test agent_search::tests::stub agent_search::tests::cancel agent_search::tests::dropping agent_search::tests::silent agent_search::tests::result_then`
Expected: compile error — `spawn_agent_search` / `spawn_with_timeout` not defined.

- [ ] **Step 7: Implement `spawn_agent_search` / `spawn_with_timeout`**

Process model (silence of 10+ s between lines is normal, so a per-line cancel check is not enough):

```
create cwd dir; Command::new(claude).args(build_agent_args(..)).current_dir(cwd)
  .stdin(null).stdout(piped).stderr(piped).spawn()
  — on Unix, ETXTBSY (raw_os_error == libc::ETXTBSY: a parallel test thread forked while the stub
    was open for writing) is retried up to 5 times, 50 ms apart
  — other spawn error → send Done(Err("failed to run claude: …")) and return a handle with child: None
child → Arc<Mutex<Child>>, shared by the handle, the watchdog and the reader
stderr thread: read_to_string into Arc<Mutex<String>>
watchdog thread: every 100 ms; if cancel or elapsed > timeout → set flag `timed_out` (only for timeout), lock child, kill();
  exits when `reader_done` (AtomicBool) is set — by a drop guard in the reader, so a reader panic also stops it
reader thread (owns tx): BufReader::lines over stdout; keeps the last 3 raw lines; per line parse_stream_line:
  Searches(q) → extend de-duplicated list, send Progress(list.clone()) if it grew
  Final(r)    → remember r
on EOF: wait for exit by polling try_wait() every 50 ms under short locks
  (never hold the lock across a blocking wait, so cancel() never blocks);
  if cancel → return without sending
  Final(Ok(text)) → parse_hits → resolve_hits → empty → Err(AGENT_NO_MATCH_MSG) else Ok   (Final is checked before timed_out)
  Final(Err(msg)) → Done(Err(msg))
  timed_out → Done(Err("agent timed out"))
  no Final → Done(Err(format!("claude exited with {status}: {stderr}; last output: {last 3 stdout lines}")))
```

- [ ] **Step 8: Run all agent tests**

Run: `cargo test agent_search::tests`
Expected: all pass.

- [ ] **Step 9: Lint and commit**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings`
Expected: exit 0.

```bash
git add src/agent_search.rs
git commit -m "Add a claude -p agent that finds sessions across the whole history" -- src/agent_search.rs src/lib.rs
```

---

### Task 3: Switch Ctrl+G to agent search and drop re-ranking

**Files:**
- Create: `src/tui/render_ai.rs` (hits view, AI status span, AI hints)
- Modify: `src/tui/state.rs` (`AiState`, AI handlers, `poll_ai_result`, `tick`, remove rank code), `src/tui/search_mode.rs` (`on_toggle_regex`, `toggle_automation_filter`, `toggle_project_filter` lose `invalidate_ai_rank`), `src/tui/render_search.rs` (remove `ai_unranked_separator_at`, `build_ai_unranked_separator`, old `build_ai_hints`/`ai_status_span`; route to `render_ai`), `src/tui/mod.rs` (`mod render_ai;`), `src/tui/dispatch.rs` (`KeyContext.ai_searching`, `KeyAction::CancelAiSearch`), `src/agent_search.rs` (add `for_test`), `src/lib.rs` (remove `pub mod ai;`)
- Delete: `src/ai.rs`
- Modify: `CLAUDE.md` (module tree line for `ai.rs` → `agent_search.rs` + `config.rs`; "Key data flow" item 7 rewritten; drop the Ctrl+R/A/H `invalidate_ai_rank` sentence), `README.md` (line 42 "AI ranking" → "AI search"; "Main keys" `Ctrl+G` line; "AI ranking" section → "AI search" describing the flow, wait time, hints, latest-point resume)

**Interfaces:**
- Consumes: Task 1 `config::load_default_config`, Task 2 `agent_search::{spawn_agent_search, resolve_ccs_path, AgentSearchHandle, AgentEvent, FoundSession}`.
- Produces (`AiState`, replaces old fields `thinking`, `result_rx`, `ranked_count`, `original_recent_order`, `original_groups_order`):
  ```rust
  pub struct AiState {
      pub active: bool,
      pub query: InputState,
      pub(crate) search: Option<AgentSearchHandle>,   // Some = agent running
      pub progress: Vec<String>,                        // latest Progress list
      pub hits: Option<Vec<FoundSession>>,              // Some = results state
      pub hit_cursor: usize,
      pub error: Option<String>,
  }
  impl AiState { pub fn searching(&self) -> bool { self.search.is_some() } }
  ```
  - `App.ccs_path: Result<PathBuf, String>` — set once in `App::new` from `agent_search::resolve_ccs_path()`.
  - `fn start_agent_search(&mut self, query: String, claude: Result<PathBuf, String>)` — the testable seam (tests pass `Err(..)` or a stub path); `submit_ai_query` resolves `which::which("claude")`, loads config, then calls it; the ccs path comes from `self.ccs_path` (`Err` → `ai.error`). It first resets `progress` (empty), `error` (`None`), `hits` (`None`) and `hit_cursor` (0), then either sets `error` (claude / ccs path error) or installs the handle.
  - `src/agent_search.rs` (Produces, test-only): `#[cfg(test)] pub(crate) fn AgentSearchHandle::for_test(rx: Receiver<AgentEvent>) -> (AgentSearchHandle, Arc<AtomicBool>)` with `child: None`, for the state tests. Added here, not in Task 2, so Task 2's commit has no unused test code under `clippy -D warnings`.
  - `src/tui/dispatch.rs`: `KeyContext { .., pub ai_searching: bool }` (filled from `self.ai.searching()`; existing `KeyContext` literals in dispatch tests get `ai_searching: false`), `KeyAction::CancelAiSearch` — `classify_search_esc` returns it when `ai_mode && ai_searching`; Ctrl+G and Ctrl+C on an empty query keep `ExitAiMode`.

State machine (spec "Состояния режима Ctrl+G"):

| state | Enter | Esc | Up/Down | Right | Ctrl+B | Tab/Ctrl+V | char/Backspace/Delete/word-delete/Ctrl+C clear |
|---|---|---|---|---|---|---|---|
| input (`!searching && hits.is_none()`) | submit | exit mode | list (unchanged behaviour) | query cursor | list tree (unchanged) | unchanged | edit query |
| searching | no-op | `CancelAiSearch` → input, query kept | no-op | query cursor | no-op | unchanged | edit query + cancel → input |
| results (`hits.is_some()`) | resume/pick hit | exit mode | move `hit_cursor` | tree for hit | tree for hit | no-op | edit query + clear hits → input |

Ctrl+G and Ctrl+C on an empty query are `ExitAiMode` in every state. Ctrl+B arrives as `EnterTreeMode` or `EnterTreeModeRecent` (`classify_ctrl_b` picks by list context); `handle_ai_action` catches both, plus `Tab` and `TogglePreview`, before they reach the list handlers. Every query edit clears `ai.error`, in every state. `exit_ai_mode` drops `search` (Drop cancels), clears hits/progress/error. `suspend_for_resume` sets `ai.search = None` (Drop kills the agent) and clears `ai.progress`; `ai.hits` and `hit_cursor` are kept for the return. Exiting the tree returns to the results view because `ai.hits` is untouched by tree mode. Remove `ai_rank_pinned` gating in `poll_recent_loads` / `handle_search_result` (lists are no longer reordered).

- [ ] **Step 1: Write failing state tests in `src/tui/state.rs` tests (replace the old rank tests)**

```rust
#[test] fn ai_mode_claude_missing_submit_shows_error_and_stays_in_input() {
    // app.ai.active = true; query "x"; app.start_agent_search("x".into(), Err("Claude binary not found in PATH".into()))
    // → error Some(..), !searching(), hits None, ai.active
}
#[test] fn searching_esc_cancels_and_keeps_query() {
    // install AgentSearchHandle from a test constructor (pub(crate) fn AgentSearchHandle::for_test(rx) -> (Self, Arc<AtomicBool>))
    // handle_action(CancelAiSearch) → !searching(), ai.active, query text kept, cancel flag true
}
#[test] fn searching_ctrl_g_exits_ai_mode_and_cancels() { /* handle_action(ExitAiMode) → !ai.active, !searching(), cancel flag true */ }
#[test] fn searching_ctrl_b_does_nothing() { /* EnterTreeModeRecent and EnterTreeMode → !tree_mode, still searching */ }
#[test] fn searching_when_resume_taken_then_agent_cancelled_and_progress_cleared() {
    // handle installed, progress ["x"], outcome = Resume(..); take_outcome() → !searching(), progress empty, cancel flag true
}
#[test] fn restart_after_error_clears_error_and_progress() {
    // error Some("old"), progress ["old"], hits Some(1 hit); start_agent_search("q", Ok("/nonexistent/claude".into()))
    // → error None, progress empty, hits None, hit_cursor 0, searching()
}
#[test] fn edit_after_error_clears_error() { /* error Some("Not logged in"), InputChar('a') → error None */ }
#[test] fn tick_disconnect_without_done_shows_stopped_unexpectedly() {
    // handle installed, sender dropped without Done; tick() → !searching(), error == Some("agent stopped unexpectedly")
}
#[test] fn searching_query_edit_cancels_search() { /* InputChar('a') → !searching(), cancel flag true */ }
#[test] fn tick_progress_event_updates_progress() { /* send Progress(vec!["kitty"]) → ai.progress == ["kitty"], still searching */ }
#[test] fn tick_done_ok_installs_hits_and_resets_cursor() { /* Done(Ok(2 hits)) → hits.len()==2, hit_cursor 0, !searching() */ }
#[test] fn tick_done_err_shows_error_without_hits() { /* Done(Err("Not logged in")) → error == "Not logged in", hits None */ }
#[test] fn results_down_up_move_hit_cursor_within_bounds() { /* 2 hits: Down, Down → 1; Up, Up → 0 */ }
#[test] fn results_enter_resumes_hit_at_latest_point() {
    // outcome == AppOutcome::Resume(ResumeTarget { session_id, file_path, source, uuid: None }), should_quit
}
#[test] fn results_enter_in_picker_mode_picks_hit() { /* picker_mode → AppOutcome::Pick(.., message_uuid: None) */ }
#[test] fn results_right_opens_tree_for_hit_and_exit_tree_returns_to_results() {
    // Right → tree_mode, then exit_tree_mode() → !tree_mode, hits still Some, ai.active
}
#[test] fn results_ctrl_b_opens_tree_for_hit() {
    // recent list non-empty, hit_cursor 1; EnterTreeModeRecent → tree_mode on hits[1].session.file_path, not recent.cursor's session
}
#[test] fn results_tab_and_ctrl_v_do_nothing() { /* search.groups non-empty; Tab, TogglePreview → !preview_mode, hits still Some */ }
#[test] fn results_char_clears_hits_and_returns_to_input() { /* InputChar('z') → hits None, query ends with 'z' */ }
#[test] fn results_esc_exits_ai_mode_and_keeps_recent_list_order() {
    // recent.filtered order before == after; ai.active false; hits None
}
#[test] fn toggles_in_ai_mode_do_not_clear_hits() { /* Ctrl+R/Ctrl+A/Ctrl+H with hits Some → hits still Some */ }
```

Rewrite the existing `search_results_with_filters_when_returning_from_resume_then_state_is_kept`: instead of `ai.ranked_count = Some(1)`, set `ai.hits = Some(2 hits)`, `ai.hit_cursor = 1`; after `take_outcome()` → `return_from_resume(Some("s2"))` assert `ai.hits` still has 2 entries and `ai.hit_cursor == 1`.

Dispatch tests in `src/tui/dispatch.rs`:

```rust
#[test] fn ai_searching_esc_classify_key_returns_cancel_ai_search() { /* ai_mode, ai_searching → CancelAiSearch */ }
#[test] fn ai_searching_ctrl_g_classify_key_returns_exit_ai_mode() { /* ai_mode, ai_searching, Ctrl+G → ExitAiMode */ }
#[test] fn ai_not_searching_esc_classify_key_returns_exit_ai_mode() { /* ai_mode, !ai_searching → ExitAiMode */ }
```

- [ ] **Step 2: Write failing render tests in `src/tui/render_ai.rs` tests**

```rust
#[test] fn hits_render_shows_header_and_why_line_per_hit() {
    // TestBackend 100x20, 2 hits → buffer contains each session summary and "↳ <why>"
}
#[test] fn opened_hit_render_shows_opened_mark() { /* opened_sessions contains hits[0].session.session_id → hit 0's row contains "[opened]", hit 1's does not */ }
#[test] fn searching_status_lists_progress_queries() { /* progress ["kitty","котлин"] → status contains "searching: \"kitty\", \"котлин\"" */ }
#[test] fn searching_hints_show_esc_cancel() { /* build_ai_hints(&view) contains "[Esc] Cancel" */ }
#[test] fn results_hints_show_resume_tree_back() { /* contains "Resume", "Tree", "Back" */ }
#[test] fn input_hints_show_search() { /* no hits, not searching → "[Enter] Search" */ }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test tui::`
Expected: compile errors on the new `AiState` fields / `render_ai` module.

- [ ] **Step 4: Implement**

- `AiState` as in Interfaces; `handle_ai_action` / `on_ai_enter` / `handle_ai_query_edit` / `handle_ai_cursor_move` follow the state table (Up/Down in results handled before falling through to list navigation).
- `poll_ai_result` drains all pending events per tick (`try_recv` loop); `Done` sets `search = None`; channel `Disconnected` without `Done` (the handle is still installed, so the TUI did not cancel it) sets `search = None` and `ai.error = Some("agent stopped unexpectedly")`.
- `submit_ai_query`: empty query or `searching()` → return; `load_default_config()` error → `ai.error`, return.
- Delete `src/ai.rs`, `rank_items`, `apply_ai_rank_to_*`, `ai_rank_candidates`, `invalidate_ai_rank`, `ai_rank_pinned`, separator code and their tests; keep `AppView` usage.
- `render`: when `app.ai.active && app.ai.hits.is_some()` and not preview → `render_ai::render_ai_hits(frame, app, list_area)`. Make `SessionHeaderParts` (with its fields) and `render_session_header_line` `pub(crate)` in `render_search.rs`, and build each hit's header line with them as the recent list does, with `opened: app.opened_sessions.contains(&hit.session.session_id)` (`build_recent_session_header_text` is `#[cfg(test)]` and hard-codes `opened: false`, so it is not used); second line `↳ {why}` truncated with `truncate_to_width`.
- Update `CLAUDE.md` and `README.md` as listed in Files.

- [ ] **Step 5: Run the full suite**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: exit 0, no failures.

- [ ] **Step 6: Manual check (author's machine; development check, not a release gate)**

```bash
ls ~/.claude/projects/*/ | wc -l > /tmp/ccs-before-$$.txt   # count before
cargo run
# Ctrl+G, «где я планировал добавить в ccs ранжирование через модель решений от typesafe», Enter
#   → status shows searching: "…" while running; first hit is session 505ac6be…
# Esc, Ctrl+G, «расширение raycast чтобы прыгать во вкладку kitty», Enter → first hit 8d95fc28…
# Down/Up move, → opens tree, Esc returns to hits, Esc returns to the untouched recent list
# Ctrl+G, query, Enter, Esc while searching → status clears, `pgrep -f "claude -p"` shows nothing
find ~/.claude/projects -name '*.jsonl' -newer /tmp/ccs-before-$$.txt | grep -v subagents   # expected: no new files from the agent
```

Also: `printf '[agent_search]\neffort = "turbo"\n' > ~/.config/ccs/config.toml` → Ctrl+G Enter shows `config …: unknown variant`; remove the file afterwards (only if it did not exist before).

- [ ] **Step 7: Commit**

```bash
git add src/tui/render_ai.rs
git commit -m "Turn Ctrl+G into an agent search across the whole session history" -- src/tui/render_ai.rs src/tui/state.rs src/tui/search_mode.rs src/tui/render_search.rs src/tui/dispatch.rs src/tui/mod.rs src/agent_search.rs src/lib.rs src/ai.rs CLAUDE.md README.md
```
