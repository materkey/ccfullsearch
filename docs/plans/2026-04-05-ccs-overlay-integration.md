# ccs overlay integration

## Overview

Двусторонняя overlay-интеграция между ccs (сессионный поиск) и claude (Claude Code CLI):

1. **ccs → claude**: из TUI выбрать сессию → открыть claude в overlay → после выхода вернуться в ccs (просмотр нескольких сессий подряд без потери контекста поиска)
2. **claude → ccs**: из Claude Code skill → открыть ccs picker в overlay → выбрать сессию → вернуть результат для resume

Образец: revdiff plugin (`/Users/vkkovalev/projects/revdiff/.claude-plugin/`) — `launch-revdiff.sh` с tmux/kitty/wezterm overlay.

## Context

- ccs использует ratatui + crossterm, resume через Unix `exec()` (заменяет процесс, без возврата)
- Нет suspend/resume TUI паттерна — tree mode работает внутри одного alternate screen
- revdiff уже имеет рабочий overlay-паттерн: tmux `display-popup -E`, kitty `launch --type=overlay` + sentinel, wezterm `split-pane` + sentinel
- Текущий skill (`skill/SKILL.md`) — только CLI mode, без overlay

### Ключевые файлы
| Файл | Роль |
|------|------|
| `src/main.rs` | CLI args, TUI lifecycle, resume dispatch |
| `src/tui/state.rs:95-174` | `App` struct, resume_* fields |
| `src/tui/search_mode.rs:165-202` | `on_enter()` — sets resume fields |
| `src/tui/tree_mode.rs:179-195` | `on_enter_tree()` — sets resume fields |
| `src/resume/launcher.rs:231-241` | `resume_cli()` — exec() |
| `src/resume/launcher.rs:207-228` | `build_resume_command()` — reusable |
| revdiff `launch-revdiff.sh` | Reference overlay launcher |

## Development Approach
- **Testing approach**: TDD — тесты сначала, потом код
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

## Testing Strategy
- **Unit tests**: required for every task
- Key testable units: PickedSession serialization, output formatting, resume_cli_child, CLI arg parsing
- Shell script testing: manual verification in tmux/kitty/wezterm

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Implementation Steps

### Task 1: PickedSession struct and key-value serialization

Добавить структуру выбранной сессии и её формат вывода (key-value).

- [ ] write test: `PickedSession::to_key_value()` outputs correct format:
  ```
  session_id: abc-123
  file_path: /path/to/session.jsonl
  source: CLI
  project: my-project
  ```
- [ ] write test: `PickedSession::to_key_value()` with Desktop source
- [ ] write test: `PickedSession::write_output()` writes to file when path given
- [ ] write test: `PickedSession::write_output()` writes to stdout when no path
- [ ] implement `PickedSession` struct in `src/tui/state.rs` with `to_key_value()` and `write_output()`
- [ ] run tests — must pass before next task

### Task 2: Picker mode in App state and on_enter

Добавить `picker_mode` в App, изменить `on_enter()` и `on_enter_tree()`.

- [ ] write test: `on_enter()` in picker mode sets `picked_session` instead of `resume_*`
- [ ] write test: `on_enter()` in picker mode from recent sessions
- [ ] write test: `on_enter_tree()` in picker mode sets `picked_session`
- [ ] write test: Esc in picker mode leaves `picked_session` as None
- [ ] add `picker_mode: bool` and `picked_session: Option<PickedSession>` to `App` struct in `src/tui/state.rs`
- [ ] modify `on_enter()` in `src/tui/search_mode.rs` — if `picker_mode`, populate `picked_session` instead of `resume_*`
- [ ] modify `on_enter_tree()` in `src/tui/tree_mode.rs` — same picker branch
- [ ] run tests — must pass before next task

### Task 3: `ccs pick` subcommand and TUI lifecycle refactor

Добавить subcommand `Pick`, рефакторинг main loop в `run_tui()`.

- [ ] write test: CLI parsing of `ccs pick` with optional query and `--output`
- [ ] write test: `run_tui` returns `TuiOutcome::Pick` when picker mode + selection
- [ ] write test: `run_tui` returns `TuiOutcome::Quit` when picker mode + Esc (exit code 1)
- [ ] write test: `run_tui` returns `TuiOutcome::Resume` when normal mode
- [ ] add `Pick { query: Option<String>, output: Option<String> }` to `Commands` enum in `src/main.rs`
- [ ] extract TUI lifecycle into `run_tui()` → returns `TuiOutcome` enum
- [ ] in `main()`: match `TuiOutcome::Pick` → call `write_output()`, exit 0/1
- [ ] run tests — must pass before next task

### Task 4: Picker mode visual indicator

Показать `[PICK]` в status bar когда picker mode активен.

- [ ] write test: status bar contains "[PICK]" indicator when `picker_mode` is true
- [ ] write test: status bar does NOT contain "[PICK]" in normal mode
- [ ] modify render logic in `src/tui/render_search.rs` to show `[PICK]` indicator
- [ ] run tests — must pass before next task

### Task 5: `resume_cli_child()` — child process вместо exec

Новая функция для запуска claude как дочернего процесса (с возвратом).

- [ ] write test: `build_resume_command()` returns correct (working_dir, resume_arg) — уже есть, проверить покрытие
- [ ] write test: `resume_cli_child()` calls Command::status() not exec() (mock-based or integration)
- [ ] implement `resume_cli_child()` in `src/resume/launcher.rs` — использует `build_resume_command()` + `Command::status()`
- [ ] run tests — must pass before next task

### Task 6: `--overlay` flag и TUI loop

Добавить `--overlay` флаг: после resume через child process — вернуться в TUI.

- [ ] write test: CLI parsing of `--overlay` flag
- [ ] write test: outer loop in main — `TuiOutcome::Resume` + overlay → calls `resume_cli_child` and continues
- [ ] write test: `TuiOutcome::Resume` without overlay → calls `resume()` (exec) and breaks
- [ ] add `--overlay` to `Cli` struct in `src/main.rs`
- [ ] implement outer loop: `run_tui()` → match Resume → if overlay, call `resume_cli_child`, loop back
- [ ] run tests — must pass before next task

### Task 7: Shell launcher script (`launch-ccs.sh`)

Скрипт по образцу `launch-revdiff.sh` для запуска ccs picker в overlay.

- [ ] create `.claude-plugin/skills/ccs/scripts/launch-ccs.sh`
- [ ] implement terminal detection: tmux → kitty → wezterm → fallback (direct run)
- [ ] tmux: `display-popup -E` with `ccs pick --output=$OUTPUT_FILE`
- [ ] kitty: `kitty @ launch --type=overlay` + sentinel file polling
- [ ] wezterm: `wezterm cli split-pane --bottom` + sentinel file polling
- [ ] fallback: direct `ccs pick --output=$OUTPUT_FILE` (for Bash tool in Claude Code)
- [ ] add `CCS_POPUP_WIDTH` / `CCS_POPUP_HEIGHT` env var support (default 90%)
- [ ] resolve ccs to absolute path (same pattern as revdiff line 10-15)
- [ ] manual test in tmux — must work before next task

### Task 8: Claude Code plugin structure

Создать `.claude-plugin/` с plugin.json и SKILL.md.

- [ ] create `.claude-plugin/plugin.json` (name, version, description, skills path)
- [ ] create `.claude-plugin/skills/ccs/SKILL.md` with two modes:
  - CLI mode (preserved from current `skill/SKILL.md`): `ccs search`, `ccs list`
  - Overlay picker mode (new): launch-ccs.sh → parse key-value output → offer resume
- [ ] update old `skill/SKILL.md` with note about plugin
- [ ] manual test: install plugin, verify skill triggers correctly

### Task 9: Verify acceptance criteria

- [ ] verify `ccs pick` outputs key-value format, exit 0 on selection, exit 1 on cancel
- [ ] verify `ccs pick --output=/tmp/test` writes to file
- [ ] verify `ccs --overlay` opens claude as child, returns to ccs after exit
- [ ] verify `launch-ccs.sh` works in tmux overlay
- [ ] verify skill triggers and processes picker output
- [ ] run full test suite (`cargo test`)
- [ ] run linter (`cargo clippy`)

### Task 10: [Final] Update documentation

- [ ] update README.md: add picker mode and overlay sections
- [ ] update CHANGELOG if project has one

## Technical Details

### Picker output format (key-value)
```
session_id: <uuid>
file_path: <absolute path to .jsonl>
source: CLI|Desktop
project: <project name>
```
Exit code 0 = selection made, exit code 1 = cancelled (empty output).

### TuiOutcome enum
```rust
enum TuiOutcome {
    Quit,
    Resume { session_id: String, file_path: String, source: SessionSource, uuid: Option<String> },
    Pick(PickedSession),
}
```

### Terminal overlay detection (shell)
Priority: `$TMUX` → `$KITTY_LISTEN_ON` → `$WEZTERM_PANE` → fallback (direct run).

### Fallback behavior
- `ccs` без `--overlay` и не в overlay-терминале → exec() как сейчас (backward compatible)
- `launch-ccs.sh` вне tmux/kitty/wezterm → прямой запуск `ccs pick` (работает из Bash tool Claude Code)

## Post-Completion

**Manual verification:**
- Тест в tmux: `ccs --overlay` → Enter → claude opens → exit → back in ccs
- Тест в kitty: `launch-ccs.sh` → overlay popup → pick session → output captured
- Тест в wezterm: то же самое через split-pane
- Тест skill из Claude Code: вызвать skill → overlay → pick → resume предложен
