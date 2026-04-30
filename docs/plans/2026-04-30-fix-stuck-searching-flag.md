# Fix Stuck `searching` Flag — Preemptive Cancellation Architecture

## Overview

The TUI permanently displays "Searching…" after a small race during fast typing/editing, even when the background worker is idle and no `rg` child exists. Rooted in three architectural anti-patterns in the search subsystem:

1. **Stored vs. derived state**: `SearchState.searching: bool` is a stored flag that shadows "is there an outstanding request?". Any early `return` in `handle_search_result` (state.rs:1102-1109) leaves it at `true` forever. Live evidence: `sample(1)` of PID 50939 showed search worker parked on `mpmc::Receiver::recv → semaphore_wait_trap`, no in-flight `rg` — yet the UI still showed "Searching…".
2. **No preemption**: a single long-lived worker thread (state.rs:627-638) consumes queries serially via `query_rx.recv()`. Older requests run to completion even when a newer query has superseded them. On a 4 GB / 11 554-jsonl corpus, typing `F → FP → FPF` queues three searches that take ≈ 5 s + 2 s + 1 s = 8 s of head-of-line blocking.
3. **Buffered stdout**: `Command::new("rg").output()` collects the full ripgrep stdout into memory before returning. Single character `F` produces 891 k JSON lines and pushes peak RSS to 594 MB (observed on the live process). *(This third item is bundled for memory cleanup; the stuck-flag fix does not depend on it, but we are already touching the `Command` path so streaming comes for free.)*

The fix replaces the worker-and-flag model with a **single-source-of-truth handle** that owns the lifecycle of an outstanding request, with cooperative + forced cancellation reaching the `rg` child process.

## Context (from discovery)

- **Files involved**:
  - `src/tui/state.rs` — `SearchState`, `start_search`, `handle_search_result`, `tick`, App constructor with the legacy worker thread (lines 623-638, 1067-1090, 1092-1178, 1370-1384).
  - `src/search/ripgrep.rs` — `search_multiple_paths`, `search_single_path` (lines 80-191) — uses blocking `Command::output()`.
  - `src/search/mod.rs` — re-exports `search_multiple_paths`, `RipgrepMatch`, `SearchResult`.
  - Tests inline in both files (`#[cfg(test)] mod tests`).
- **Existing patterns to mirror**: `message_count_cancel: Option<Arc<AtomicBool>>` in `SearchState` (state.rs:527, used in 789-791, 1142-1144, 1155-1169) is the project's idiomatic cancellation primitive — apply the same pattern to the main search.
- **Dependencies**: no new crates. `std::sync::Arc`, `std::sync::atomic::AtomicBool`, `std::process::{Command, Stdio, Child}`, `std::io::{BufRead, BufReader}`, `std::thread`, `std::sync::mpsc::{Sender, Receiver, channel}` are already in use.
- **Tests reference points**: `test_stale_search_result_ignored_when_scope_changes` (state.rs:429-456) currently asserts secondary-filter behavior that will be removed; needs rewriting against the new contract (seq-only discrimination, derived `is_searching()`).

## Development Approach

- **testing approach**: TDD (tests first)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional — they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- maintain backward compatibility of public CLI behavior (TUI surface is internal — internal API may change freely)

## Testing Strategy

- **unit tests**: required for every task
  - cancellation: cancel-before-spawn returns immediately, cancel-during-iteration returns `Err("cancelled")` and the `rg` child is reaped (no zombie)
  - state transitions: `start_search` cancels prior handle; `handle_search_result` discriminates by `seq` only and clears `current` exactly when the matching result lands
  - integration: typing through three queries cancels the first two and only the last produces a visible result
- **e2e tests**: project has no Playwright/Cypress harness. Manual smoke after Task 5: `cargo run --release` → type `F` slowly, then quickly type `PF`, observe the status bar transitions correctly and final results match `FPF`.
- **regression guard**: an explicit test for the original bug — push two start_searches with overlapping fields where the second one races with input mutation; assert `is_searching() == false` after the second result lands and no later mutation can leave it stuck.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

**Architecture**: each `start_search` becomes a self-contained request with its own thread, cancellation token, and lifecycle handle. The UI holds at most one `SearchHandle`. Spawning a new request cancels the prior handle (cooperative + forced via `Child::kill`). The "searching" status becomes a derived predicate (`current.is_some()`).

```
                          ┌──────────────────────┐
                          │ App.tick()           │
                          │   poll search_rx     │
                          │   on result:         │
                          │     if seq matches → │
                          │       current = None │
                          │       process matches│
                          └──────────▲───────────┘
                                     │ Sender<BackgroundSearchResult>
                                     │
   ┌────────────────────┐  spawn   ┌─┴────────────────────────────┐
   │ start_search()     │ ────────▶│ thread N (per request)       │
   │   seq += 1         │          │   rg = Command::spawn()      │
   │   prev.cancel.set  │          │   stream stdout line-by-line │
   │   current = handle │          │   check cancel each line     │
   └────────────────────┘          │   if cancel: child.kill()    │
                                   │   if !cancel: tx.send(result)│
                                   └──────────────────────────────┘
```

**Key design decisions**:
1. **`SearchHandle { seq, cancel: Arc<AtomicBool> }`** is the single source of truth for in-flight state. `is_searching()` is `current.is_some()`. The `searching: bool` field is removed.
2. **Spawn-per-request** instead of single long-lived worker. The worker thread (state.rs:627-638) is deleted. Brief peak of two concurrent threads at preemption (the one being cancelled + the new one) is acceptable; the cancelled one exits within milliseconds after `child.kill()`.
3. **Cooperative cancellation in ripgrep wrapper** with `Child::spawn() + BufReader::lines()` instead of `Command::output()`. Each line iteration checks `cancel.load(Relaxed)`; on cancel, kill the child, wait, return `Err("cancelled")`. The worker drops the cancelled result without sending.
4. **Seq is the only discriminator** in `handle_search_result`. Secondary filters on `query`/`paths`/`use_regex` are removed: a newer request always cancels the older one before it can send, so any received result with `seq == current.seq` is by definition consistent with the request that produced it.

**How it fits in**: the message-count subsystem already uses `Arc<AtomicBool>` cancellation (state.rs:527, 1155-1169). The main search adopts the same primitive. Tests in `tui/state.rs` follow the existing fixture style.

## Technical Details

### New types (`src/tui/state.rs`)

```rust
pub(crate) struct SearchHandle {
    pub seq: u64,
    pub cancel: Arc<AtomicBool>,
}
```

### `SearchState` changes

- Remove: `searching: bool`, `search_tx: Sender<(u64, String, Vec<String>, bool)>`.
- Add: `result_tx: Sender<BackgroundSearchResult>` — kept as a field so each spawn-per-request thread can `.clone()` it. Currently `result_tx` is local to `App::new` (state.rs:623) and moved into the legacy worker; promote it to a `SearchState` field.
- Add: `current: Option<SearchHandle>`.
- Keep: `BackgroundSearchResult` struct unchanged (state.rs:493-501). The `query`/`paths`/`use_regex` fields stay because `query` still feeds `self.search.results_query = query` in `handle_search_result`. We stop *validating* them, not carrying them.

### `App::is_searching()` accessor

`pub fn is_searching(&self) -> bool { self.search.current.is_some() }` — exposed via `AppView` for renderers.

### `start_search` (state.rs)

```rust
pub(crate) fn start_search(&mut self) {
    self.search.search_seq += 1;
    let seq = self.search.search_seq;
    self.last_query = self.input.text().to_string();
    self.last_regex_mode = self.regex_mode;
    self.last_search_paths = self.search_paths.clone();

    if let Some(prev) = self.search.current.take() {
        prev.cancel.store(true, Ordering::Relaxed);
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    let tx = self.search.result_tx.clone();
    let query = self.input.text().to_string();
    let paths = self.search_paths.clone();
    let use_regex = self.regex_mode;

    thread::spawn(move || {
        let result = search_multiple_paths(&query, &paths, use_regex, &cancel_worker);
        if !cancel_worker.load(Ordering::Relaxed) {
            let _ = tx.send(BackgroundSearchResult { seq, query, paths, use_regex, result });
        }
    });

    self.search.current = Some(SearchHandle { seq, cancel });
}
```

### `handle_search_result` (state.rs)

```rust
pub(crate) fn handle_search_result(&mut self, r: BackgroundSearchResult) {
    let Some(current) = self.search.current.as_ref() else { return; };
    if r.seq != current.seq { return; }
    self.search.current = None;
    // ...existing match-processing logic, minus the now-redundant secondary filter...
    // CRITICAL: preserve the AI-rank gate from state.rs:1130 verbatim:
    //   if self.ai.ranked_count.is_none() && !self.ai.thinking { self.apply_groups_filter(); }
}
```

**Race window note**: between the worker's final `cancel.load()` and `tx.send(...)`, a cancellation may arrive too late to suppress the send. The result then lands on `result_rx` with a stale `seq` and is dropped by `handle_search_result`'s `seq != current.seq` check. This is by design — the seq discriminator handles the load-vs-send race; do not try to close it with extra synchronization.

**Imports**: at the top of `state.rs`, ensure `use std::sync::atomic::{AtomicBool, Ordering};` is present (the existing `message_count_cancel` code at state.rs:1143, 1159 uses fully-qualified paths — pick one style and apply consistently).

### `reset_search_state` / `clear_input`

Replace `self.search.searching = false;` with `self.search.current = None;` (also cancels the in-flight request via `take()` + drop, but explicitly setting the cancel flag is cleaner — keep an explicit `if let Some(h) = self.search.current.take() { h.cancel.store(true, Relaxed); }`).

### `search_multiple_paths` and `search_single_path` (`src/search/ripgrep.rs`)

Both gain an extra `cancel: &Arc<AtomicBool>` argument. `search_single_path` switches from `Command::output()` to:

```rust
let mut child = Command::new("rg")
    .args(&args)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
let stdout = child.stdout.take().expect("stdout was piped");
let reader = BufReader::new(stdout);
for line in reader.lines().map_while(Result::ok) {
    if cancel.load(Ordering::Relaxed) {
        let _ = child.kill();
        let _ = child.wait();
        return Err("cancelled".into());
    }
    // existing parse + post-filter
}
let status = child.wait()?;
if !status.success() && status.code() != Some(1) {
    return Err(format!("ripgrep exited {status}"));
}
```

The cheap relaxed `AtomicBool::load` per line (~1 ns) is dwarfed by JSON parsing (~10 µs/line); no batching needed.

### Background worker thread

The `thread::spawn(move || while let Ok(...) = query_rx.recv() { ... })` block in `App::new` (state.rs:627-638) is removed entirely. The `query_tx`/`query_rx` channel goes away.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code, tests, doc comments inside this repo.
- **Post-Completion** (no checkboxes): manual TUI smoke (cannot be automated without a TTY harness), reinstall via `cargo install --path .` on Mac and Z270XX, observe-on-real-corpus regression check.

## Implementation Steps

### Task 1: Add cooperative cancellation to ripgrep wrapper

**Files:**
- Modify: `src/search/ripgrep.rs`
- Modify: `src/tui/state.rs` (legacy worker thread call site only — passes a fresh always-false `Arc<AtomicBool>` per iteration so behavior is unchanged at this stage)

- [x] write a test for `search_multiple_paths` accepting `&Arc<AtomicBool>`: when the flag is `true` *before* invocation, the function returns `Err("cancelled")` immediately (no `rg` spawned)
- [x] write a test for mid-flight cancellation: build a tempdir fixture with a JSONL file containing > 10 k lines that all match a query; spawn the search in a thread; sleep ~50 ms; set the cancel flag; `join` the thread; assert the result is `Err("cancelled")` and the wall-clock time of the join is well under the uncancelled baseline (e.g. < 500 ms deadline). The deadline assertion is what verifies the rg child was killed rather than drained — drop the earlier "no zombie via `ps`" idea, it's unobservable from test scope and not idiomatic in this codebase
- [x] convert `search_single_path` from `Command::output()` to `Command::spawn()` + `BufReader::lines()`; check `cancel.load(Ordering::Relaxed)` each line; on cancel, `child.kill()` + `child.wait()` + return `Err("cancelled")`. Add `let status = child.wait()?;` after the loop; treat `status.success()` and `status.code() == Some(1)` (no matches) as success
- [x] thread the `cancel: &Arc<AtomicBool>` argument through `search_multiple_paths` (forward to `search_single_path` per path, and short-circuit between paths if cancel flips)
- [x] update the legacy worker thread (state.rs:627-638) to construct `let cancel = Arc::new(AtomicBool::new(false));` per iteration and pass `&cancel` to `search_multiple_paths`. This keeps behavior unchanged — Task 1 is a pure-refactor commit. The worker loop still receives and serves one query at a time
- [x] keep the `#[cfg(test)] fn search` (ripgrep.rs:64) and `search_with_options` (ripgrep.rs:70) test helpers' public signatures unchanged: they internally create a fresh `Arc<AtomicBool>::new(false)` and forward to `search_multiple_paths`. This means existing test bodies (`search("Hello", path)` etc.) need no edits
- [x] run tests — `cargo test ripgrep` and full `cargo test` must pass before next task

### Task 2: Introduce SearchHandle and replace `searching: bool`

**Files:**
- Modify: `src/tui/state.rs`
- Modify: `src/tui/view.rs` (expose `is_searching()` to renderers)
- Modify: `src/tui/render_search.rs` (line 340 reads `app.search.searching`)

> **Atomicity note**: After this task lands, the legacy worker thread (state.rs:627-638) becomes idle — its `query_rx.recv()` returns `Err` because we drop the `query_tx` field and the sender is no longer held anywhere. The thread exits cleanly. Task 3 then removes the now-dead worker code. This means the tree compiles and tests pass at the end of Task 2 even though the worker is technically still spawned but immediately stops.

- [x] write a test asserting that calling `start_search` twice in a row stores `Some(_)` in `current`, sets the *first* handle's `cancel` flag to `true`, and that the second handle has a different `seq`. Use an empty `search_paths` (which makes `search_multiple_paths` return `Ok` immediately so the spawned thread exits without doing real work)
- [x] write a test for the new `handle_search_result` contract via direct channel injection: (a) `current = None` → `handle_search_result` no-op; (b) `current = Some(seq=5)` and result with `seq=3` → no-op, `current` unchanged; (c) `current = Some(seq=5)` and result with `seq=5` → `current = None`, `groups` populated
- [x] write a test that `handle_search_result` does NOT call `apply_groups_filter()` when `ai.ranked_count.is_some()` — i.e. preserves the AI-rank gate (mirror the existing `ai_handle_search_result_preserves_groups_when_rank_applied` test at state.rs:2906)
- [x] write a test that `clear_input()` during an in-flight search sets the current handle's cancel flag to `true` and clears `current`
- [x] rewrite `test_stale_search_result_ignored_when_scope_changes` (state.rs:429-456) against the new contract — assert that a result with `seq < current.seq` is dropped and `current` is preserved (no scope-change semantics anymore)
- [x] add `pub(crate) struct SearchHandle { pub seq: u64, pub cancel: Arc<AtomicBool> }` to `state.rs`
- [x] in `SearchState`: remove `searching: bool` and `search_tx`; add `current: Option<SearchHandle>` and `result_tx: Sender<BackgroundSearchResult>`
- [x] add `pub fn is_searching(&self) -> bool { self.search.current.is_some() }` on `App`; expose via `AppView::is_searching()`
- [x] rewrite `start_search` per the design block above (cancel previous handle → spawn thread → store new handle)
- [x] rewrite `handle_search_result` per the design block above; **CRITICAL: preserve verbatim the `if self.ai.ranked_count.is_none() && !self.ai.thinking { self.apply_groups_filter(); }` gate** at state.rs:1130 — only the seq/query/paths/use_regex secondary `if` block at lines 1102-1109 is removed
- [x] update `reset_search_state` and `clear_input`: replace `self.search.searching = false;` with `if let Some(h) = self.search.current.take() { h.cancel.store(true, Ordering::Relaxed); }`
- [x] update `render_search.rs:340` to call `view.is_searching()` instead of reading `app.search.searching`
- [x] run tests — `cargo test` must pass before next task

### Task 3: Remove the long-lived background worker thread

**Files:**
- Modify: `src/tui/state.rs`

- [ ] write an end-to-end test for spawn-per-request preemption: build a `tempfile::TempDir` with one short JSONL fixture (e.g. one match line), construct `App::new(vec![tmp.path().to_str().unwrap().into()])`, set `app.input.set_text("foo")`, call `app.start_search()` twice back-to-back, sleep 100 ms, then collect `app.search.search_rx.try_iter().count()` — expect exactly **1** delivered result (the first request was cancelled before completion or its send was suppressed by `!cancel.load()`). Then call `app.tick()` and assert `app.is_searching() == false`
- [ ] delete the `thread::spawn(move || while let Ok((seq, query, paths, use_regex)) = query_rx.recv() { ... })` block from `App::new` (state.rs:627-638)
- [ ] delete the `query_tx`/`query_rx` channel construction (state.rs:624)
- [ ] keep `result_rx` (consumer in `tick()`); `result_tx` is now stored in `SearchState` per Task 2
- [ ] verify `tick()` still drains `search_rx.try_recv()` correctly — no change expected, but run the existing test `test_app_receives_recent_sessions_from_background` to confirm the polling path is healthy
- [ ] run tests — `cargo test` must pass before next task

### Task 4: Update existing tests touching `searching` and stale-result paths

**Files:**
- Modify: `src/tui/state.rs` (test module)
- Modify: `src/tui/search_mode.rs` (test module)

- [ ] replace direct mutations of `app.search.searching = true` with `app.search.current = Some(SearchHandle { seq: <appropriate>, cancel: Arc::new(AtomicBool::new(false)) })` at:
  - `src/tui/state.rs:1701` (test setup)
  - `src/tui/state.rs:2058` (test setup)
  - `src/tui/search_mode.rs:412` (test setup)
  - `src/tui/search_mode.rs:434` (test setup)
  - `src/tui/search_mode.rs:455` (test setup)
- [ ] replace assertions on `app.search.searching` with `app.is_searching()` (or `app.search.current.is_some()` inside the same module) at:
  - `src/tui/state.rs:1718`
  - `src/tui/state.rs:2091`
  - `src/tui/search_mode.rs:412` (post-condition assertion paired with the setup line above)
- [ ] write a concrete regression test reproducing the original stuck-flag failure mode against the new model: in a unit test, override the result channel to a test channel, call `app.start_search()` twice (the second cancels the first via `prev.cancel.store(true)`), inject two `BackgroundSearchResult`s onto the channel — one with `seq=1`, one with `seq=2` — call `app.tick()` once, then assert `app.is_searching() == false` *and* that only the seq=2 result's data populated `app.search.groups`
- [ ] grep across the codebase: `rg 'app\.search\.searching|search_tx\b|\bsearching:\s*bool'` should return zero non-test hits and zero references in renderer/state code
- [ ] run tests — `cargo test` must pass

### Task 5: Verify acceptance criteria

- [ ] verify all requirements from Overview are implemented (no `searching: bool` left; `current` is the source of truth; ripgrep is cancellable; worker thread removed)
- [ ] verify edge cases are handled (rapid typing, Ctrl+C mid-search, regex toggle mid-search, project-filter toggle mid-search, AI-rank invalidation still works)
- [ ] run full test suite: `cargo test`
- [ ] run lints: `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] run formatter: `cargo fmt --check`
- [ ] manual TUI smoke: `cargo run --release` → in TUI, type `F` slowly, then quickly type `PF`; observe that the status bar transitions cleanly and final results are for `FPF`. Type `abc`, edit to `abd`, edit back to `abc` — verify no stuck "Searching…"
- [ ] manual memory check: `cargo build --release && /usr/bin/time -l ./target/release/ccs search F` and confirm peak RSS is dramatically lower than 594 MB (running via `cargo run` would also count `cargo`'s footprint, so measure the binary directly)

### Task 6: Update CLAUDE.md to reflect new architecture

**Files:**
- Modify: `CLAUDE.md`

- [ ] update the "Key data flow" section #1 (Search) to describe spawn-per-request + cancellation instead of debounced async search via shared worker
- [ ] note `SearchHandle` and the derived `is_searching()` predicate
- [ ] move this plan to `docs/plans/completed/` (`mkdir -p docs/plans/completed && git mv docs/plans/2026-04-30-fix-stuck-searching-flag.md docs/plans/completed/`)
- [ ] run tests — `cargo test` (no Rust code changed, but a final clean run guards against accidental edits) and `cargo fmt --check`

## Post-Completion

*Items requiring manual intervention or external systems — informational only*

**Manual verification**:
- Reinstall via `cargo install --path . --locked` on Mac after merge (release build).
- Reinstall on Z270XX via the temp-clone procedure documented in `CLAUDE.local.md` (do not pull or reset on the divergent working tree there).
- Real-corpus smoke: open `ccs` against the live ~/.claude/projects (4 GB / 11 554 jsonl), exercise rapid editing of queries, confirm no stuck "Searching…" state and no perceptible 8-second hang from the previous HOL behavior.

**External system updates**:
- None. This is internal-only; the CLI surface and JSONL file format are unchanged.
- If a release tag is cut, `cargo-dist` will publish to the materkey/homebrew-ccs tap automatically.
