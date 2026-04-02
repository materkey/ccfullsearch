# P1: UX and compatibility improvements

## Overview
- Respect `CLAUDE_CONFIG_DIR` env var and add Linux Desktop path support
- Show meaningful session titles (custom/AI/agent names) instead of raw first prompt
- Make thinking blocks searchable
- Filter agent/subagent files from search results and recent sessions
- Fix cross-project resume by passing file path instead of session ID
- Deduplicate sessions across worktrees
- Source: `docs/plans/2026-04-02-claude-code-source-research.md` (P1 items)

## Context (from discovery)
- **Claude Code source**: `~/projects/claude-code/src/utils/envUtils.ts`, `types/logs.ts`, `listSessionsImpl.ts`
- **ccfullsearch files**: `src/lib.rs`, `src/recent.rs`, `src/search/message.rs`, `src/search/ripgrep.rs`, `src/tree/mod.rs`, `src/resume/launcher.rs`
- **Key gaps found**:
  1. `lib.rs:13` — only checks `CCFS_SEARCH_PATH`, ignores `CLAUDE_CONFIG_DIR` (Claude Code uses it in `envUtils.ts:10`)
  2. `lib.rs:26` — Desktop path macOS-only, no Linux `~/.config/Claude/...`
  3. `recent.rs` — summary is first user message; never reads `custom-title`, `ai-title`, `agent-name`, `last-prompt` metadata records from JSONL tail
  4. `message.rs:109` — `extract_content()` match has `_ => {}` fallthrough, drops `"thinking"` blocks
  5. `ripgrep.rs` — no filtering of `agent-*.jsonl` or `subagents/` path matches
  6. `recent.rs:624` — `collect_jsonl_recursive()` skips `agent-*` prefix but not files inside `subagents/` dirs
  7. `resume/launcher.rs` — passes session ID to `claude --resume`; fails for cross-project sessions
  8. `recent.rs` — no dedup by session_id across project dirs

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

### Task 1: Support CLAUDE_CONFIG_DIR env var and Linux Desktop path
`CLAUDE_CONFIG_DIR` overrides `~/.claude` as the Claude config root. Linux Desktop uses `~/.config/Claude/local-agent-mode-sessions/`. Currently `lib.rs` hardcodes both.
- [x] in `get_search_paths()` (`src/lib.rs`): when `CCFS_SEARCH_PATH` is not set, read `CLAUDE_CONFIG_DIR` env var as base instead of hardcoded `~/.claude` — `std::env::var("CLAUDE_CONFIG_DIR").ok().unwrap_or_else(|| home.join(".claude"))`
- [x] add Linux Desktop path: `~/.config/Claude/local-agent-mode-sessions/` (only if dir exists)
- [x] keep macOS Desktop path too (only if dir exists) — use `Path::exists()` check
- [x] write test `test_search_paths_respects_claude_config_dir` — set env var, verify it's used as base
- [x] write test `test_search_paths_default_without_env` — verify ~/.claude/projects used by default
- [x] run tests — must pass before next task

### Task 2: Extract session titles from JSONL tail metadata
Claude Code writes metadata records (`custom-title`, `ai-title`, `agent-name`, `last-prompt`) as separate JSONL entries (no uuid) at the end of the file. Title priority: agentName > customTitle > aiTitle > summary > lastPrompt > firstUserMessage.
- [x] in `find_summary_from_tail_with_chain()` (`src/recent.rs`): while scanning tail, check record types: `custom-title` → field `customTitle`, `ai-title` → field `aiTitle`, `agent-name` → field `agentName`, `last-prompt` → field `lastPrompt`
- [x] store extracted metadata in local variables during tail scan
- [x] after scan, apply priority: if agentName found → use it; else customTitle; else aiTitle; else existing summary logic; else lastPrompt; else firstUserMessage
- [x] write test `test_tail_extracts_custom_title` — JSONL with custom-title record → summary is customTitle
- [x] write test `test_tail_extracts_ai_title` — JSONL with ai-title record → summary is aiTitle
- [x] write test `test_title_priority_custom_over_ai` — both present → customTitle wins
- [x] write test `test_title_priority_agent_name_highest` — agentName > customTitle > aiTitle
- [x] run tests — must pass before next task

### Task 3: Add thinking blocks to search content extraction
`"thinking"` content blocks (extended reasoning) use field `"thinking"` not `"text"`. Currently silently dropped by `_ => {}` fallthrough in both `extract_content()` and `extract_preview()`.
- [x] in `Message::extract_content()` (`src/search/message.rs:85-109`): add match arm `"thinking" => { if let Some(t) = item.get("thinking").and_then(|t| t.as_str()) { parts.push(t.to_string()); } }`
- [x] in `extract_preview()` (`src/tree/mod.rs`): add same match arm for `"thinking"` in the content array iteration
- [x] write test `test_extract_content_thinking_block` — message with thinking block → content includes thinking text
- [x] write test `test_extract_preview_thinking_block` — preview includes thinking text
- [x] run tests — must pass before next task

### Task 4: Filter agent/subagent files from search and recent sessions
ripgrep search matches agent-*.jsonl files (noisy duplicates). Recent sessions walker skips `agent-*` prefix but not files inside `<sessionId>/subagents/` directories.
- [x] in `src/search/ripgrep.rs`: after collecting matches, filter out matches where file path contains `/subagents/` or filename starts with `agent-`
- [x] in `collect_jsonl_recursive()` (`src/recent.rs:624`): add check to skip files where any path component is `subagents`
- [x] write test `test_search_filters_agent_files` — match from agent-*.jsonl is excluded
- [x] write test `test_search_filters_subagent_files` — match from */subagents/*.jsonl is excluded
- [x] write test `test_recent_skips_subagent_dir` — files in subagents/ dir not in recent list
- [x] run tests — must pass before next task

### Task 5: Cross-project resume via file path
When resuming session from different project dir, `claude --resume <session-id>` fails because Claude searches only in current project dir. Claude accepts `.jsonl` file paths for `--resume`.
- [x] in resume code (`src/resume/launcher.rs` or `src/resume/mod.rs`): change resume command to pass the `.jsonl` file path directly instead of session ID
- [x] verify the file path is absolute (it should be, ccfullsearch stores absolute paths)
- [x] write test `test_resume_command_uses_file_path` — verify the exec'd command includes file path not session ID
- [x] run tests — must pass before next task

### Task 6: Deduplicate sessions across worktrees
Same session can appear in multiple `~/.claude/projects/` dirs when using git worktrees. Claude Code deduplicates by sessionId, keeping newest mtime.
- [x] in `collect_recent_sessions()` or its caller (`src/recent.rs`): after collecting all sessions, group by session_id, keep only the entry with newest `timestamp` (or file mtime)
- [x] write test `test_dedup_sessions_keeps_newest` — two sessions with same ID, different timestamps → only newest kept
- [x] write test `test_dedup_sessions_different_ids_preserved` — different session IDs both kept
- [x] run tests — must pass before next task

### Task 7: Final verification
- [x] `cargo fmt --check` — must pass
- [x] `cargo clippy --all-targets --all-features -- -D warnings` — must pass
- [x] `cargo test` — all tests must pass
- [x] verify no orphaned imports or dead code warnings

## Technical Details

**CLAUDE_CONFIG_DIR** — Claude Code reads this in `envUtils.ts:10`: `process.env.CLAUDE_CONFIG_DIR ?? join(homedir(), '.claude')`. ccfullsearch should do the same for `projects/` subdir.

**Metadata record types** — appended to JSONL tail without uuid:
- `{"type":"custom-title","customTitle":"My session","sessionId":"..."}` — user-set via /rename
- `{"type":"ai-title","aiTitle":"Debugging auth flow","sessionId":"..."}` — AI-generated
- `{"type":"agent-name","agentName":"researcher","sessionId":"..."}` — swarm agent name
- `{"type":"last-prompt","lastPrompt":"Fix the login bug","sessionId":"..."}` — most recent user prompt

**Thinking blocks** — stored inline in assistant `message.content[]`:
```json
{"type":"thinking","thinking":"The user wants...","signature":"EvACCkY..."}
```

**Resume with file path** — Claude Code accepts `.jsonl` path for `--resume` (see `cli/print.ts:776`). Example: `claude --resume /Users/foo/.claude/projects/-Users-foo-project-A/abc123.jsonl`

## Post-Completion

**Manual verification:**
- Set `CLAUDE_CONFIG_DIR=/tmp/test-claude` and verify ccs finds sessions there
- Open ccs TUI, verify sessions show custom titles instead of raw prompts
- Search for a term that exists only in thinking blocks — verify it's found
- Verify agent-*.jsonl files don't appear as separate search results
- Resume a session from a different project directory — verify it works
- If using worktrees: verify no duplicate sessions in recent list
