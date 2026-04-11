# CCS Architecture Refactoring Plan

## Executive Summary

Кодовая база ccs (16,200 строк, 366 unit-тестов) имеет три структурных проблемы, усиливающих друг друга:

1. **Dependency cycle** — `search` импортирует `resume`, `resume` импортирует `search`
2. **Duplicated core logic** — DAG chain-walking (3 реализации), content extraction (3 реализации), record parsing (ad-hoc `serde_json::Value` в 6+ файлах)
3. **Monolithic TUI state** — `App` struct с 40+ pub полями, render мутирует state, key dispatch — неструктурированный каскад

Рефакторинг разбит на **4 фазы** с чёткими зависимостями. Фаза 1 — обязательный prerequisite. Фазы 2 и 3 параллельны. Фаза 4 — cleanup.

```
Phase 1: Dep-cycle fix          ← MUST LAND FIRST
    |
    +--→ Phase 2: TUI decomposition    (parallel)
    |
    +--→ Phase 3: Session data layer   (parallel)
                                        
Phase 4: Integration cleanup     ← after both 2 and 3
```

**Estimated scope**: ~1,200–1,600 lines net-changed, 14–16 files, zero new external dependencies.

---

## Phase 1 — Break Dependency Cycle

**PR**: `fix: remove search↔resume import cycle`  
**Estimated size**: 4–5 files, <100 lines changed  
**Risk**: LOW

### Problem

```
search/ripgrep.rs  →  resume/mod.rs       (resolve_parent_session)
resume/launcher.rs →  search/message.rs   (Message::extract_content)
```

Two subsystems that should be independent have a bidirectional dependency cycle.

### Changes

#### Step 1: Move `resolve_parent_session` to `session.rs`

`resume/mod.rs:23–61` → `session.rs` (after `find_session_file_in_paths`).

This is a pure filesystem path-resolution function with zero resume logic. It belongs alongside `extract_session_id`, `find_session_file_in_paths`.

```
src/session.rs          +resolve_parent_session fn (+4 test cases from resume/mod.rs)
src/resume/mod.rs       -resolve_parent_session fn, -test wrapper, -4 test cases
                        +use crate::session::resolve_parent_session
src/search/ripgrep.rs   use crate::session::resolve_parent_session (was resume)
```

#### Step 2: Inline `extract_content` in `launcher.rs`

Add local `extract_content_from_value(raw: &serde_json::Value) -> String` in `launcher.rs` (45 lines, mirroring `Message::extract_content`). Remove `use crate::search::Message`.

**Alternative** (preferred for long-term): Move `extract_content` to `session.rs` as a shared free function `extract_message_content(raw: &serde_json::Value) -> String`. Both `Message::extract_content` and `launcher.rs` delegate to it. Zero duplication.

#### Step 3: Fix `SessionSource` import inconsistency

`main.rs` lines 562, 574: `ccs::search::SessionSource` → `ccs::session::SessionSource`.

#### Step 4: Replace `BackgroundSearchResult` 5-tuple

```rust
// Before (state.rs:148–154):
pub(crate) type BackgroundSearchResult = (u64, String, Vec<String>, bool, Result<SearchResult, String>);

// After:
pub(crate) struct BackgroundSearchResult {
    pub seq: u64,
    pub query: String,
    pub paths: Vec<String>,
    pub use_regex: bool,
    pub result: Result<crate::search::SearchResult, String>,
}
```

Update 6 construction sites (5 in state.rs, 1 in search_mode.rs) and 1 destructuring site.

### Verification

```bash
cargo build && cargo test && cargo clippy --all-targets --all-features -- -D warnings
```

---

## Phase 2 — TUI Architecture Decomposition

**PR**: `refactor: decompose App struct into domain sub-structs`  
**Estimated size**: 5–6 files, 300–500 lines changed  
**Risk**: MEDIUM  
**Dependency**: Phase 1 merged

### New Types

#### `ResumeTarget` — eliminates 4-Options invariant violation

```rust
// src/tui/state/resume.rs
pub struct ResumeTarget {
    pub session_id: String,
    pub file_path: String,
    pub source: SessionSource,
    pub uuid: Option<String>,
    pub query: String,
}
```

Replaces `resume_id`, `resume_file_path`, `resume_source`, `resume_uuid` — four separate `Option<String>` that `into_outcome()` silently drops if any is None.

#### `InputState` — cursor invariant enforced in one place

```rust
// src/tui/state/input.rs
pub struct InputState {
    text: String,
    cursor_pos: usize,  // always valid UTF-8 boundary
}
// Methods: push_char, backspace, delete_forward, move_left/right, 
// move_word_left/right, delete_word_left/right, move_home/end, clear
```

Currently cursor logic spread across 8+ methods in state.rs and search_mode.rs.

#### `KeyAction` enum + `classify_key` — testable dispatch

```rust
// src/tui/dispatch.rs
pub enum KeyAction {
    Quit, Cancel, InsertChar(char), Backspace, BackspaceWord,
    DeleteForward, DeleteWordForward,
    MoveCursorLeft, MoveCursorRight, MoveCursorWordLeft, MoveCursorWordRight,
    MoveCursorHome, MoveCursorEnd, ClearInput,
    NavigateUp, NavigateDown, ExpandGroup, CollapseGroup,
    TogglePreview, Confirm,
    ToggleRegex, ToggleProjectFilter, ToggleAutomationFilter, EnterTreeMode,
    TreeUp, TreeDown, TreeJumpPrevBranch, TreeJumpNextBranch,
    TreePreview, TreeConfirm, ExitTree,
    Noop,
}

pub fn classify_key(key: KeyEvent, tree_mode: bool, expanded: bool, 
                     input_at_end: bool, input_empty: bool) -> KeyAction;
```

Replaces the unstructured `if/continue/match` cascade in `main.rs:157–295`. Pure function — 30+ test cases with no terminal setup.

#### `AppView` — pure render

```rust
// src/tui/view.rs
pub struct AppView<'a> {
    pub input: &'a InputState,
    pub search: &'a SearchState,
    pub tree: &'a TreeState,
    // ... all read-only references
}
```

Render signature: `render(frame: &mut Frame, view: &AppView)` — no `&mut App`. Eliminates the `tree_visible_height` mutation-in-render side effect.

### Migration Steps

1. **InputState extraction** (lowest risk) — extract text+cursor into InputState
2. **ResumeTarget** (fixes bug class) — replace 4 Options with struct
3. **KeyAction dispatch** (highest test value) — add classify_key alongside old cascade, then remove old
4. **SearchState + TreeState extraction** — move field groups to sub-structs
5. **Render purity** — AppView projection, remove `&mut App` from render
6. **extract_summary decomposition** — scan_head/scan_tail/scan_middle as independent functions

### Verification

```bash
cargo test tui && cargo test render_snapshots
cargo run  # manual smoke test: search, tree mode, recent sessions
```

---

## Phase 3 — Session Data Layer

**PR**: `refactor: unified session record taxonomy and DAG engine`  
**Estimated size**: 4–6 files, 200–400 lines changed  
**Risk**: MEDIUM  
**Dependency**: Phase 1 merged  
**Parallel with**: Phase 2

### New Modules

#### `src/session/record.rs` — Typed Session Records

```rust
pub enum SessionRecord {
    Message { uuid, parent_uuid, session_id, timestamp, role, content, branch, is_sidechain, is_meta },
    Summary { uuid, session_id, text, leaf_uuid },
    CustomTitle { session_id, title },
    AiTitle { session_id, title },
    AgentName { session_id, name },
    LastPrompt { session_id, text },
    CompactBoundary { uuid, logical_parent_uuid, session_id, timestamp },
    Other { uuid, parent_uuid, session_id, timestamp, record_type, is_sidechain },
    Metadata { record_type, raw },
}

pub enum ContentBlock { Text(String), ToolUse { name, input_json }, ToolResult { content }, Thinking(String) }
pub enum ContentMode { Full, Preview { max_chars: usize }, TextOnly }
pub enum MessageRole { User, Assistant }

impl SessionRecord {
    pub fn from_jsonl(line: &str) -> Option<Self>;
    pub fn render_content(blocks: &[ContentBlock], mode: ContentMode) -> String;
    pub fn dag_uuid(&self) -> Option<&str>;
    pub fn dag_parent_uuid(&self) -> Option<&str>;
    pub fn content_blocks(&self) -> Option<&[ContentBlock]>;
}
```

Replaces all ad-hoc `serde_json::Value` field access by string key across 6+ files.

#### `src/dag/mod.rs` — Unified DAG Engine

```rust
pub enum TipStrategy { LastAppended, MaxTimestamp }
pub enum DisplayFilter { Standard, MessagesOnly }

pub struct SessionDag { /* entries, parent_set, displayable_order */ }

impl SessionDag {
    pub fn from_file(path: &str, filter: DisplayFilter) -> Result<Self, String>;
    pub fn from_records(records: impl Iterator<Item = (usize, &SessionRecord)>, filter: DisplayFilter) -> Self;
    pub fn tip(&self, strategy: TipStrategy) -> Option<&str>;
    pub fn chain_from(&self, tip: &str) -> HashSet<String>;
    pub fn is_on_latest_chain(&self, uuid: &str, strategy: TipStrategy) -> bool;
}
```

Replaces 3 duplicate implementations:
- `resume/fork.rs`: `parse_dag()` + `build_chain()` + `find_tip()`
- `tree/mod.rs`: `build_latest_chain()`  
- `recent.rs`: `build_latest_chain()`

**Critical design decision**: Tip-selection strategy DIVERGES intentionally between callers. `fork.rs`/`recent.rs` use `LastAppended` (append order, O(n), no timestamp parsing). `tree/mod.rs` uses `MaxTimestamp` (handles clock-skew in visual tree). Both strategies are preserved as explicit enum variants.

### Content Extraction Separator

> **KEY DECISION** (must be stated explicitly in PR description):
> 
> `recent.rs` joins with `" "` (space) — one-line summary display.  
> `search/message.rs` joins with `"\n"` (newline) — multi-line searchable content.  
> `tree/mod.rs` joins with `" "` + truncates — visual preview.
>
> Resolution: `ContentMode` enum encodes the separator in each variant. No behavior change.

### Migration Steps

1. **Add `session/record.rs`** — SessionRecord + ContentMode (no callers changed)
2. **Add `dag/mod.rs`** — SessionDag with both strategies (no callers changed)
3. **Migrate `resume/fork.rs`** — replace DagInfo/parse_dag with SessionDag
4. **Migrate `recent.rs`** — replace build_latest_chain + extract_text_content
5. **Migrate `tree/mod.rs`** — replace local build_latest_chain + extract_preview
6. **Simplify `search/message.rs`** — Message::from_jsonl delegates to SessionRecord

### Verification

```bash
cargo test recent && cargo test search && cargo test tree_parsing
cargo test resume  # fork.rs tests must pass unchanged
```

---

## Phase 4 — Integration Cleanup

**PR**: `chore: post-refactor import cleanup`  
**Estimated size**: 1–2 files, <50 lines  
**Dependency**: Phase 2 AND Phase 3 merged

- Remove `pub use message::SessionSource` from `search/mod.rs`
- Update remaining `crate::search::SessionSource` imports → `crate::session::SessionSource`
- Remove any dead-code warnings from Phase 2/3
- Run final `cargo clippy`

---

## Safety Net

Run before EVERY PR:

```bash
# 1. Format
cargo fmt --check

# 2. No warnings
cargo clippy --all-targets --all-features -- -D warnings

# 3. All tests pass, count must not decrease
cargo test 2>&1 | grep -E "^test result"
# Expected: "test result: ok. N passed; 0 failed" where N >= 366

# 4. Binary runs
cargo run -- --help | grep -c "Usage"

# 5. Integration tests
cargo test --test tree_parsing
cargo test --test resume_resolution
cargo test --test render_snapshots
cargo test --test cli_search
```

---

## PR Strategy

| PR | Phase | Files | Lines | Review |
|----|-------|-------|-------|--------|
| PR-1 | Phase 1: dep-cycle | 4–5 | <100 | 30 min |
| PR-2 | Phase 2: TUI decomp | 5–6 | 300–500 | 90 min |
| PR-3 | Phase 3: session data | 4–6 | 200–400 | 60 min |
| PR-4 | Phase 4: cleanup | 1–2 | <50 | 15 min |

PR-1 is prerequisite. PR-2 and PR-3 can be reviewed in parallel. PR-4 follows both.

**Do NOT combine Phase 2 and Phase 3** — they touch overlapping conceptual territory but different file domains.

## Out of Scope

- `src/update.rs` — no structural problems
- `src/resume/launcher.rs` — platform-specific exec, correct and tested
- `src/resume/path_codec.rs` — 17 tests, correct behavior
- `tests/fixtures/*.jsonl` — must remain unchanged
- `Cargo.toml` — no new dependencies required
- `.github/workflows/ci.yml` — CI adequate
