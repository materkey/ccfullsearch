# Remote Hosts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ccs` on the Mac searches, lists, previews and resumes sessions on remote SSH hosts (Z270XX) next to local ones, through one persistent `ccs serve --stdio` channel per host.

**Architecture:** A new non-TUI module `src/remote/` owns the host config, the ssh command builder, the wire format, the server-side compute (`serve`) and the host client. Results carry a `Host`; every read of a session file goes through `SessionRef::local_path()`, which is `None` for remote rows, so remote data comes only from the server. The TUI merges per-source buffers by the shared `search_seq`; CLI and resume use the same client and builder.

**Tech Stack:** Rust 2021, ratatui/crossterm, clap, serde/serde_json, std threads + mpsc, `rg`, OpenSSH client. No new crates.

**Spec:** `docs/superpowers/specs/2026-10-22-remote-hosts-design.md` (Russian). Read it before every task; this plan cites its sections by heading.

## Global Constraints

- Error type: `Result<T, String>`, no custom error types (CLAUDE.md).
- Rendering takes `&AppView` and never mutates or does I/O; requests start on cursor move or in `tick()` (CLAUDE.md, spec «Отображение»).
- CI: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`; CRAP gate fails any new function with CRAP > 15 and any new duplicate pair (`.cargo-crap.toml`); erosion gate in `.github/scripts/scb-report.py`. Keep functions small and covered.
- Test names follow `subject_when_condition_then_result` (existing style, e.g. `failed_launch_when_returning_from_resume_then_no_session_is_marked`).
- `PROTOCOL_VERSION = 1`.
- Hello: `{"type":"hello","protocol":1,"ccs_version":"<CARGO_PKG_VERSION>"}`.
- Serve ssh options, exact order: `-o BatchMode=yes -o ConnectTimeout=3 -o ServerAliveInterval=5 -o ServerAliveCountMax=2 -o Compression=yes <name> <ccs_path> serve --stdio`.
- Resume ssh: same options without `BatchMode=yes`, plus `-t` first; remote command is one shell-quoted string `<ccs_path> resume --file <path> [--uuid <uuid>]`.
- Timeouts (all «моё решение» in spec, injectable via `ClientTimeouts`): hello 3 s, request 10 s, retry after Down 60 s; CLI wait 5 s for `search`, 10 s for `list`.
- Config: `$XDG_CONFIG_HOME/ccs/hosts.json`, default `~/.config/ccs/hosts.json` on every OS; `{"hosts":[{"name":"Z270XX","ccs_path":"ccs"}]}`, `ccs_path` optional (default `"ccs"`).
- Env: `CCS_HOSTS` (comma list, empty = no hosts) overrides the file; `CCS_SSH` replaces the `ssh` program; if `CCFS_SEARCH_PATH` is set and `CCS_HOSTS` is not, there are no remote hosts.
- `ccs serve` never reads the hosts config and never installs the TUI panic hook.
- Remote labels in JSON: `"host": "local"` or the host name.
- Exit classification before hello: 255 → Down; 127 or `command not found` in stderr → Down with text `ccs не найден`; other non-zero → `Incompatible { version: None }`; hello with other protocol → `Incompatible { version: Some(v) }`.
- Search response budget: `search "error"` from Z270XX ≈ 2 MB before compression.
- UI copy (Russian, verbatim): `недоступен`, `обнови ccs`, `ccs не найден`, `не удалось подключиться`.

## Review Focus

- A path or uuid with spaces, quotes or `$` in a remote resume — expected: the remote shell receives it intact (Task 4 test `shell_quote_when_value_has_quote_and_space_then_roundtrips_through_sh`).
- Remote ccs prints a non-JSON line before hello (rc file noise) — expected: skipped, not fatal (Task 8 test `client_when_noise_precedes_hello_then_ready`).
- Ctrl+C or backspace-to-empty while a remote search is in flight — expected: recent list shows, late reply dropped (Task 9 test `remote_reply_when_input_cleared_then_dropped`).
- Same session id on Mac and Z270XX — expected: two rows, `[opened]` mark only on the one that ran (Task 2 test `opened_mark_when_same_session_on_two_hosts_then_only_resumed_host_marked`).
- Host goes away mid-response (half line, EOF) — expected: host Down with reason, pending requests fail, local results untouched (Task 8 test `client_when_eof_mid_line_then_down_and_pending_failed`).

---

### Task 1: Shared project-matching helpers

**Files:**
- Create: `src/project_match.rs`
- Modify: `src/lib.rs` (add `pub(crate) mod project_match;`), `src/tui/state.rs:221-234` (remove the two fns, import them)
- Test: inline `#[cfg(test)]` in `src/project_match.rs`; move `test_path_is_within_project_rejects_sibling_prefixes` there

**Interfaces:**
- Produces:
  - `pub(crate) fn normalize_path_for_prefix_check(path: &str) -> String` (moved verbatim)
  - `pub(crate) fn path_is_within_project(file_path: &str, project_path: &str) -> bool` (moved verbatim)
  - `pub(crate) fn home_relative(path: &str, home: &str) -> String` — strip `home` + `/` by components; return the path unchanged when outside `home`; `home` itself → `""`
  - `pub(crate) fn remote_project_matches(provider: SessionProvider, session_cwd: &str, project: &str, home: &str) -> bool` — `rel = home_relative(session_cwd, home)`; Claude: `normalize(rel) == normalize(project)`; Codex/Opencode: `path_is_within_project(rel, project)`

- [ ] **Step 1: Write failing tests**
  - `home_relative_when_inside_home_then_strips_home` — `("/home/m/projects/x", "/home/m") == "projects/x"`
  - `home_relative_when_sibling_prefix_then_unchanged` — `("/home/mx/a", "/home/m") == "/home/mx/a"`
  - `remote_project_matches_when_claude_in_subdir_then_false` — Claude, cwd `/home/m/projects/x/sub`, project `projects/x` → false
  - `remote_project_matches_when_codex_in_subdir_then_true` — Codex, same → true
  - `remote_project_matches_when_sibling_name_then_false` — Codex, cwd `/home/m/projects/x-old`, project `projects/x` → false
  - `remote_project_matches_when_outside_home_then_exact_abs` — cwd `/srv/p`, project `/srv/p` → true
- [ ] **Step 2:** `cargo test project_match` → FAIL (module missing)
- [ ] **Step 3:** Create the module, move the two fns, implement the two new ones; `tui/state.rs` uses `crate::project_match::{...}`.
- [ ] **Step 4:** `cargo test` → all PASS; `cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] **Step 5:** `git commit -m "Move project path matching into a shared module"`

---

### Task 2: `Host` and `SessionRef` on every result, caches rekeyed

No behaviour change: every existing producer sets `Host::Local`.

**Files:**
- Create: `src/remote/mod.rs` (`pub mod` declarations grow in later tasks)
- Modify: `src/lib.rs` (`pub mod remote;`), `src/search/group.rs` (`SessionGroup`), `src/recent.rs` (`RecentSession`), `src/tui/state.rs` (`ResumeTarget`, `PickedSession`, `TuiOutcome::Resume`, `SearchState.latest_chains`, `automation_cache`, `message_count_rx`, `opened_sessions`, `return_from_resume`, `set_group_message_count`, dedup `seen` sets), `src/tui/search_mode.rs`, `src/tui/tree_mode.rs`, `src/tui/render_search.rs`, `src/main.rs` (`handle_outcome`, `apply_after_resume`), all struct literals in tests
- Test: inline in `src/remote/mod.rs` and `src/tui/state.rs`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)] pub enum Host { #[default] Local, Remote(String) }` with `pub fn is_local(&self) -> bool`, `pub fn label(&self) -> &str` (`"local"` or the name)
  - `#[derive(Debug, Clone, PartialEq, Eq, Hash)] pub struct SessionRef { pub host: Host, pub file_path: String }` with `pub fn new(host: Host, file_path: impl Into<String>) -> Self`, `pub fn local(file_path: impl Into<String>) -> Self`, `pub fn local_path(&self) -> Option<&str>` (Some only for `Host::Local`)
  - New fields: `SessionGroup.host: Host`, `SessionGroup.project_label: Option<String>` (server-provided label; `None` for local), `SessionGroup.matches_total: Option<usize>` (server count before the per-session cap; `None` for local; render «показано N из M» when `Some(m) > matches.len()`), `RipgrepMatch.content_truncated: bool` (`false` for local), `RecentSession.host: Host`, `ResumeTarget.host: Host`, `PickedSession.host: Host`, `TuiOutcome::Resume { host: Host, .. }`
  - `SearchState.latest_chains: HashMap<SessionRef, HashSet<String>>`; `App.automation_cache: HashMap<SessionRef, Option<String>>`; `message_count_rx: Option<Receiver<(SessionRef, usize, bool)>>`; `App.opened_sessions: HashSet<(Host, String)>`
  - `pub fn return_from_resume(&mut self, opened: Option<(&Host, &str)>)`
  - `fn group_ref(group: &SessionGroup) -> SessionRef` helper in `state.rs`
  - Rule for all later tasks: code that opens a session file calls `SessionRef::local_path()` and skips (or uses server data) on `None`.

- [ ] **Step 1: Write failing tests**
  - `session_ref_when_remote_then_no_local_path` — `SessionRef::new(Host::Remote("Z".into()), "/x").local_path() == None`
  - `host_label_when_local_then_local` — `Host::Local.label() == "local"`
  - `opened_mark_when_same_session_on_two_hosts_then_only_resumed_host_marked` — two groups with session_id `s1`, hosts Local and Remote("Z"); `return_from_resume(Some((&Host::Remote("Z".into()), "s1")))`; `opened_sessions` contains `(Remote("Z"), "s1")` and not `(Local, "s1")`
  - `message_count_when_same_path_on_two_hosts_then_only_matching_host_updated` — `set_group_message_count(&mut groups, &SessionRef::local("/p"), 5, false)` leaves the remote group's count `None`
- [ ] **Step 2:** `cargo test remote:: session_ref host_label opened_mark message_count_when` → FAIL
- [ ] **Step 3:** Add types and fields, rekey, thread `Host::Local` through producers and the `[opened]` render checks (`render_search.rs:815,1164`).
- [ ] **Step 4:** `cargo test` → all PASS (existing tests updated only for new fields); clippy clean
- [ ] **Step 5:** `git commit -m "Carry the host on every session result and key caches by it"`

---

### Task 3: Hosts config

**Files:**
- Create: `src/remote/config.rs`
- Modify: `src/remote/mod.rs`
- Test: inline

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub struct HostConfig { pub name: String, pub ccs_path: String }`
  - `pub fn config_path(xdg_config_home: Option<String>, home: Option<PathBuf>) -> Option<PathBuf>` → `<xdg or ~/.config>/ccs/hosts.json`
  - `pub fn resolve_hosts(ccs_hosts: Option<String>, search_path_overridden: bool, file_contents: Option<String>) -> Result<Vec<HostConfig>, String>` — pure; precedence: `ccs_hosts` set → parse comma list (trim, skip empty; `ccs_path` taken from the file entry with the same name if present, else `"ccs"`); else `search_path_overridden` → `[]`; else parse file; `None` → `[]`
  - `pub fn load_hosts() -> Result<Vec<HostConfig>, String>` — reads env `CCS_HOSTS`, `CCFS_SEARCH_PATH`, `XDG_CONFIG_HOME`, file; calls `resolve_hosts`

- [ ] **Step 1: Write failing tests**
  - `resolve_hosts_when_ccs_hosts_empty_then_none` — `Some("")` → `[]`
  - `resolve_hosts_when_search_path_overridden_without_ccs_hosts_then_none` — `(None, true, Some(file))` → `[]`
  - `resolve_hosts_when_file_has_host_then_default_ccs_path` — file `{"hosts":[{"name":"Z270XX"}]}` → `[HostConfig{name:"Z270XX", ccs_path:"ccs"}]`
  - `resolve_hosts_when_ccs_hosts_names_file_host_then_keeps_its_ccs_path` — env `"Z270XX"`, file with `ccs_path:"/opt/ccs"` → that path
  - `resolve_hosts_when_file_invalid_json_then_error` — `Err` containing `hosts.json`
  - `config_path_when_xdg_set_then_under_xdg` — `Some("/x")` → `/x/ccs/hosts.json`
- [ ] **Step 2:** `cargo test remote::config` → FAIL
- [ ] **Step 3:** Implement with serde `Deserialize` structs local to the module.
- [ ] **Step 4:** `cargo test remote::config` → PASS
- [ ] **Step 5:** `git commit -m "Load remote hosts from hosts.json and CCS_HOSTS"`

---

### Task 4: ssh command builder

**Files:**
- Create: `src/remote/ssh.rs`; `tests/fixtures/fake-ssh.sh` (executable)
- Test: inline

**Interfaces:**
- Consumes: `HostConfig` (Task 3)
- Produces:
  - `pub enum SshMode<'a> { Serve, Resume { file_path: &'a str, uuid: Option<&'a str> } }`
  - `pub fn ssh_args(host: &HostConfig, mode: &SshMode) -> Vec<String>` — exact args from Global Constraints
  - `pub fn ssh_program() -> String` — `CCS_SSH` or `"ssh"`
  - `pub fn ssh_command(host: &HostConfig, mode: &SshMode) -> std::process::Command`
  - `pub fn shell_quote(s: &str) -> String` — POSIX single-quote; `'` → `'\''`
  - `fake-ssh.sh`: drops `-t` and `-o <opt>` pairs and the host name, then `exec sh -c "$*"`. Used by Tasks 8, 13, 14 tests.

- [ ] **Step 1: Write failing tests**
  - `ssh_args_when_serve_then_batchmode_and_serve_stdio` — equals the exact vector for `HostConfig{name:"Z270XX", ccs_path:"ccs"}`
  - `ssh_args_when_resume_then_tty_without_batchmode` — first arg `-t`, no `BatchMode=yes`, last arg `"ccs resume --file '/a b/c.jsonl' --uuid 'u1'"`
  - `shell_quote_when_value_has_quote_and_space_then_roundtrips_through_sh` — `sh -c "printf %s <quoted>"` prints `it's a $x`
- [ ] **Step 2:** `cargo test remote::ssh` → FAIL
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test remote::ssh` → PASS
- [ ] **Step 5:** `git commit -m "Build ssh commands for remote serve and resume in one place"`

---

### Task 5: Wire format

**Files:**
- Create: `src/remote/wire.rs`; `tests/fixtures/wire_golden.json`
- Test: inline (golden test reads the fixture)

**Interfaces:**
- Produces (all `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`, no `#[serde(default)]`, no `host` field):
  - `pub const PROTOCOL_VERSION: u32 = 1;`
  - `WireHello { r#type: String /* "hello" */, protocol: u32, ccs_version: String }`
  - `WireRequest { id: u64, method: String, params: serde_json::Value }`; `WireCancel { method: String, params: WireCancelParams { id: u64 } }`
  - `WireResponse { id: u64, result: Option<serde_json::Value>, error: Option<String> }` (`skip_serializing_if = "Option::is_none"`)
  - `SearchParams { query: String, regex: bool, project: Option<String>, full_content: bool, limit: Option<usize> }`
  - `RecentParams { limit: usize, project: Option<String> }`; `ShowParams { file_path: String, line: Option<usize>, uuid: Option<String> }`; `TreeParams { file_path: String }`
  - `WireMatch { session_id, role, content, text_content, content_truncated: bool, timestamp: DateTime<Utc>, branch: Option<String>, file_path: Option<String>, line_number: usize, uuid: Option<String>, parent_uuid: Option<String>, on_latest_chain: bool }`
  - `WireGroup { session_id, file_path, project: String, automation: Option<String>, message_count: Option<usize>, message_count_compacted: bool, matches: Vec<WireMatch>, matches_total: usize }`
  - `WireSearch { groups: Vec<WireGroup>, truncated: bool }`
  - `WireRecent { session_id, file_path, project, timestamp, summary, automation, branch, message_count: Option<usize>, preview_role: String /* "user"|"assistant" */, cwd: Option<String> }`
  - `WireShow { content: String, text_content: String }`
  - `WireTreeRow { uuid, role, timestamp, content_preview, graph_symbols, is_on_latest_chain, is_branch_point, is_compaction }`; `WireTree { session_id, file_path, tip_uuid: Option<String>, branch_count: usize, rows: Vec<WireTreeRow> }`
  - Conversions: `impl WireGroup { pub fn into_group(self, host: Host) -> (SessionGroup, HashSet<String> /* latest-chain uuids from flags */) }`, `impl WireRecent { pub fn into_recent(self, host: Host) -> RecentSession }`, `impl From<&TreeRow> for WireTreeRow`, `impl WireTree { pub fn into_tree(self, host: Host) -> SessionTree }`.
  - In `src/tree/mod.rs` (this task): `SessionTree.host: Host` (Local in `from_file`), `remote_branch_count: Option<usize>`, `pub(crate) fn from_rows(host: Host, session_id: String, file_path: String, tip_uuid: Option<String>, branch_count: usize, rows: Vec<TreeRow>) -> Self` (empty `nodes`/`children`, `source = SessionSource::from_path(&file_path)`); `branch_count()` returns `remote_branch_count` when set.

- [ ] **Step 1: Write failing tests**
  - `wire_shapes_when_serialized_then_match_golden` — one fixed value of each struct serialized with `serde_json::to_value`, compared to `tests/fixtures/wire_golden.json`. Header comment in the test: changing this fixture requires bumping `PROTOCOL_VERSION`.
  - `wire_group_when_missing_field_then_deserialize_fails` — JSON without `matches_total` → `Err`
  - `wire_group_into_group_when_flags_set_then_latest_chain_has_flagged_uuids` — two matches, one `on_latest_chain: true` → set contains only its uuid; group `host == Remote("Z")`, `project_label == Some(project)`
- [ ] **Step 2:** `cargo test remote::wire` → FAIL
- [ ] **Step 3:** Implement structs and conversions; write the golden fixture from the first passing serialization and review it by eye.
- [ ] **Step 4:** `cargo test remote::wire` → PASS
- [ ] **Step 5:** `git commit -m "Define the remote wire format with a golden shape test"`

---

### Task 6: Server-side compute

**Files:**
- Create: `src/remote/server.rs`
- Modify: `src/session/mod.rs` (add `pub fn read_session_cwd(path: &str) -> Option<String>`: Codex → `read_codex_session_cwd`; Opencode → session `directory`; Claude → `extract_cwd` of the first record that has one, scanning ≤ 50 lines), `src/tui/state.rs` (move `read_opencode_session_cwd` next to it if needed, no copy)
- Test: inline, using `tests/fixtures/*.jsonl` via `CARGO_MANIFEST_DIR`

**Interfaces:**
- Consumes: Task 1 `remote_project_matches`, Task 5 wire types
- Produces:
  - `pub const SNIPPET_CONTEXT_CHARS: usize = 200; pub const MAX_MATCHES_PER_SESSION: usize = 50; pub const MAX_SESSIONS: usize = 300;` (tune in Task 16 against the 2 MB budget)
  - `pub fn search(params: &SearchParams, search_paths: &[String], home: &str, cancel: &Arc<AtomicBool>) -> Result<WireSearch, String>` — `search_multiple_paths` → `group_by_session` → per group (stop on cancel): project filter via `read_session_cwd` + `remote_project_matches`; `detect_session_automation`; `count_session_messages`; `build_chain_from_tip` → `on_latest_chain` per match; `project = extract_project_from_path`; snippets via `extract_context` unless `full_content` (then only the first `limit` matches by time are untrimmed); cap per session and sessions
  - `pub fn recent(params: &RecentParams, search_paths: &[String], home: &str) -> Vec<WireRecent>` — `collect_recent_sessions` + project filter
  - `pub fn show(params: &ShowParams) -> Result<WireShow, String>` — reuse the target-locating code of `cli_show` (split it into a pure fn in `cli.rs` returning the message; no copy)
  - `pub fn tree(params: &TreeParams) -> Result<WireTree, String>` — `SessionTree::from_file` → wire

- [ ] **Step 1: Write failing tests**
  - `server_search_when_query_matches_fixture_then_group_has_counts_and_project` — `linear_session.jsonl`, query from that fixture; one group, `message_count.is_some()`, `!project.is_empty()`, every match `content.chars().count() <= 2*200+query_len+6`
  - `server_search_when_branched_then_only_latest_chain_matches_flagged` — `branched_session.jsonl`; at least one `on_latest_chain == false`
  - `server_search_when_cancelled_then_err_cancelled` — cancel set before call → `Err("cancelled")`
  - `server_search_when_project_mismatch_then_no_groups` — `project: Some("projects/none")` → empty
  - `server_tree_when_branched_then_tip_and_branch_count` — `tip_uuid.is_some()`, `branch_count >= 1`
  - `server_show_when_uuid_given_then_full_text` — non-empty `content`
- [ ] **Step 2:** `cargo test remote::server` → FAIL
- [ ] **Step 3:** Implement; split `cli_show`'s locating logic so both callers share it.
- [ ] **Step 4:** `cargo test` → PASS
- [ ] **Step 5:** `git commit -m "Compute remote search, recent, show and tree results on the serving side"`

---

### Task 7: `ccs serve --stdio`

**Files:**
- Create: `src/remote/serve.rs`, `tests/serve_stdio.rs`
- Modify: `src/main.rs` (`Commands::Serve { #[arg(long)] stdio: bool }`, hidden from help; `install_panic_hook()` moves after `Cli::parse()` and is skipped for `Serve`)

**Interfaces:**
- Consumes: Task 6 server fns, Task 5 wire types
- Produces: `pub fn run_stdio<R: BufRead, W: Write + Send + 'static>(input: R, output: W, search_paths: Vec<String>, home: String) -> i32`
  - writes hello first; one thread per request; `HashMap<u64, Arc<AtomicBool>>` cancel registry; `cancel` sets the flag; handler wrapped in `std::panic::catch_unwind` → `{"id":N,"error":"panic: …"}`; cancel flag checked right before taking the output mutex; whole lines under one `Mutex<W>`; poisoned mutex → `into_inner`; unknown method → error response; EOF → set all flags, return 0

- [ ] **Step 1: Write failing tests** (`tests/serve_stdio.rs`, spawn `env!("CARGO_BIN_EXE_ccs")` with `CCFS_SEARCH_PATH=tests/fixtures`)
  - `serve_when_started_then_first_line_is_hello_with_protocol_1`
  - `serve_when_search_request_then_response_with_same_id_and_groups`
  - unit `serve_when_cancel_sent_before_reply_then_no_reply_for_that_id` in `serve.rs` — `run_stdio` built over an injected dispatch whose `search` handler blocks on a barrier until the cancel line is read; assert the output has no line with id 1
  - `serve_when_unknown_method_then_error_response`
  - `serve_when_stdin_closed_then_exits_zero`
  - `serve_when_panicking_handler_then_error_response_and_server_alive` — unit test in `serve.rs` with an injected handler that panics
- [ ] **Step 2:** `cargo test --test serve_stdio` → FAIL
- [ ] **Step 3:** Implement; dispatch table method → handler fn keeps CC low.
- [ ] **Step 4:** `cargo test --test serve_stdio && cargo test remote::serve` → PASS
- [ ] **Step 5:** `git commit -m "Add ccs serve --stdio speaking the remote protocol"`

---

### Task 8: Host client

**Files:**
- Create: `src/remote/client.rs`, `tests/remote_client.rs`

**Interfaces:**
- Consumes: Tasks 3–5, 7
- Produces:
  - `#[derive(Debug, Clone, PartialEq)] pub enum HostState { Connecting, Ready, Down { reason: String }, Incompatible { version: Option<String> } }` (keep `since: Instant` in the client, not in the enum, so the enum stays comparable)
  - `pub struct ClientTimeouts { pub hello: Duration, pub request: Duration, pub retry: Duration }` with `Default` = 3 s / 10 s / 60 s
  - `pub enum Request { Search(SearchParams), Recent(RecentParams), Show(ShowParams), Tree(TreeParams) }`
  - `pub enum Reply { Search(WireSearch), Recent(Vec<WireRecent>), Show(WireShow), Tree(WireTree) }`
  - `pub enum ClientEvent { State(HostState), Hello { ccs_version: String }, Reply { id: u64, result: Result<Reply, String> } }`
  - `pub trait HostLink: Send { fn name(&self) -> &str; fn state(&self) -> &HostState; fn send(&mut self, req: Request) -> Option<u64>; fn cancel(&mut self, id: u64); fn poll(&mut self, now: Instant); fn reconnect_now(&mut self); }` — implemented by `HostClient`; the TUI holds `Box<dyn HostLink>` so tests can use a recording fake
  - `pub struct HostClient` with `pub fn spawn(config: HostConfig, timeouts: ClientTimeouts, events: Sender<(String, ClientEvent)>) -> Self`, `pub fn name(&self) -> &str`, `pub fn state(&self) -> &HostState`, `pub fn send(&mut self, req: Request) -> Option<u64>` (None unless Ready), `pub fn cancel(&mut self, id: u64)` (removes from pending, writes cancel), `pub fn poll(&mut self, now: Instant)` (hello deadline, per-request timeouts → `Reply{result: Err("timeout")}` without changing state, retry after Down), `pub fn reconnect_now(&mut self)`, `Drop` closes stdin, kills and waits the child
  - `pub fn classify_exit_before_hello(code: Option<i32>, stderr_tail: &str) -> HostState` (pure; rules in Global Constraints)
  - ssh spawned via `ssh_command(.., SshMode::Serve)` with `process_group(0)`, stdin/stdout piped, stderr piped to a tail buffer (last 20 lines); reader thread parses lines, skips non-JSON lines before hello; after hello a non-JSON line or EOF → Down with stderr tail
  - Replies for ids not in pending are dropped.

- [ ] **Step 1: Write failing tests**
  - unit `classify_exit_before_hello_when_255_then_down`, `..._when_127_then_down_ccs_not_found`, `..._when_clap_exit_2_then_incompatible_without_version`
  - `tests/remote_client.rs` (`CCS_SSH=tests/fixtures/fake-ssh.sh`, `ccs_path = env!("CARGO_BIN_EXE_ccs")`, `CCFS_SEARCH_PATH=tests/fixtures`):
    - `client_when_serve_starts_then_ready_and_search_replies`
    - `client_when_noise_precedes_hello_then_ready` — `ccs_path` = a script printing `noise` then exec ccs
    - `client_when_program_silent_then_down_after_hello_deadline` — `ccs_path = "sleep 30"`, hello timeout 300 ms → `Down` within 1 s
    - `client_when_protocol_differs_then_incompatible_with_version` — script prints `{"type":"hello","protocol":99,"ccs_version":"9.9.9"}`
    - `client_when_old_ccs_then_incompatible_without_version` — script prints clap error to stderr, exits 2
    - `client_when_eof_mid_line_then_down_and_pending_failed` — script prints hello, reads a line, prints `{"id":1,` and exits
    - `client_when_request_times_out_then_only_that_request_fails` — request timeout 200 ms against a script that answers nothing after hello; state stays `Ready`
    - `client_when_cancelled_then_late_reply_dropped`
- [ ] **Step 2:** `cargo test --test remote_client && cargo test remote::client` → FAIL
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** same commands → PASS
- [ ] **Step 5:** `git commit -m "Add a per-host client with hello, timeouts and exit classification"`

---

### Task 9: TUI search over hosts

**Files:**
- Create: `src/tui/hosts_state.rs` (`HostsState`), add to `src/tui/mod.rs`
- Modify: `src/tui/state.rs` (`App.hosts: HostsState`, `start_search`, `handle_search_result`, `reset_search_state`, `suspend_for_resume`, `tick`, `apply_groups_filter`, `apply_recent_automation_to_groups`), `src/tui/search_mode.rs` (`on_right` skips `build_chain_from_tip` for remote), `src/tui/render_search.rs` (host label before project, host status spans, project from `group.project_label` for remote), `src/main.rs` (`init_app` calls `App::with_remote_hosts(load_hosts()?)`; config error → stderr warning, no hosts)
- Test: inline in `state.rs` / `hosts_state.rs`; `tests/render_snapshots.rs` one new snapshot

**Interfaces:**
- Consumes: Tasks 2, 5, 8
- Produces:
  - `pub fn with_remote_hosts(self, hosts: Vec<HostConfig>) -> Self` on `App` (spawns clients); `App::new` unchanged (no hosts)
  - `#[derive(Debug, Clone, PartialEq, Eq, Default)] pub enum HostFilter { #[default] All, Local, Remote, Only(String) }` with `pub fn allows(&self, host: &Host) -> bool` (defined here; Task 12 adds cycling)
  - `pub struct HostsState { links: Vec<Box<dyn HostLink>>, filter: HostFilter, events_rx: Receiver<(String, ClientEvent)>, events_tx: Sender<..>, pending: HashMap<(String, u64), PendingKind>, search_buffers: HashMap<String, (u64 /*seq*/, Vec<SessionGroup>)>, status: HashMap<String, HostStatus> }`
  - `pub(crate) enum PendingKind { Search { seq: u64 }, Recent, Show(SessionRef, ShowKey), Tree(SessionRef) }`
  - `pub(crate) fn handle_remote_event(&mut self, host: &str, event: ClientEvent)` on `App` (tests drive it directly)
  - `fn rebuild_all_groups(&mut self)` — local groups + every buffer whose seq == `search_seq`; sort by `latest_timestamp` desc; filters; cursor anchored by `(host, session_id)` (fallback: nearest index)
  - AI re-rank (`ai_rank_candidates`) passes no first-message context for remote sessions (spec «Связь с Ctrl+G»): call `collect_session_context` only when `local_path()` is `Some`.
  - Behaviour per spec «Поиск в TUI»: resets only on new seq; `reset_search_state` and `suspend_for_resume` cancel all pending remote searches and `search_seq += 1`; on `State(Ready)` send `Recent` and (input non-empty, filter allows) `Search` for current seq; `apply_groups_filter` applies `group_matches_project` only to `Host::Local` groups; message-count thread only over local refs; remote automation comes from the group, never `detect_session_automation`
  - Status text per host: `"{name}: {n}"`, `"{name}: …"`, `"{name}: недоступен"`, `"{name}: обнови ccs"`, `"{name}: ccs не найден"`; version note when `ccs_version` differs; a detail line with the Down reason under the status bar when any host is Down

- [ ] **Step 1: Write failing tests** (inline; build `App::new` and push `ClientEvent`s through `handle_remote_event`)
  - `remote_reply_when_same_seq_after_local_then_merged_and_cursor_kept` — local groups arrive, cursor on group 1, remote reply arrives → `groups.len()` grows, selected `(host, session_id)` unchanged
  - `remote_reply_when_stale_seq_then_dropped`
  - `remote_reply_when_input_cleared_then_dropped` — start search, `clear_input()`, deliver reply with old seq → `in_recent_sessions_mode()`
  - `remote_reply_when_suspended_for_resume_then_dropped`
  - `host_ready_when_query_pending_then_search_sent_for_current_seq` — `HostsState` built with a recording fake `HostLink`
  - `remote_group_when_project_filter_on_then_kept_without_disk_read`
  - `remote_group_when_message_counts_run_then_count_from_server_kept`
  - render snapshot `search_results_when_remote_group_then_host_label_and_status`
- [ ] **Step 2:** `cargo test tui::` → FAIL
- [ ] **Step 3:** Implement. Keep `is_searching()` local-only.
- [ ] **Step 4:** `cargo test` → PASS
- [ ] **Step 5:** `git commit -m "Merge remote search results into the picker"`

---

### Task 10: Recent sessions and Ctrl+A over hosts

**Files:**
- Modify: `src/tui/state.rs` (`RecentState.remote: HashMap<String, Vec<RecentSession>>`, `apply_filter`, `total_count`), `src/tui/search_mode.rs` (`toggle_project_filter` resends), `src/tui/hosts_state.rs`
- Test: inline

**Interfaces:**
- Consumes: Task 9 `HostsState`, Task 1 `home_relative`
- Produces:
  - `fn remote_project_param(&self) -> Option<String>` on `App` — `project_filter` on → `home_relative(current_cwd, $HOME)`; else `None`
  - `apply_filter` unions `remote` buffers after the local union, without `session_matches_project`; `seen` keyed by `(host, session_id)`; sorted by `sort_recent_sessions_desc`
  - `total_count` = existing local math + `remote.values().map(len).sum()`
  - Ctrl+A toggle → `Recent` to every Ready allowed host with the new `project`, and `Search` if input non-empty

- [ ] **Step 1: Write failing tests**
  - `recent_when_remote_buffer_and_project_filter_then_remote_rows_kept`
  - `recent_total_count_when_remote_rows_then_included`
  - `recent_when_same_session_id_on_two_hosts_then_two_rows`
  - `project_toggle_when_hosts_ready_then_recent_resent_with_home_relative_project`
  - `recent_project_label_when_remote_then_server_label_used` (pick output uses it too)
- [ ] **Step 2:** `cargo test tui::state` → FAIL
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` → PASS
- [ ] **Step 5:** `git commit -m "Show remote recent sessions and scope them by project"`

---

### Task 11: Remote preview and tree

**Files:**
- Modify: `src/tree/mod.rs` (`pub(crate) fn insert_cached_content(&self, uuid: &str, text: String)`; `get_full_content` answers only from `content_cache` when `host` is remote; the 64-entry cache clear keeps working for both; `from_rows` comes from Task 5), `src/tui/tree_mode.rs` (`enter_tree_mode_for_file` takes `SessionRef`; remote → `Request::Tree`), `src/tui/hosts_state.rs` (show pending set `HashSet<(SessionRef, ShowKey)>`, preview cache), `src/tui/search_mode.rs` / `tree_mode.rs` cursor moves call `request_preview_if_needed()`, `src/tui/render_search.rs` and `render_tree.rs` read only caches
- Test: inline

**Interfaces:**
- Produces:
  - `pub(crate) enum ShowKey { Line(usize), Uuid(String) }`
  - `fn request_preview_if_needed(&mut self)` on `App` — for the selected remote match with `content_truncated` (search) or selected remote tree row without cache: send `Show` once (pending set dedupes)
  - `SearchState.preview_cache: HashMap<(SessionRef, ShowKey), WireShow>`; renderer prefers it over the snippet

- [ ] **Step 1: Write failing tests**
  - `remote_tree_when_reply_arrives_then_tree_installed_with_branch_count`
  - `remote_tree_get_full_content_when_not_cached_then_none_without_disk`
  - `preview_when_cursor_moves_twice_on_same_match_then_one_show_sent`
  - `preview_when_show_reply_arrives_then_cached_and_full_text_rendered` (render via `AppView`, no request from render)
- [ ] **Step 2:** `cargo test tree:: tui::` → FAIL
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` → PASS
- [ ] **Step 5:** `git commit -m "Load remote previews and trees on demand"`

---

### Task 12: Ctrl+L host filter

**Files:**
- Modify: `src/tui/dispatch.rs` (`KeyAction::CycleHostFilter`, `KeyContext.has_remote_hosts`), `src/tui/state.rs` (`App.host_filter: HostFilter`, `handle_toggle_action`), `src/tui/hosts_state.rs`, `src/tui/render_search.rs` (status label, hint only when hosts exist)
- Test: inline in `dispatch.rs`, `state.rs`

**Interfaces:**
- Produces:
  - `impl HostFilter { pub fn next(&self, hosts: &[String]) -> HostFilter }` (type from Task 9): `All → Local → Remote → Only(h1) … → All`; `Only` steps only when `hosts.len() > 1`
  - Cycling: `invalidate_ai_rank()` when AI active; retry every Down host (`reconnect_now`); filter `Local` sends nothing remote; switching to a filter that allows a host sends the current query/recent to it; the view filter hides disallowed hosts' rows in groups and recent
  - Ctrl+L with no hosts → `KeyAction::None`

- [ ] **Step 1: Write failing tests**
  - `host_filter_next_when_one_host_then_all_local_remote_cycle`
  - `host_filter_next_when_two_hosts_then_includes_each_host`
  - `ctrl_l_when_no_remote_hosts_then_no_action`
  - `host_filter_when_local_then_remote_rows_hidden_and_no_remote_search_sent`
- [ ] **Step 2:** `cargo test host_filter ctrl_l` → FAIL
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` → PASS
- [ ] **Step 5:** `git commit -m "Cycle the host filter with Ctrl+L"`

---

### Task 13: Remote resume

**Files:**
- Modify: `src/main.rs` (`Commands::Resume { #[arg(long)] file: String, #[arg(long)] uuid: Option<String>, #[arg(long)] host: Option<String> }`; `handle_outcome` routes `Host::Remote` to the remote launcher; Desktop + remote → error string, picker returns), `src/resume/launcher.rs` (`pub fn resume_remote_child(host: &HostConfig, file_path: &str, uuid: Option<&str>) -> Result<(), String>` via `ssh_command(.., SshMode::Resume)` + `run_child`; 255 → `"{host}: не удалось подключиться"`), `src/resume/mod.rs` (`pub fn resume_here(file_path: &str, uuid: Option<&str>) -> Result<(), String>` = `resume_inner(.., ResumeMode::Exec)` with session id from file)
- Test: inline + `tests/cli_resume_remote.rs`

**Interfaces:**
- Produces:
  - `ccs resume --file F [--uuid U]` (no `--host`): `resume_here`; `Err` → message on stderr, exit 1, never prompts
  - `ccs resume --host H --file F [--uuid U]`: looks up `H` in `load_hosts()`, `resume_remote_child`; exit code mirrors the result
  - `fn ssh_exit_result(host: &str, code: Option<i32>, ran_for: Duration) -> Result<(), String>` — 255 → connection error regardless of time, else `child_exit_result`
  - Picker path: remote sessions always run as child (`picker_launch_runs_as_child` gets `host`), quick-exit and bootstrap rules unchanged

- [ ] **Step 1: Write failing tests**
  - `ssh_exit_result_when_255_then_connection_error_with_host`
  - `ssh_exit_result_when_agent_fails_fast_then_agent_error`
  - `picker_launch_when_remote_then_child`
  - `remote_desktop_session_when_resumed_then_error`
  - `tests/cli_resume_remote.rs`: `resume_host_when_fake_ssh_then_remote_command_has_quoted_file_and_uuid` — `CCS_SSH` = script that writes its argv to a temp file and exits 0; assert the last arg equals `"<ccs_path> resume --file '/a b/s.jsonl' --uuid 'u1'"`
  - `resume_here_when_session_missing_then_exit_1_without_prompt` (stdin closed, asserts no "Press Enter" on stderr)
- [ ] **Step 2:** `cargo test ssh_exit_result picker_launch remote_desktop && cargo test --test cli_resume_remote` → FAIL
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** same → PASS
- [ ] **Step 5:** `git commit -m "Resume remote sessions over ssh"`

---

### Task 14: CLI `--host`

**Files:**
- Modify: `src/main.rs` (`--host` on `Search`, `List`, `Pick` as `Vec<String>`, default `all`; `--host` on `Show` default `local`), `src/cli.rs` (`host` field on `CliSearchResult`, `ListResult`; `hosts` on `CliSearchSummary`; merge; `show` over a client for remote), `src/tui/state.rs` (`PickedSession::to_key_value` prints `host: <label>`)
- Create: `src/remote/oneshot.rs`
- Test: `tests/cli_search.rs`, `tests/cli_list.rs`, `tests/cli_show.rs`, `tests/cli_pick.rs`

**Interfaces:**
- Consumes: Tasks 3, 8, 12 (`HostFilter` parsing reused for `--host`)
- Produces:
  - `pub fn parse_host_filter(values: &[String], hosts: &[String]) -> Result<HostFilter, String>`; multiple names → `Only` for each (store as `Vec<String>` if more than one; extend `HostFilter` with `Hosts(Vec<String>)` only if needed)
  - `pub fn query_hosts(hosts: &[HostConfig], req: Request, wait: Duration) -> Vec<(String, Result<Reply, HostStatusLabel>)>` in `oneshot.rs` — spawns one `HostClient` per host in parallel, waits for hello + reply up to `wait` total, drops clients
  - `pub enum HostStatusLabel { Ok, Down(String), Incompatible(Option<String>), Timeout }` serialized as `"ok" | "down" | "incompatible" | "timeout"` plus `error`
  - `CliSearchSummary.hosts: Vec<CliHostSummary { name, status, shown, error: Option<String> }>` (local host listed as `"local"` with `"ok"`)
  - `pub const CLI_SEARCH_WAIT: Duration = 5 s; pub const CLI_LIST_WAIT: Duration = 10 s;` and `pub const CLI_DEFAULT_HOSTS: &str = "all";` (the single constant the user may flip after Task 16)
  - Remote search runs in parallel with local and passes `full_content` and `limit` into `SearchParams`; merge rows by timestamp desc, then `--limit`

- [ ] **Step 1: Write failing tests** (existing tests keep passing because `CCFS_SEARCH_PATH` disables hosts)
  - `search_when_ccs_hosts_fake_then_rows_have_host_field` — `CCS_HOSTS=fake`, `CCS_SSH=fake-ssh.sh`, hosts file in temp `XDG_CONFIG_HOME` with `ccs_path` = test binary; every match row has `"host"`, summary `hosts` has `local` and `fake` with `ok`
  - `search_when_host_local_then_no_remote_rows`
  - `search_when_host_down_then_summary_reports_down_and_local_rows_present` — `ccs_path = "false"`
  - `list_when_fake_host_then_rows_have_host_field`
  - `show_when_host_given_then_reads_through_remote`
  - `pick_output_when_local_then_host_local_line` (unit on `to_key_value`)
- [ ] **Step 2:** `cargo test --test cli_search --test cli_list --test cli_show` → FAIL
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` → PASS
- [ ] **Step 5:** `git commit -m "Search, list and show across hosts from the CLI"`

---

### Task 15: Docs and skill

**Files:**
- Modify: `README.md` (hosts.json, `--host`, Ctrl+L, `serve`/`resume`), `CLAUDE.md` (architecture tree adds `src/remote/`, `src/project_match.rs`, `src/tui/hosts_state.rs`; data-flow bullet for remote hosts; `## Environment` adds `CCS_HOSTS`, `CCS_SSH`, `XDG_CONFIG_HOME`), `.claude/skills/ccs/SKILL.md` and `.agents/skills/ccs/SKILL.md` (`host` field, `ccs show --host`, `--host local` for speed, remote pick flows), `.claude/skills/ccs/scripts/launch-resume.sh` (when pick output has `host:` other than `local`, run `ccs resume --host <host> --file <file_path> [--uuid <uuid>]`)
- If an agent-search prompt that calls `ccs show` already exists in `src/` (parallel Ctrl+G work), add `--host <host>` from each row to it (spec «Связь с Ctrl+G»); otherwise nothing.
- Test: `bash -n` on the script; `cargo test` unchanged

- [ ] **Step 1:** Edit docs and script.
- [ ] **Step 2:** `bash -n .claude/skills/ccs/scripts/launch-resume.sh` → no output
- [ ] **Step 3:** `git commit -m "Document remote hosts in README, CLAUDE.md and the ccs skill"`

---

### Task 16: Measure and verify on Z270XX

No code beyond tuning the three constants of Task 6 if the budget is missed.

- [ ] **Step 1:** Build release in the worktree: `cargo build --release`.
- [ ] **Step 2:** Copy the source tree to Z270XX without touching `~/projects/claude-code-fullsearch-rust` there: `rsync -a --exclude target ./ Z270XX:/tmp/ccs-remote-build/` then `ssh Z270XX 'cd /tmp/ccs-remote-build && cargo build --release'`. Remote binary: `/tmp/ccs-remote-build/target/release/ccs`. Do not `cargo install`.
- [ ] **Step 3:** Temp config on the Mac: `XDG_CONFIG_HOME=$SCRATCH/xdg` with `ccs/hosts.json` = `{"hosts":[{"name":"Z270XX","ccs_path":"/tmp/ccs-remote-build/target/release/ccs"}]}`.
- [ ] **Step 4:** Payload check: `search "error"` response size via `ssh Z270XX /tmp/ccs-remote-build/target/release/ccs serve --stdio` with one request; expected ≈ 2 MB, else tune `MAX_MATCHES_PER_SESSION` / `MAX_SESSIONS` / `SNIPPET_CONTEXT_CHARS` and commit `Tune remote search payload limits`.
- [ ] **Step 5:** Benchmark with `hyperfine --warmup 1 --runs 10` on the Mac, queries: a rare word, `error`, regex `OOM|OutOfMemory`; variants: installed `ccs` (current release), new `--host local`, new default (Z270XX up), new default with `CCS_HOSTS=nosuchhost`; same four for `ccs list`. Save the table to `$SCRATCH/bench.md`.
- [ ] **Step 6:** Show-after-search latency (spec PLAUSIBLE finding): time a `show` sent right after `search "error"` starts streaming; record the number.
- [ ] **Step 7:** Manual TUI check with the temp config: search, preview, tree, resume Claude and Codex, overlay, Tailscale down (`CCS_HOSTS=nosuchhost` as proxy), Ctrl+C during `ssh -t` resume in overlay → hosts stay Ready. Record results.
- [ ] **Step 8:** Clean up: `ssh Z270XX 'rm -rf /tmp/ccs-remote-build'`.
