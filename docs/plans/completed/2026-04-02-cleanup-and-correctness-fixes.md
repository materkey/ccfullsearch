# Cleanup + correctness fixes: align ccfullsearch with Claude Code session format

## Overview
- Remove dead "linearization" code that was already disabled (`#[cfg(test)]` only)
- Fix correctness bugs in DAG building, leaf finding, chain walking, and fork creation to match Claude Code's actual behavior
- Investigate `--fork-session` CLI flag as potential replacement for our fork.rs
- Sources: old plan `2026-04-02-remove-linearization-improve-resume.md` + research `2026-04-02-claude-code-source-research.md`

## Context (from discovery)
- **ccfullsearch files**: `src/session.rs`, `src/recent.rs`, `src/search/message.rs`, `src/resume/launcher.rs`, `src/resume/fork.rs`, `src/tree/mod.rs`
- **Claude Code source**: `~/projects/claude-code/src/utils/sessionStorage.ts`, `sessionStoragePortable.ts`, `conversationRecovery.ts`, `types/logs.ts`
- **Key correctness issues found**:
  1. Leaf finding uses `last_uuid` (last line) instead of computing terminal messages
  2. No `logicalParentUuid` — tree breaks at compact_boundary points
  3. No `isSidechain` filtering — agent sidechains can hijack "latest" selection
  4. No `compact_boundary` handling — pre-boundary records pollute chain
  5. No legacy `progress` type bridge — old transcripts break chain walk
  6. Fork includes orphaned metadata lines
  7. `--fork-session` flag exists in Claude Code but ccfullsearch doesn't use it

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests**
- **CRITICAL: all tests must pass before starting next task**
- Run `cargo test` after each change, `cargo clippy` at the end

## Testing Strategy
- **Unit tests**: inline `#[cfg(test)]` modules in each source file
- Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Phase 1: Remove dead linearization code (Tasks 1-4)

### Task 1: Remove synthetic linear API from session.rs
- [x] remove `SYNTHETIC_LINEAR_FIELD` constant (line 87)
- [x] remove `mark_synthetic_linear_record()` function (lines 133-136)
- [x] remove `is_synthetic_linear_record()` function (lines 139-143)
- [x] remove test `test_mark_synthetic_linear_record` (lines 280-284)
- [x] remove test `test_is_synthetic_linear_record_false_by_default` (lines 287-290)
- [x] run tests — must pass before next task

### Task 2: Remove synthetic filters from recent.rs
- [x] remove all `if session::is_synthetic_linear_record(&json) { continue; }` occurrences (lines 122, 186, 293, 399, 531)
- [x] simplify combined condition at line 344 — keep only the record type check
- [x] remove test `test_extract_summary_returns_none_for_synthetic_linear_bootstrap_only`
- [x] remove test `test_extract_summary_keeps_resumed_synthetic_branch_visible`
- [x] run tests — must pass before next task

### Task 3: Remove synthetic filter from search/message.rs
- [x] remove `if session::is_synthetic_linear_record(&json) { return None; }` (lines 28-30)
- [x] verify `use crate::session;` import is still used (yes — `extract_record_type` etc.)
- [x] remove test `test_skip_synthetic_linear_message`
- [x] run tests — must pass before next task

### Task 4: Remove linearization code from resume/launcher.rs
- [x] remove `SYNTHETIC_SOURCE_PATH_FIELD` constant (line 18, `#[cfg(test)]`)
- [x] remove `SessionAnalysis.is_linear` field (line 29) and all `#[cfg(test)]` computation blocks: variables (lines 44-48), UUID tracking (lines 57-61, 74-76), chain walk (lines 107-123), struct literal field (lines 131-132)
- [x] remove `#[cfg(test)]` functions: `cleanup_legacy_synthetic_sessions`, `disposable_synthetic_session_matches_source`, `create_linear_session`, `synthetic_source_fingerprint`, `is_synthetic_linear_session_file`, `stable_synthetic_session_id`, `fnv1a64`
- [x] remove tests: `test_analyze_session_treats_interleaved_metadata_as_linear`, `test_analyze_session_still_detects_real_branch_with_metadata_nodes`
- [x] remove all `test_create_linear_session_*` tests (6 tests)
- [x] remove all `test_cleanup_legacy_synthetic_sessions_*` tests (3 tests)
- [x] run tests — must pass before next task

### Phase 2: Fork correctness (Tasks 5-7)

### Task 5: Add isSidechain filter to fork.rs
Sidechain messages (`isSidechain: true`) are subagent messages that should NOT participate in the main conversation chain. Claude Code filters them when finding leaves.
- [x] in `build_chain_from_tip`: after parsing json, skip records where `isSidechain` is true — `json.get("isSidechain").and_then(|v| v.as_bool()).unwrap_or(false)`
- [x] in `latest_tip_uuid`: same isSidechain filter
- [x] in `create_fork`: same isSidechain filter when building uuid_to_parent map
- [x] write test `test_build_chain_ignores_sidechain_records` — sidechain entry at end should not become tip
- [x] write test `test_create_fork_ignores_sidechain_records`
- [x] run tests — must pass before next task

### Task 6: Add compact_boundary handling to fork.rs
`compact_boundary` is a system message with `parentUuid: null` that marks a compaction point. Records before it should be ignored in chain building.
- [x] in `build_chain_from_tip`: detect `{"type":"system","subtype":"compact_boundary"}`, on match clear `uuid_to_parent` and `last_uuid` — only post-boundary records matter
- [x] in `latest_tip_uuid`: same compact_boundary reset
- [x] in `create_fork`: same compact_boundary reset (clear uuid_to_parent, skip pre-boundary lines)
- [x] write test `test_build_chain_resets_on_compact_boundary` — pre-boundary UUIDs not in chain
- [x] write test `test_create_fork_handles_compact_boundary`
- [x] run tests — must pass before next task

### Task 7: Stop including metadata-only lines in fork output
Lines without `uuid` are metadata (summary, custom-title, tag, etc.) keyed by original sessionId — they become orphaned when sessionId is rewritten.
- [x] in `create_fork`: change `None =>` arm (lines without uuid) from including as-is to skipping
- [x] write test `test_create_fork_skips_metadata_without_uuid` — verify summary/snapshot lines not in output
- [x] update existing test `test_create_fork_from_non_latest_branch` if it asserts metadata presence
- [x] run tests — must pass before next task

### Phase 3: DAG correctness in tree/mod.rs (Tasks 8-11)

### Task 8: Fix leaf finding — use terminal messages instead of last_uuid
**Bug**: `build_latest_chain()` (tree/mod.rs:429-444) uses `last_uuid` (last line with uuid) as the tip. But the last line could be a metadata record, attribution-snapshot, or sidechain. Claude Code computes terminal messages (uuid NOT in any parentUuids set) and picks the latest user/assistant among them.
- [x] rewrite `build_latest_chain()`: compute `parent_uuids` set from all nodes, find terminals (uuid not in parent_uuids), filter to user/assistant with `!isSidechain`, pick the one with latest timestamp as tip
- [x] add `isSidechain` check when collecting nodes in `from_file()` — read `json.get("isSidechain")` and store in `DagNode`
- [x] write test `test_latest_chain_ignores_trailing_metadata` — metadata uuid at end should not become tip
- [x] write test `test_latest_chain_ignores_sidechain_leaf` — isSidechain leaf should not become tip
- [x] write test `test_latest_chain_picks_latest_terminal` — with multiple terminal branches, picks latest timestamp
- [x] run tests — must pass before next task

### Task 9: Add logicalParentUuid support
**Bug**: `compact_boundary` messages have `parentUuid: null` but `logicalParentUuid` preserves the logical link. Without this, the tree breaks at compaction points — post-compact messages become orphaned roots.
- [x] in `from_file()`: after reading `parent_uuid`, if it's `None`, check `json.get("logicalParentUuid")` and use it as fallback
- [x] in `find_displayable_parent()`: same fallback (already walks parent_uuid, will now traverse through logical links)
- [x] write test `test_logical_parent_uuid_bridges_compact_boundary` — compact_boundary with logicalParentUuid should connect pre/post-compact chains
- [x] run tests — must pass before next task

### Task 10: Add legacy progress type bridge
**Bug**: Old transcripts have `progress` type records that participate in the parentUuid chain. Claude Code bridges across them — if a message's parentUuid points at a `progress` record, it rewires to that progress record's parentUuid. ccfullsearch already has progress nodes in the DAG (test fixtures use them), but `build_display_graph()` skips them because they have no role/content_preview. The issue is when the ONLY path through the chain goes via a progress node — `find_displayable_parent()` already walks through non-displayable nodes, so this may already work.
- [x] verify `find_displayable_parent()` correctly walks through progress nodes (read code at tree/mod.rs:281-315)
- [x] write test `test_display_graph_bridges_progress_nodes` — user(u1) -> progress(p1) -> user(u2) should show u1 as parent of u2 in display
- [x] if bridge doesn't work, add explicit progress bridging in `from_file()` similar to Claude Code's approach
- [x] run tests — must pass before next task

### Task 11: Handle system/attachment records in DAG
**Context**: `system` and `attachment` records have uuid/parentUuid and participate in the DAG chain. ccfullsearch already includes them in `nodes` HashMap (line 141), but they're not displayable (no role/content_preview) — they're invisible bridges. `find_displayable_parent()` walks through them. This should already work correctly.
- [x] verify system nodes participate correctly in the DAG (they should be bridges, not displayed)
- [x] write test `test_system_nodes_bridge_correctly` — user(u1) -> system(s1) -> user(u2), display graph should show u1 as parent of u2
- [x] write test `test_attachment_nodes_bridge_correctly` — similar test with attachment type
- [x] run tests — must pass before next task

### Phase 4: Investigate --fork-session (Task 12)

### Task 12: Investigate `claude --fork-session` as fork.rs replacement
Claude Code has `--fork-session` flag: creates a new session ID when resuming. If it works for resuming from a non-latest branch, our fork.rs could be simplified.
- [x] test manually: run `claude --resume <session-id> --fork-session` and verify behavior
- [x] test: does it work with a specific message? (we resume from a non-latest branch tip)
- [x] document findings as comments in `resume/mod.rs`
- [x] if --fork-session can replace our fork: add a TODO/note for future simplification (separate plan)
- [x] if it can't: document WHY in comments (e.g., "Claude's --fork-session only forks from latest leaf, not arbitrary branch tips")
- [x] run tests — must pass before next task

### Task 13: Final verification
- [x] `cargo fmt --check` — must pass
- [x] `cargo clippy --all-targets --all-features -- -D warnings` — must pass
- [x] `cargo test` — all tests must pass
- [x] verify no orphaned imports or dead code warnings

## Technical Details

**isSidechain filter** — check `json.get("isSidechain").and_then(|v| v.as_bool()).unwrap_or(false)`. Claude Code sets this on subagent messages; they should not participate in the main chain.

**compact_boundary detection** — check `type == "system" && subtype == "compact_boundary"`. On match, clear all accumulated state. Claude Code does this in `readTranscriptForLoad` (sessionStoragePortable.ts:717-793).

**logicalParentUuid** — when `parentUuid` is null (as in compact_boundary), `logicalParentUuid` preserves the logical link to the pre-boundary chain. Used for tree visualization continuity.

**Terminal message = leaf** — a message whose uuid does NOT appear as any other message's parentUuid. Among terminals, the latest user/assistant (non-sidechain) is the main conversation tip.

**Progress bridge** — `progress` type records are legacy ephemeral entries (hook_progress, bash_progress, etc.) that may sit in the parentUuid chain. Modern Claude Code no longer writes them, but old transcripts have them. Claude Code bridges across by remapping parentUuid references.

**Metadata skip in fork** — lines without `uuid` field are metadata entries (summary, custom-title, tag, file-history-snapshot, agent-name, etc.). They reference the original sessionId and become broken when sessionId is rewritten. Claude Code's `loadTranscriptFile` keys these by sessionId, so they won't match the fork's new ID anyway.

## Post-Completion

**Manual verification:**
- Open ccs TUI, search for a session with sidechain messages, enter tree mode — verify sidechain doesn't hijack latest chain
- Test with a session that has compact_boundary markers — verify tree doesn't break
- Resume from a non-latest branch — verify fork works correctly
- Test with an old session that has `progress` type records
- Test `--fork-session` finding (Task 12)

**Future work (separate plans):**
- P1: Metadata extraction (custom-title, ai-title, tag, summary, last-prompt, pr-link) for richer session list
- P1: Thinking block search in extract_content
- P1: Subagent transcript discovery (`<sessionId>/subagents/agent-<agentId>.jsonl`)
- P2: Image data skipping in search, isMeta/isVirtual flags, worktree-aware discovery
- See full list in `2026-04-02-claude-code-source-research.md`
