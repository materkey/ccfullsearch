# Cleanup & correctness: align with Claude Code session format

## Overview

Combined plan based on analysis of Claude Code source (`~/projects/claude-code/src/utils/sessionStorage.ts`, `sessionStoragePortable.ts`, `conversationRecovery.ts`, `listSessionsImpl.ts`, `types/logs.ts`).

Three priorities:
1. **Remove dead linearization code** — already disabled (#[cfg(test)]), ~570 lines of cruft
2. **Fix P0 correctness bugs** — chain building, leaf finding, compact_boundary, isSidechain, progress bridge
3. **P1 improvements** — new metadata types, empty file skip

Fork (`create_fork`) is kept for tree-view branch resume — `--fork-session` cannot replace it because it doesn't accept a specific message UUID to branch from.

## Context

**Research reference**: `docs/plans/2026-04-02-claude-code-source-research.md` — full findings with file references.

**Key Claude Code files**:
- `sessionStorage.ts:2069-2206` — buildConversationChain + recoverOrphanedParallelToolResults
- `sessionStorage.ts:3472-3813` — loadTranscriptFile + progress bridge + leaf finding
- `sessionStoragePortable.ts:79-149` — parseSessionInfoFromLite (head/tail metadata)
- `sessionStoragePortable.ts:486-510` — compact_boundary detection
- `listSessionsImpl.ts:79-149` — lite session listing
- `types/logs.ts:221-231` — TranscriptMessage (parentUuid, logicalParentUuid, isSidechain, agentId)

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- **CRITICAL: every task MUST include new/updated tests**
- **CRITICAL: all tests must pass before starting next task**
- Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

---

## Phase 1: Remove synthetic linearization

### Task 1: Remove synthetic linear API from session.rs
- [x] remove `SYNTHETIC_LINEAR_FIELD` constant
- [x] remove `mark_synthetic_linear_record()` function
- [x] remove `is_synthetic_linear_record()` function
- [x] remove tests: `test_mark_synthetic_linear_record`, `test_is_synthetic_linear_record_false_by_default`
- [x] run tests — must pass before next task

### Task 2: Remove synthetic filters from recent.rs
- [x] remove all 6 `if session::is_synthetic_linear_record(&json) { continue; }` checks
- [x] at combined condition: simplify — keep only the record type check
- [x] remove tests: `test_extract_summary_returns_none_for_synthetic_linear_bootstrap_only`, `test_extract_summary_keeps_resumed_synthetic_branch_visible`
- [x] run tests — must pass before next task

### Task 3: Remove synthetic filter from search/message.rs
- [x] remove `if session::is_synthetic_linear_record(&json) { return None; }`
- [x] verify `use crate::session` is still used (yes — `extract_record_type` etc.)
- [x] remove test: `test_skip_synthetic_linear_message`
- [x] run tests — must pass before next task

### Task 4: Remove linearization code from resume/launcher.rs
- [x] remove `SYNTHETIC_SOURCE_PATH_FIELD` constant (#[cfg(test)])
- [x] remove `SessionAnalysis.is_linear` field + all #[cfg(test)] computation blocks
- [x] remove #[cfg(test)] functions: `cleanup_legacy_synthetic_sessions`, `disposable_synthetic_session_matches_source`, `create_linear_session`, `synthetic_source_fingerprint`, `is_synthetic_linear_session_file`, `stable_synthetic_session_id`, `fnv1a64`
- [x] remove tests: `test_analyze_session_treats_interleaved_metadata_as_linear`, `test_analyze_session_still_detects_real_branch_with_metadata_nodes`, all `test_create_linear_session_*` (6), all `test_cleanup_legacy_synthetic_sessions_*` (3)
- [x] run tests — must pass before next task

---

## Phase 2: Fix P0 correctness bugs

### Task 5: Add isSidechain filter to fork.rs
**Bug**: `build_chain_from_tip` treats sidechain (subagent) records as regular messages. If the last UUID-bearing record is a sidechain entry, the "latest chain" is wrong. Claude Code filters with `!m.isSidechain` when finding leaf. (Research #1)
- [x] in `build_chain_from_tip`: skip records where `isSidechain` is true
- [x] in `latest_tip_uuid`: same filter
- [x] in `create_fork`: same filter when building uuid_to_parent map
- [x] write test `test_build_chain_ignores_sidechain_records`
- [x] write test `test_create_fork_ignores_sidechain_records`
- [x] run tests — must pass before next task

### Task 6: Add compact_boundary handling to fork.rs
**Bug**: Sessions >5MB contain `{"type":"system","subtype":"compact_boundary"}` markers. Everything before the last marker is stale. `build_chain_from_tip` parses everything, potentially building chains through stale UUIDs. (Research #2)
- [x] in `build_chain_from_tip`: on compact_boundary, clear `uuid_to_parent` and `last_uuid`
- [x] in `latest_tip_uuid`: same reset
- [x] in `create_fork`: same reset + skip pre-boundary lines
- [x] write test `test_build_chain_resets_on_compact_boundary`
- [x] write test `test_create_fork_handles_compact_boundary`
- [x] run tests — must pass before next task

### Task 7: Add logicalParentUuid fallback to tree/mod.rs
**Bug**: compact_boundary messages have `parentUuid: null` but `logicalParentUuid` preserves the logical connection. Tree view breaks at compaction points. (Research #2)
- [x] in tree building: when `parentUuid` is null, fall back to `logicalParentUuid`
- [x] add `extract_logical_parent_uuid` to session.rs
- [x] write test with compact_boundary that uses logicalParentUuid
- [x] run tests — must pass before next task

### Task 8: Add legacy progress bridge to fork.rs and tree/mod.rs
**Bug**: Old transcripts have `type: "progress"` records in the parentUuid chain. Claude Code bridges through them; ccs breaks the chain. (Research #4)
- [x] in `build_chain_from_tip`: when encountering progress type, record uuid→parentUuid but don't set as last_uuid
- [x] in tree building: bridge through progress records (chain-resolve parentUuid through consecutive progress entries)
- [x] write test `test_build_chain_bridges_through_progress`
- [x] write test `test_tree_bridges_through_progress`
- [x] run tests — must pass before next task

### Task 9: Handle system/attachment types in tree view
**Bug**: Tree only processes user/assistant/summary. Misses system (compact_boundary, microcompact_boundary) and attachment records that participate in the DAG. (Research #5)
- [x] in tree building: include `system` and `attachment` type records as DAG participants
- [x] ensure compact_boundary system records are visually distinct or filtered appropriately
- [x] write test with system and attachment records in chain
- [x] run tests — must pass before next task

### Task 10: Stop including metadata-only lines in fork output
**Bug**: `create_fork` copies lines without `uuid` (summaries, snapshots, etc.) and rewrites sessionId. These are keyed by original sessionId/leafUuid and become orphaned.
- [x] in `create_fork`: skip lines without uuid (metadata records)
- [x] write test `test_create_fork_skips_metadata_without_uuid`
- [x] update existing fork tests if they assert metadata presence
- [x] run tests — must pass before next task

### Task 11: Skip empty session files ⚠ НЕ РЕАЛИЗОВАНО
**Bug found live**: `ccs` showed a session from a 1-byte (empty) JSONL file, then `claude --resume` failed with "No conversation found". Claude Code's `resolveSessionFilePath` skips files where `s.size == 0`.
- [ ] in recent.rs session scanning: skip files with size ≤ 1
- [ ] in search: skip empty files
- [ ] write test for empty file handling
- [ ] run tests — must pass before next task

> На момент архивации производственный фильтр по размеру в `src/recent.rs` отсутствует. Если симптом проявится — открыть отдельный план.

---

## Phase 3: P1 improvements

### Task 12: Extract aiTitle and lastPrompt from session metadata
**Feature**: Claude Code stores `ai-title`, `last-prompt` as tail metadata entries. These give better session descriptions than parsing full content. (Research #6)

Current title priority in ccs: `agentName > customTitle > aiTitle > summary > lastPrompt > firstUserMessage`
- [x] in recent.rs: extract `aiTitle` from tail (type="ai-title", field "aiTitle")
- [x] in recent.rs: extract `lastPrompt` from tail (type="last-prompt", field "lastPrompt")
- [x] use `lastPrompt` as better summary fallback (after customTitle, before firstPrompt)
- [x] write tests for new metadata extraction
- [x] run tests — must pass before next task

### Task 13: Extract tag and pr-link metadata ⚠ НЕ РЕАЛИЗОВАНО
**Feature**: Claude Code stores tags and PR links. Useful for display and filtering. (Research #6)
- [ ] in recent.rs: extract `tag` from tail (type="tag")
- [ ] in recent.rs: extract `prNumber`/`prUrl` from tail (type="pr-link")
- [ ] display tag and PR info in TUI session list if present
- [ ] write tests
- [ ] run tests — must pass before next task

> На момент архивации tail-extraction для `tag` / `pr-link` отсутствует в `src/recent.rs`. Если понадобится — отдельный план.

### Task 14: Final verification
- [x] `cargo fmt --check` — must pass
- [x] `cargo clippy --all-targets --all-features -- -D warnings` — must pass
- [x] `cargo test` — all tests must pass
- [x] verify no orphaned imports or dead code warnings

---

## Technical Details

**isSidechain filter**: `json.get("isSidechain").and_then(|v| v.as_bool()).unwrap_or(false)`

**compact_boundary detection**: `type == "system" && subtype == "compact_boundary"`. On match, clear all accumulated state.

**logicalParentUuid**: `json.get("logicalParentUuid").and_then(|v| v.as_str())` — fallback when parentUuid is null at compaction points.

**Progress bridge**: progress records have uuid and parentUuid but type="progress". Chain-resolve through consecutive progress entries so later messages pointing at progress bridge to the nearest non-progress ancestor.

**Metadata skip in fork**: Lines without `uuid` are metadata entries (summary, custom-title, tag, etc.). They reference original sessionId — broken with new fork ID.

**Empty file skip**: Claude Code uses `s.size > 0` check in `resolveSessionFilePath`.

## Deferred (P2, separate plan)

Items from research doc that are valuable but out of scope for this plan:
- Parallel tool result recovery (recoverOrphanedParallelToolResults) — complex DAG repair
- Head/tail 64KB lite reading optimization (LITE_READ_BUF_SIZE) — performance
- Subagent transcript discovery and parent→agent linking
- Thinking block search
- Image data skipping in search
- isMeta/isCompactSummary/isVirtual visual filtering
- Worktree-aware session discovery
- --fork-session flag usage (evaluate if it can simplify fork.rs)

## Post-Completion

**Manual verification:**
- Open ccs TUI, search for a session, enter tree mode, resume from a non-latest branch — verify fork works
- Test with a large session (>5MB) that has compact_boundary markers
- Verify empty sessions no longer appear in session list
