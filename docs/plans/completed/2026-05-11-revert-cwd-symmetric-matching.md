# Revert symmetric cwd matching in `session_matches_project`

## Overview

В предыдущей итерации
(`docs/plans/completed/2026-05-11-recent-sessions-codex-ctrl-a.md`) был введён
симметричный предикат `paths_share_project(a, b)` для cwd-сравнения. Live-тест
показал реальный bug: Codex-сессии с `cwd = $HOME` матчатся **всем**
под-проектам пользователя. План — откатить симметрию к асимметричному
`path_is_within_project(session_cwd, current_cwd)`: session.cwd должна лежать
**внутри** (или равна) current_cwd. Кейс «ccs в поддиректории монорепо,
session в корне» больше не покрывается (документировано в Risks).

## Context (from discovery)

- Файлы:
  - `src/tui/state.rs` — содержит `paths_share_project` (удаляется),
    `session_matches_project` (правка одной ветки), `path_is_within_project`
    (оставляем).
  - `CLAUDE.md:82` — описывает `paths_share_project` по имени, нужно
    переписать абзац.
- Тесты `test_session_matches_project_*` в `src/tui/state.rs::tests` — один из
  них (`current_in_subdir_of_session`) меняет ожидание с true на false и
  переименовывается.
- Артефакты архитектурного обзора:
  - `~/.claude/plans/recent-sessions-proud-kernighan-agent-revert-reviewer.md`
  - `~/.claude/plans/recent-sessions-proud-kernighan-agent-a2e64e5b725b21f46.md`
- Конвенции: inline `#[cfg(test)] mod tests`, тесты `test_<thing>_<behavior>`,
  pre-PR гейты — `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test`.

## Development Approach

- **Testing approach**: TDD. Обновляем тест (red phase: assert flip с
  invariant'ом проверяет, что симметрия точно убрана), затем правим код,
  смотрим зелёный.
- CRITICAL: каждая правка проходит `cargo test`, `cargo fmt --check`,
  `cargo clippy -- -D warnings` перед коммитом.
- Делать минимальный набор изменений. Не трогать union, total_count,
  canonicalize, toggle_project_filter, status-text — они корректны.

## Testing Strategy

- **Unit**: один обновлённый тест `test_session_matches_project_..._does_not_match`
  фиксирует асимметричное направление (regression marker через имя теста и
  комментарий).
- **E2E**: нет.
- **Ручная верификация**: `cargo run` из `~/projects/agents-gradle`, Ctrl+A —
  Codex-сессии с `cwd = ~` должны исчезнуть.

## Progress Tracking

- Отмечать `[x]` сразу.
- ➕ — новые подзадачи.
- ⚠️ — блокеры.

## Solution Overview

Один файл, две правки + переименование/инверсия теста + один абзац в
CLAUDE.md:

1. Удалить хелпер `paths_share_project`.
2. В `session_matches_project` заменить вызов
   `paths_share_project(session_cwd, cwd)` на
   `path_is_within_project(session_cwd, cwd)`, обновить doc-comment с
   объяснением асимметрии и $HOME-регрессии.
3. Переименовать тест `test_session_matches_project_current_in_subdir_of_session`
   → `..._does_not_match`, инвертировать assert, добавить комментарий со
   ссылкой на $HOME-кейс.
4. Обновить `CLAUDE.md:82` — заменить описание `paths_share_project` на
   `path_is_within_project(session.cwd, current_cwd)` асимметричное; убрать
   «ccs в монорепо-поддиректории» как ронимый кейс.

## Technical Details

### Хелпер (state.rs, после правки)

```rust
/// Decide whether `session` belongs to the project the user is currently
/// in. Combines the Claude-style file_path check (session file lives
/// inside one of `project_paths`) with an asymmetric cwd check:
/// `session.cwd` must be inside (or equal to) `current_cwd`. The
/// asymmetry is intentional — symmetric matching would let a Codex
/// session recorded at `$HOME` falsely match every project under
/// `$HOME`, which was observed in the wild.
fn session_matches_project(
    session: &RecentSession,
    project_paths: &[String],
    current_cwd: Option<&str>,
) -> bool {
    if project_paths
        .iter()
        .any(|p| path_is_within_project(&session.file_path, p))
    {
        return true;
    }
    match (session.cwd.as_deref(), current_cwd) {
        (Some(session_cwd), Some(cwd)) => path_is_within_project(session_cwd, cwd),
        _ => false,
    }
}
```

### Тест (state.rs::tests, после правки)

```rust
#[test]
fn test_session_matches_project_current_in_subdir_of_session_does_not_match() {
    // Was true under symmetric matching. Made false intentionally:
    // symmetric matching falsely included Codex sessions recorded at
    // common ancestors like $HOME for every subdir the user cd's into.
    // Abstract paths /repo and /repo/sub stand in for the
    // $HOME / $HOME/projects/foo case.
    let session = make_session_with_cwd("/tmp/codex/rollout.jsonl", Some("/repo"));
    assert!(!session_matches_project(&session, &[], Some("/repo/sub")));
}
```

### CLAUDE.md:82 — before/after

Before:
> OR `paths_share_project(session.cwd, current_cwd)` — symmetric, either
> path may contain the other (covers "ccs in a monorepo subdir" and
> "session recorded from a submodule").

After:
> OR `path_is_within_project(session.cwd, current_cwd)` — asymmetric,
> session.cwd must lie inside (or equal) current_cwd. A previous
> symmetric variant was reverted because Codex sessions recorded at
> `$HOME` falsely matched every project under `$HOME`.

## What Goes Where

- **Implementation Steps** — `[ ]` чекбоксы ниже.
- **Post-Completion** — ручная проверка `cargo run` + Ctrl+A.

## Implementation Steps

### Task 1: Flip cwd-matching to asymmetric

**Files:**
- Modify: `src/tui/state.rs`

- [x] (TDD) переименовать тест
      `test_session_matches_project_current_in_subdir_of_session` →
      `test_session_matches_project_current_in_subdir_of_session_does_not_match`,
      инвертировать assert (`assert!(!session_matches_project(...))`),
      добавить regression-комментарий со ссылкой на $HOME-кейс (см.
      Technical Details). Запустить `cargo test
      state::tests::test_session_matches_project_current_in_subdir_of_session_does_not_match`
      — должен **упасть** (red phase: текущая симметрия делает его true,
      assert!(!true) → fail).
- [x] в `session_matches_project` (`src/tui/state.rs:248-263`) заменить
      вызов `paths_share_project(session_cwd, cwd)` на
      `path_is_within_project(session_cwd, cwd)`. Обновить doc-comment по
      шаблону из Technical Details.
- [x] удалить функцию `paths_share_project` и её doc-комментарий
      (`src/tui/state.rs:236-242`) — после правки выше она orphan.
- [x] `grep -n "paths_share_project" src/` — должно вернуть 0 результатов.
- [x] прогнать `cargo test state::tests` — все 9 `test_session_matches_project_*`
      тестов зелёные.
- [x] прогнать `cargo test` целиком — нет регрессий.
- [x] `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`.

### Task 2: Update CLAUDE.md architecture description

**Files:**
- Modify: `CLAUDE.md`

- [x] заменить фрагмент про `paths_share_project` в `CLAUDE.md:82` на
      описание `path_is_within_project(session.cwd, current_cwd)` (см.
      Technical Details > CLAUDE.md:82 before/after). Убрать упоминание
      «covers ccs in a monorepo subdir», добавить упоминание $HOME-регрессии
      и причины ревёрта.
- [x] `grep -n "paths_share_project\|symmetric" CLAUDE.md` — должно
      вернуть 0 (или только не относящиеся к нашему контексту совпадения).

### Task 3: Verify acceptance criteria

- [x] `cargo test` — все зелёные.
- [x] `cargo fmt --check`.
- [x] `cargo clippy --all-targets --all-features -- -D warnings`.
- [x] (manual) `cargo run` из `~/projects/agents-gradle` → recent sessions →
      Ctrl+A. В списке должны быть Claude+Codex для текущего проекта;
      Codex-сессии с cwd=`~` (как `019e0c65-aed6`, `019df743-aef3` из
      bug-репорта) НЕ видны. (Owner verifies — automated loop cannot
      drive interactive TUI; automated gates above cover the unit-level
      regression via `test_session_matches_project_*_does_not_match`.)

### Task 4: [Final] Archive plan

- [x] двинуть этот план в `docs/plans/completed/2026-05-11-revert-cwd-symmetric-matching.md`.

## Post-Completion

- Push ветки `recent-sessions-codex-ctrl-a` и открытие/обновление PR через
  release-skill либо вручную через `gh pr ...`.
- (Future, отдельный план) Эвристика «session.cwd является проектным
  корнем» через наличие `.git`/`Cargo.toml` — позволит вернуть кейс «ccs в
  поддиректории, session в корне монорепо» без $HOME false-positive.
