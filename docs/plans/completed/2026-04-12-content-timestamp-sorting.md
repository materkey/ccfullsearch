# Sort recent sessions by content timestamp instead of file mtime

## Overview
- `RecentSession.timestamp` uses file mtime, which diverges from actual last message time in ~20% of files (up to 17 hours)
- Root cause: Claude appends `last-prompt` and `permission-mode` records (no `timestamp` field) after session ends, inflating mtime
- Fix: extract max `timestamp` from already-parsed JSONL lines in `scan_tail`/`scan_head`, use it instead of mtime
- Zero performance impact: no additional I/O, `session::extract_timestamp()` already exists

## Context
- All changes in `src/recent.rs`
- `session::extract_timestamp()` in `src/session/mod.rs` handles both `timestamp` (CLI) and `_audit_timestamp` (Desktop)
- `find_summary_from_tail_with_chain()` already parses every JSON line in the tail — adding timestamp extraction is one extra `.get()` per line
- Pre-filter by mtime in `collect_recent_sessions()` stays unchanged (Phase 1 sorting)
- Measured: 6/30 files had >40 min drift, some shifted 10 positions in sort order

## Development Approach
- **Testing approach**: TDD — write failing test first, then implement
- All changes in one file (`src/recent.rs`), single logical unit
- **CRITICAL: every task MUST include new/updated tests**
- **CRITICAL: all tests must pass before starting next task**

## Implementation Steps

### Task 1: TDD — write failing test for content timestamp preference
- [x] write test `test_extract_summary_prefers_content_timestamp_over_mtime`: create JSONL file with content timestamp `2025-01-01T10:00:00Z`, set file mtime to `2025-06-01T10:00:00Z`, assert `RecentSession.timestamp` matches content timestamp (not mtime)
- [x] run tests — new test must FAIL (proves it currently uses mtime)

### Task 2: Add `last_timestamp` to structs
- [x] add `last_timestamp: Option<DateTime<Utc>>` to `TailSummaryScan` (line 172)
- [x] add `last_timestamp: Option<DateTime<Utc>>` to `ScanResult` (line 72)
- [x] run tests — new test still fails, existing tests pass

### Task 3: Extract timestamps in scan functions
- [x] in `find_summary_from_tail_with_chain` loop (after line 244): track max `session::extract_timestamp(&json)` into `result.last_timestamp`
- [x] in `scan_tail` (line 310): propagate `tail.last_timestamp` to `ScanResult.last_timestamp`
- [x] in `scan_head` (line 105): track max timestamp the same way
- [x] run tests — new test still fails (extract_summary not yet using it)

### Task 4: Use content timestamp in `extract_summary`
- [x] after head+tail+middle merge, compute `content_timestamp` as max of `head.last_timestamp`, `tail.last_timestamp`, and (if scanned) `middle.last_timestamp`
- [x] replace all 4 `timestamp: mtime_timestamp` (lines 624, 637, 650, 676) with `timestamp: content_timestamp.unwrap_or(mtime_timestamp)`
- [x] run tests — new test must PASS now

### Task 5: Update existing tests and docs
- [x] `test_collect_recent_sessions_sorts_by_timestamp_desc` (line 1112): remove `set_file_mtime` calls, update comment "mtime" → "content timestamp"
- [x] `test_dedup_sessions_keeps_newest` (line 1633): remove `set_file_mtime` calls, update comment
- [x] update doc comment on `extract_summary` (line 536): "Uses file mtime" → "Uses last message timestamp (falls back to file mtime)"
- [x] update doc comment on `collect_recent_sessions` (line 731): "sorts by filesystem mtime" → "sorts by content timestamp"
- [x] run tests — all pass

### Task 6: Verify acceptance criteria
- [x] run full test suite: `cargo test`
- [x] run linter: `cargo clippy --all-targets --all-features -- -D warnings`
- [x] run formatter: `cargo fmt --check`
- [x] run TUI: `cargo run` — verify recent sessions list renders correctly

## Technical Details
- `session::extract_timestamp(json)` → tries `json.get("timestamp")`, falls back to `json.get("_audit_timestamp")`, parses RFC 3339 → `Option<DateTime<Utc>>`
- Timestamp resolution: take `max()` across all scan phases (head/tail/middle) — handles files where newest record is anywhere
- Fallback to mtime when no content timestamp found (e.g., malformed files)
- Pre-sort in `collect_recent_sessions` stays mtime-based — it's only a rough filter to pick candidate files
