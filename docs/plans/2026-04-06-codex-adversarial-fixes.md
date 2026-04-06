# Codex Adversarial Fixes

## Overview
Fix three safety/correctness issues identified by Codex adversarial review of the entire codebase:

1. **Resume creates guessed project directories** — `build_resume_command()` trusts lossy `decode_project_path()` and `create_dir_all()`s non-existent paths, potentially launching Claude from a wrong cwd
2. **Session index race + non-atomic write** — `ensure_session_in_index()` has a fixed temp file name (`.sessions-index.json.tmp`) and no file locking, so concurrent resumes can lose entries
3. **Search silently truncates matches** — `--max-count 1000` per-file cap drops newer matches from large transcripts with no UI signal

## Context (from discovery)
- Files involved: `src/resume/launcher.rs`, `src/resume/path_codec.rs`, `src/search/ripgrep.rs`, `src/tui/state.rs`, `src/tui/render_search.rs`
- Existing tests: `src/resume/launcher.rs` (4 tests), `src/resume/path_codec.rs` (~14 tests), `src/search/ripgrep.rs` (~30 tests)
- Related patterns: atomic write already partially implemented (temp + rename), path codec has round-trip tests
- Dependencies: no new crates needed (use `std::process::id()` for unique temp names)

## Development Approach
- **Testing approach**: TDD — write failing tests first, then implement fixes
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- Run `cargo test && cargo clippy --all-targets --all-features -- -D warnings` after each change

## Testing Strategy
- **Unit tests**: required for every task — test both success and error/edge cases
- **Integration tests**: existing `tests/resume_resolution.rs` and `tests/cli_search.rs` cover end-to-end flows
- Test commands: `cargo test`, `cargo clippy --all-targets --all-features -- -D warnings`

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Implementation Steps

### Task 1: Safe project path recovery in resume (TDD)
- [ ] write test: `test_build_resume_command_does_not_create_nonexistent_project_dir` — verify that when `decode_project_path` returns a path that doesn't exist, the function does NOT call `create_dir_all` and falls back to a safe directory
- [ ] write test: `test_build_resume_command_uses_existing_decoded_path` — verify that when decoded path already exists, it is used as working dir
- [ ] write test: `test_build_resume_command_falls_back_to_session_parent` — verify fallback to session file's parent directory when decode fails or path doesn't exist
- [ ] implement fix in `build_resume_command()` (`src/resume/launcher.rs:210-233`): remove `create_dir_all` for decoded paths; only use decoded path when it already exists; fall back to session file parent dir or `$HOME`
- [ ] run tests — must pass before next task

### Task 2: Atomic session index with per-process temp file (TDD)
- [ ] write test: `test_ensure_session_in_index_uses_unique_tmp_file` — verify temp file name includes process ID
- [ ] write test: `test_ensure_session_in_index_atomic_rename` — verify final index file is valid JSON even if interrupted (write to temp file containing pid, then rename)
- [ ] write test: `test_ensure_session_in_index_idempotent` — verify calling twice with same session_id doesn't duplicate the entry
- [ ] implement fix in `ensure_session_in_index()` (`src/resume/launcher.rs:108-166`): use `format!(".{}.{}.tmp", SESSIONS_INDEX_FILE, std::process::id())` for unique temp name; propagate write/rename errors via eprintln instead of silently swallowing
- [ ] run tests — must pass before next task

### Task 3: Search truncation signal (TDD)
- [ ] write test: `test_search_returns_truncated_flag_when_max_count_hit` — verify that when a file has more than max-count matches, the result indicates truncation
- [ ] write test: `test_search_truncation_surfaces_in_status_bar` — verify the UI status text shows "results may be incomplete" when truncated
- [ ] add `truncated: bool` field to search result flow: `search_single_path` → `search_multiple_paths` → `SearchResult` type → `App.search_truncated`
- [ ] detect truncation in `search_single_path`: if ripgrep found exactly `max_count` matches in any file, set `truncated = true`
- [ ] display truncation warning in `search_results_status_text()` (`src/tui/render_search.rs:14-45`): append "(results may be incomplete)" when truncated
- [ ] run tests — must pass before next task

### Task 4: Verify acceptance criteria
- [ ] verify: resume no longer creates directories for non-existent projects
- [ ] verify: concurrent index writes use unique temp files
- [ ] verify: search truncation is surfaced in UI
- [ ] run full test suite: `cargo test`
- [ ] run linter: `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] run format check: `cargo fmt --check`

### Task 5: [Final] Update documentation
- [ ] update CLAUDE.md if any new environment variables or behaviors were added
- [ ] update README if user-visible behavior changed (truncation warning)

## Technical Details

### Task 1: Path safety
- `decode_project_path()` returns `Option<String>` — the decoded filesystem path
- Current code: `if !Path::new(&dir).exists() { fs::create_dir_all(&dir)... }` — REMOVE this
- New logic: `if Path::new(&dir).exists() { dir } else { fallback }`
- Fallback priority: session file parent → `$HOME` → `/tmp`

### Task 2: Atomic index
- Current temp file: `.sessions-index.json.tmp` (fixed name, shared by all processes)
- New temp file: `.sessions-index.json.<pid>.tmp` (unique per process)
- Rename is atomic on POSIX (same filesystem); this is sufficient for crash safety
- For concurrency (two processes): last-writer-wins is acceptable for this use case (index entry is idempotent — adding same session twice is harmless)

### Task 3: Truncation signal
- ripgrep `--max-count N`: limits matches per file. If a file has exactly N matches, truncation may have occurred
- Detection heuristic: count matches per file; if any file reached max-count, flag as potentially truncated
- `SearchResult` tuple type: add `bool` at the end for truncation flag
- Status bar already shows match/session counts — append warning text when truncated

## Post-Completion

**Manual verification:**
- Test resume on a session whose project directory was deleted — should NOT recreate it
- Run two `ccs` instances simultaneously resuming in the same project — verify both entries appear in index
- Search for a very common word in a large session file — verify truncation warning appears
