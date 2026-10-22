# Return to Search After a Quick Exit — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Если открытая из ccs сессия закрыта быстрее 60 с, ccs возвращается в прежнее состояние поиска, а открытая сессия получает метку `[opened]`.

**Architecture:** В обычном режиме терминальные провайдеры (Claude CLI, Codex, Opencode) запускаются дочерним процессом, а не через `exec`. Один `App` живёт на протяжении всех проходов `run_tui_loop`, `run_tui` его заимствует. Перед запуском `App` отменяет фоновый поиск, после быстрого выхода `App::return_from_resume` снимает флаг выхода и помечает сессию. Пока работает дочерний процесс, ccs игнорирует SIGINT/SIGQUIT, повторяет за ребёнком остановку по Ctrl+Z и после его выхода сбрасывает ввод терминала. Выход с ошибкой в первые секунды считается неудачным запуском для всех терминальных провайдеров, как сейчас у Codex.

**Tech Stack:** Rust, ratatui, crossterm, `libc` (сейчас только в `[dev-dependencies]`, Task 1 переносит его в зависимости для unix).

**Spec:** `docs/superpowers/specs/2026-10-22-return-to-search-design.md`

## Global Constraints

- Порог быстрого выхода: `Duration::from_secs(60)`, константа `QUICK_EXIT_WINDOW` рядом с `BOOTSTRAP_WINDOW` (бывший `CODEX_BOOTSTRAP_WINDOW`, Task 2) в `src/resume/launcher.rs`.
- Claude Desktop (`SessionSource::ClaudeDesktop`) в обычном режиме открывается через `exec`, как сейчас, и не возвращается в пикер.
- `ccs pick` и прямой запуск `--tree` + `--resume-uuid` не меняются. Исключение: в `--overlay` прямой запуск после неудачного старта ждёт Enter для любого провайдера (Task 2).
- Если сессия прожила дольше порога, ccs завершается. Код выхода дочернего процесса не пробрасывается: ccs выходит с 0 (уточнение к спеке, сделано по YAGNI).
- SIGTSTP не игнорируем: Ctrl+Z должен возвращать управление шеллу. Если ребёнок остановил себя сам (Claude в raw mode шлёт себе SIGSTOP), ccs останавливает и себя (Task 1).
- Имена тестов в формате `given - when - then` в snake_case, как в соседних тестах.
- Коммиты: только subject в повелительном наклонении, без префикса тикета, без attribution.
- Перед каждым коммитом: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`.

## Review Focus

1. Ctrl+C и Ctrl+Z во время работы Claude: ccs жив, SIG_IGN ставится до `spawn`, у ребёнка SIG_DFL, Ctrl+Z + `fg` не вешает терминал (Task 1).
2. Ошибка старта Claude/Opencode/Codex (код не 0 и не 130, раньше `BOOTSTRAP_WINDOW`) видна до Enter и не ставит метку (Tasks 2, 6, 7).
3. Поиск, отложенный debounce или идущий в момент Enter, после возврата не сбрасывает результаты и курсор (Task 3).
4. Enter на строке, которая была кончиком дерева при загрузке, после возврата не создаёт fork (Task 4).
5. Claude Desktop в обычном режиме по-прежнему открывается через `exec` и не возвращает в пикер (Task 7).

---

### Task 1: Дочерний процесс не роняет ccs и не ломает терминал

**Files:**
- Modify: `Cargo.toml` (`libc = "0.2"` из `[dev-dependencies]` в `[target.'cfg(unix)'.dependencies]`)
- Modify: `src/resume/launcher.rs` (`resume_cli_child`, `run_codex_command`, `resume_opencode_child`, `resume_desktop_child`)
- Test: inline `#[cfg(test)]` в `src/resume/launcher.rs`

**Interfaces:**
- Produces: `fn wait_child(command: &mut Command) -> std::io::Result<std::process::ExitStatus>` (private). Все четыре child-лаунчера вызывают её вместо `.status()`.

- [ ] **Step 1: Написать падающие тесты (`#[cfg(unix)]`)**

```rust
/// wait_child меняет обработчики сигналов на весь процесс, а тесты идут параллельно.
static WAIT_CHILD_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn child_interrupts_parent_when_waiting_then_parent_survives() {
    let _guard = WAIT_CHILD_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let status = wait_child(Command::new("sh").args(["-c", "kill -INT $PPID; exit 0"])).unwrap();
    assert!(status.success());
}

#[test]
fn child_interrupts_itself_when_waiting_then_child_dies_by_sigint() {
    use std::os::unix::process::ExitStatusExt;
    let _guard = WAIT_CHILD_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let status = wait_child(Command::new("sh").args(["-c", "kill -INT $$; sleep 5"])).unwrap();
    assert_eq!(status.signal(), Some(libc::SIGINT));
}
```

Существующий `codex_failure_when_child_exits_at_once_then_returns_error` тоже идёт через `wait_child`: взять в нём тот же `WAIT_CHILD_LOCK`.

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --lib resume::launcher::tests::child_interrupts`
Expected: ошибка компиляции, `wait_child` не определена.

- [ ] **Step 3: Реализовать `wait_child`, подключить её в четыре лаунчера, перенести `libc`**

На unix, по порядку:
1. Сохранить прежние обработчики и поставить `SIG_IGN` для SIGINT и SIGQUIT **до** `spawn`: иначе ребёнок может прислать сигнал раньше, чем ccs начнёт его игнорировать.
2. В `CommandExt::pre_exec` вернуть ребёнку `SIG_DFL` для SIGINT и SIGQUIT (`libc::signal` безопасен между `fork` и `exec`). Без этого ребёнок унаследует SIG_IGN, это ловит второй тест.
3. `spawn`, затем `libc::waitpid(child.id(), &mut status, WUNTRACED)` в цикле, `EINTR` повторять. Если ребёнок остановлен (`WIFSTOPPED`), ccs останавливает себя `libc::raise(SIGSTOP)` и ждёт дальше: `fg` шлёт SIGCONT всей группе процессов. Итог собрать через `ExitStatus::from_raw`, `child.wait()` после этого не вызывать.
4. После выхода ребёнка `libc::tcflush(STDIN_FILENO, TCIFLUSH)`, ошибку игнорировать (в тестах stdin не tty). Иначе клавиши, нажатые пока Claude закрывался, прочитает пикер: Ctrl+C сотрёт запрос, Enter снова откроет сессию.
5. Восстановить прежние обработчики на всех путях, включая ошибку `spawn`.

На `cfg(not(unix))` это просто `command.status()`. Комментарий `// Any exit code is acceptable` в `resume_cli_child` пока сохранить, его переписывает Task 2.

- [ ] **Step 4: Запустить тесты и убедиться, что они проходят**

Run: `cargo test --lib resume::launcher`
Expected: PASS.

- [ ] **Step 5: Коммит**

```bash
git commit -m "Keep ccs alive and the terminal clean while a resumed child runs" -- Cargo.toml Cargo.lock src/resume/launcher.rs
```

`Cargo.lock` указывать, только если он изменился.

### Task 2: Быстрый выход с ошибкой — неудачный запуск для любого провайдера

**Files:**
- Modify: `src/resume/launcher.rs` (`CODEX_BOOTSTRAP_WINDOW` → `BOOTSTRAP_WINDOW`, `run_codex_command` → `run_child`, `codex_exit_result` → `child_exit_result`, `resume_cli_child`, `resume_opencode_child`)
- Modify: `src/main.rs` (`picker_follows_resume`, `run_direct_resume`, новая `failed_launch_waits`)
- Test: inline-тесты в `src/resume/launcher.rs` и `src/main.rs`

**Interfaces:**
- Consumes: `wait_child` (Task 1).
- Produces:
  - `const BOOTSTRAP_WINDOW: Duration = Duration::from_secs(10);`
  - `fn run_child(command: &mut Command, name: &str) -> Result<(), String>`: засекает время, вызывает `wait_child`, ошибка `spawn` → `"Failed to spawn {name}: {e}"`, итог → `child_exit_result`.
  - `fn child_exit_result(name: &str, code: Option<i32>, ran_for: Duration) -> Result<(), String>`: прежнее правило Codex (код не 0, не 130, раньше `BOOTSTRAP_WINDOW` → `Err("{name} exited with status {code}")`).
  - `resume_cli_child`, `resume_codex_child`, `resume_opencode_child` вызывают `run_child(.., "Claude" | "Codex" | "Opencode")`. `resume_desktop_child` по-прежнему игнорирует код `open -W`.
  - `fn failed_launch_waits(file_path: &str, overlay: bool) -> bool` в `main.rs`: `overlay || is_codex_session(file_path)`, то есть запуск шёл дочерним процессом и пикер вернётся. Ветка `Err` в `picker_follows_resume`: `let stay = failed_launch_waits(file_path, overlay) && wait_for_enter("Press Enter to return to ccs: ")`. Без ожидания пикер сразу закрыл бы сообщение alternate screen'ом.
  - `run_direct_resume`: при ошибке в `--overlay` ждать Enter для любого провайдера, не только Codex (условие `overlay && is_codex_session(..)` → `overlay`).

- [ ] **Step 1: Написать падающие тесты**

```rust
// src/resume/launcher.rs, под WAIT_CHILD_LOCK
#[test]
fn claude_like_child_when_exiting_with_error_at_once_then_returns_error() {
    let mut command = Command::new("sh");
    command.args(["-c", "exit 1"]);
    assert_eq!(run_child(&mut command, "Claude").unwrap_err(), "Claude exited with status 1");
}

#[test]
fn claude_like_child_when_exiting_cleanly_at_once_then_returns_ok() {
    let mut command = Command::new("sh");
    command.args(["-c", "exit 0"]);
    assert!(run_child(&mut command, "Claude").is_ok());
}

// src/main.rs, заменяет failed_claude_resume_in_overlay_when_deciding_next_step_then_picker_returns:
// с новым правилом picker_follows_resume в этом случае читает stdin
#[test]
fn failed_launch_when_deciding_whether_to_wait_then_only_child_launches_wait() {
    assert!(failed_launch_waits("/home/u/.claude/projects/-p/s.jsonl", true));
    assert!(failed_launch_waits("/home/u/.codex/sessions/2026/01/01/rollout-x.jsonl", false));
    assert!(!failed_launch_waits("/home/u/.claude/projects/-p/s.jsonl", false));
}
```

Существующие тесты `codex_*` перевести на `run_child` / `child_exit_result` и `BOOTSTRAP_WINDOW`, ожидаемое сообщение `"Codex exited with status 1"` остаётся.

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --lib resume::launcher && cargo test --bin ccs failed_launch`
Expected: ошибка компиляции, `run_child` / `failed_launch_waits` не определены.

- [ ] **Step 3: Реализовать**

Комментарий в `resume_cli_child` переписать: Claude выходит с 0 и по `/exit`, и по Ctrl+C, а ошибка старта (`No conversation found`) даёт 1, поэтому ненулевой код в первые `BOOTSTRAP_WINDOW` — неудачный запуск. Doc-комментарий `wait_for_enter` обобщить с Codex на любой неудачный запуск.

- [ ] **Step 4: Прогнать все тесты**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Коммит**

```bash
git commit -m "Treat an early error exit of any resumed child as a failed launch" -- src/resume/launcher.rs src/main.rs
```

### Task 3: `App` переживает запуск сессии

**Files:**
- Modify: `src/tui/state.rs` (struct `App`, `App::new`, `into_outcome` → `take_outcome`, новая `suspend_for_resume`)
- Modify: `src/main.rs:186` (единственный вызов `into_outcome`, временно `app.take_outcome()`; цикл переделывает Task 6)
- Test: inline-тесты в `src/tui/state.rs` (существующие `test_into_outcome_*` переименовать под `take_outcome`)

**Interfaces:**
- Produces:
  - `pub opened_sessions: HashSet<String>` в `App` (ключ `session_id`), инициализируется пустым.
  - `pub fn take_outcome(&mut self) -> TuiOutcome` заменяет `into_outcome(self)`. Поле `input` не трогает. `TuiOutcome::Resume::query` пока остаётся и заполняется `self.input.text().to_string()`: его вместе с `saved_query`/`restore_query` убирает Task 6, где `App` начинает жить между проходами.
  - На `Resume` `take_outcome` вызывает `fn suspend_for_resume(&mut self)` (private). Раньше эту работу делал `Drop` у выброшенного `App`. Отменяет и забирает `search.current`, сбрасывает `last_keystroke = None`, `typing = false`, ставит `last_query` равным тексту `input`. Иначе отложенный debounce или результат `rg`, пришедший после возврата, обнулит `groups`, `group_cursor` и `expanded`. Поток подсчёта сообщений не отменяется: он обновляет счётчики на месте и курсор не трогает, а при долгой сессии `App` всё равно уничтожается вместе с ccs.
  - `pub fn return_from_resume(&mut self, opened: Option<&str>)`: `should_quit = false`, `outcome = None`, `needs_full_redraw = true`. Если `opened` равен `Some(session_id)`, делает `opened_sessions.insert(session_id)`; `None` передают после неудачного запуска, чтобы на сессию, которая не открылась, не ставилась метка. Больше ничего не меняет.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn search_results_with_filters_when_returning_from_resume_then_state_is_kept() {
    // app с 3 группами, input "foo", group_cursor = 2, regex_mode = true,
    // ai.active = true, ai.ranked_count = Some(1), outcome = Resume(sid "s2")
    let outcome = app.take_outcome();
    app.return_from_resume(Some("s2"));
    assert!(matches!(outcome, TuiOutcome::Resume { ref query, .. } if query == "foo"));
    assert_eq!(app.input.text(), "foo");
    assert_eq!(app.search.groups.len(), 3);
    assert_eq!(app.search.group_cursor, 2);
    assert!(app.regex_mode);
    assert!(app.ai.active);
    assert_eq!(app.ai.ranked_count, Some(1));
    assert!(!app.should_quit);
    assert!(app.outcome.is_none());
    assert!(app.needs_full_redraw);
    assert!(app.opened_sessions.contains("s2"));
}

#[test]
fn pending_search_at_enter_when_returning_and_ticking_then_results_and_cursor_are_kept() {
    // app с 3 группами, input "foo", group_cursor = 2, expanded = true,
    // search.current = Some(handle) (флаг cancel сохранить в тесте),
    // last_keystroke = Some(Instant::now() - 2 * DEBOUNCE_MS), outcome = Resume(sid "s2")
    app.take_outcome();
    app.return_from_resume(Some("s2"));
    app.tick();
    assert!(cancel.load(Ordering::Relaxed));
    assert!(app.search.current.is_none());
    assert_eq!(app.search.groups.len(), 3);
    assert_eq!(app.search.group_cursor, 2);
    assert!(app.search.expanded);
}

#[test]
fn tree_mode_selection_when_returning_from_resume_then_tree_and_cursor_are_kept() {
    // tree_mode = true, tree.tree_cursor = 4
    app.take_outcome();
    app.return_from_resume(Some("s1"));
    assert!(app.tree_mode);
    assert_eq!(app.tree.tree_cursor, 4);
}

#[test]
fn recent_sessions_cursor_when_returning_from_resume_then_cursor_is_kept() {
    // пустой input, recent.filtered из 3, recent.cursor = 1
    app.take_outcome();
    app.return_from_resume(Some("r1"));
    assert_eq!(app.recent.cursor, 1);
}

#[test]
fn failed_launch_when_returning_from_resume_then_no_session_is_marked() {
    app.take_outcome();
    app.return_from_resume(None);
    assert!(app.opened_sessions.is_empty());
    assert!(!app.should_quit);
}
```

Для фикстур использовать уже существующие хелперы тестов `state.rs`, например тот, что рядом с `test_into_outcome_resume_when_outcome_set`.

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --lib returning`
Expected: ошибка компиляции, `take_outcome` / `return_from_resume` не определены.

- [ ] **Step 3: Реализовать поле и методы, обновить doc-комментарии, где упоминается `into_outcome`**

- [ ] **Step 4: Прогнать все тесты**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Коммит**

```bash
git commit -m "Let the TUI app outlive a resumed session" -- src/tui/state.rs src/main.rs
```

### Task 4: Enter на кончике дерева не создаёт fork, даже если дерево устарело

После быстрого `/exit` Claude дописывает в файл записи (`/exit`, `local-command-stdout`), и кончик `TipStrategy::LastAppended` сдвигается. Дерево, сохранённое в `App`, этого не видит: Enter на прежнем кончике шлёт `uuid: Some(..)`, и `fork::should_fork_for_resume` молча создаёт fork. Восстановление курсора по uuid после перезагрузки не помогает: прежний кончик кончиком уже не является.

**Files:**
- Modify: `src/tree/mod.rs` (`SessionTree`, `from_file`, `build_latest_chain`, загрузчик Opencode, `new_for_test`)
- Modify: `src/tui/tree_mode.rs` (`on_enter_tree`)
- Test: inline-тесты в `src/tui/tree_mode.rs`

**Interfaces:**
- Produces:
  - `pub tip_uuid: Option<String>` в `SessionTree`: кончик на момент загрузки по `TipStrategy::LastAppended`, как в `fork::should_fork_for_resume`. `build_latest_chain` возвращает его вместе с цепочкой из того же `SessionDag`. Для Opencode и `new_for_test` — `None`.
  - `on_enter_tree` (ветка `Resume`): `uuid: None`, если `row.uuid` равен `tree.tip_uuid`, иначе `Some(row.uuid)`. `None` значит «продолжить последнюю ветку», то есть ту же сессию вместе с дописанным хвостом. Ветка `Pick` (`message_uuid`) не меняется.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn tree_loaded_before_session_grew_when_entering_old_tip_then_resume_has_no_uuid() {
    // tmp jsonl: u1 (user) -> a1 (assistant); SessionTree::from_file;
    // дописать в файл u2 (user "/exit", parentUuid a1);
    // app.tree.session_tree = Some(tree), курсор на строке a1
    assert!(crate::resume::fork::should_fork_for_resume(&path, "a1"));
    app.on_enter_tree();
    assert!(matches!(app.outcome, Some(AppOutcome::Resume(ResumeTarget { uuid: None, .. }))));
}

#[test]
fn tree_row_off_tip_when_entering_then_resume_keeps_uuid() {
    // тот же файл без дописывания, курсор на строке u1
    app.on_enter_tree();
    assert!(matches!(app.outcome, Some(AppOutcome::Resume(ResumeTarget { uuid: Some(ref u), .. })) if u == "u1"));
}
```

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --lib tui::tree_mode::tests::tree_`
Expected: первый тест FAIL (`uuid` равен `Some("a1")`).

- [ ] **Step 3: Реализовать**

- [ ] **Step 4: Прогнать все тесты**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Коммит**

```bash
git commit -m "Resume without a fork when Enter lands on the loaded tree tip" -- src/tree/mod.rs src/tui/tree_mode.rs
```

### Task 5: Метка `[opened]` на открытой сессии

**Files:**
- Modify: `src/tui/render_search.rs` (`SessionHeaderParts`, `render_session_header_line`, `format_session_header_line`, все четыре места, где строится `SessionHeaderParts`)
- Test: inline-тесты в `src/tui/render_search.rs`

**Interfaces:**
- Consumes: `App::opened_sessions` (Task 3).
- Produces: поле `opened: bool` в `SessionHeaderParts`. Константа `OPENED_TAG: &str = " [opened]"`.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn opened_search_group_when_rendering_then_tag_is_shown_only_on_it() {
    let mut app = make_test_app_with_n_groups(3);
    app.opened_sessions.insert(app.search.groups[1].session_id.clone());
    // render 120x24 через TestBackend, как в соседних тестах
    assert_eq!(count_occurrences(buffer, "[opened]"), 1);
}

#[test]
fn opened_recent_session_when_rendering_then_tag_is_shown() {
    let mut app = make_test_app_with_n_recent_sessions(3);
    app.opened_sessions.insert(app.recent.filtered[0].session_id.clone());
    assert!(buffer_contains(buffer, 120, 24, "[opened]"));
}
```

`count_occurrences` написать рядом с `buffer_contains`, если такого хелпера ещё нет.

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --lib tui::render_search::tests::opened`
Expected: FAIL или ошибка компиляции.

- [ ] **Step 3: Реализовать**

Метка ставится сразу после `[A]` в том же порядке и в `render_session_header_line`, и в `format_session_header_line`. Стиль: `base`, если строка выбрана, иначе `DIM_FG`, фон через `style_with_bg`, как у `[A]`. Значение `opened` берётся как `app.opened_sessions.contains(session_id)`.

- [ ] **Step 4: Прогнать все тесты**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Коммит**

```bash
git commit -m "Mark sessions opened from the current search" -- src/tui/render_search.rs
```

### Task 6: Один `App` на все проходы пикера

Меняются только пути, которые уже возвращаются в пикер: `--overlay` и повтор после неудачного запуска. Теперь они восстанавливают полное состояние, а не только запрос. Обычный режим по-прежнему делает `exec`.

**Files:**
- Modify: `src/main.rs` (`run_tui`, `run_tui_inner`, `run_pick`, `run_tui_loop`, `handle_outcome`, `resume_outcome`, новые `enum AfterResume` и `apply_after_resume`; удалить `run_tui_once`, `saved_query`, параметр `restore_query` и тесты `test_saved_query_*`)
- Modify: `src/tui/state.rs` (`TuiOutcome::Resume` теряет `query`, `take_outcome` перестаёт его заполнять)
- Test: inline-тесты в `src/main.rs`

**Interfaces:**
- Consumes: `App::take_outcome`, `App::return_from_resume` (Task 3).
- Produces:
  - `fn run_tui(app: &mut ccs::tui::App) -> io::Result<ccs::tui::TuiOutcome>`. `init_app` вызывается снаружи: один раз в `run_tui_loop` (`init_app(search_paths, tree_target, false, None)`) и в `run_pick`.
  - `enum AfterResume { Exit, Picker { launched: bool } }` (private, `main.rs`). `resume_outcome(session_id, file_path, source, uuid, overlay) -> AfterResume`: `launched` = запуск вернул `Ok`, пикер = `picker_follows_resume(..)`.
  - `fn apply_after_resume(app: &mut ccs::tui::App, after: AfterResume, session_id: &str) -> bool`: на `Picker { launched }` вызывает `app.return_from_resume(launched.then_some(session_id))` и возвращает `true`; на `Exit` возвращает `false`.
  - `handle_outcome(outcome, overlay, app: &mut App) -> bool`: на `Resume` → `apply_after_resume(app, resume_outcome(..), &session_id)`, иначе `false`.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn picker_after_successful_launch_when_applying_then_app_returns_and_marks_session() {
    let mut app = ccs::tui::App::new(vec![]);
    app.should_quit = true;
    assert!(apply_after_resume(&mut app, AfterResume::Picker { launched: true }, "s1"));
    assert!(!app.should_quit);
    assert!(app.opened_sessions.contains("s1"));
}

#[test]
fn picker_after_failed_launch_when_applying_then_session_is_not_marked() {
    let mut app = ccs::tui::App::new(vec![]);
    assert!(apply_after_resume(&mut app, AfterResume::Picker { launched: false }, "s1"));
    assert!(app.opened_sessions.is_empty());
}

#[test]
fn exit_when_applying_then_loop_stops() {
    let mut app = ccs::tui::App::new(vec![]);
    assert!(!apply_after_resume(&mut app, AfterResume::Exit, "s1"));
}
```

`test_handle_outcome_quit_stops_loop` и тесты, которые строят `TuiOutcome::Resume { .. query }`, поправить под новые сигнатуры.

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --bin ccs`
Expected: ошибка компиляции из-за сигнатур.

- [ ] **Step 3: Реализовать**

`run_tui_loop` создаёт `App` и крутит `run_tui(&mut app)` + `handle_outcome`, пока тот возвращает `true`. Doc-комментарии `run_tui_loop` и `handle_outcome` поправить: в пикер возвращается тот же `App`.

- [ ] **Step 4: Прогнать все тесты**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Коммит**

```bash
git commit -m "Keep one picker app across overlay and retry passes" -- src/main.rs src/tui/state.rs
```

### Task 7: Возврат в пикер после быстрого выхода

**Files:**
- Modify: `src/resume/launcher.rs` (константа)
- Modify: `src/resume/mod.rs` (новая `runs_in_terminal`, doc-комментарий `ResumeMode::Exec`)
- Modify: `src/main.rs` (`resume_outcome`, `launch_resume`, `run_direct_resume`, `picker_follows_resume`, новые `picker_launch_runs_as_child` и `is_quick_exit`; удалить `failed_launch_waits` с её тестом и ставшую ненужной `is_codex_session`)
- Modify: `CLAUDE.md` (пункт 4 «TUI lifecycle» в «Key data flow»)
- Test: inline-тесты в `src/main.rs` и `src/resume/mod.rs`

**Interfaces:**
- Consumes: `AfterResume`, `apply_after_resume` (Task 6), `BOOTSTRAP_WINDOW` (Task 2).
- Produces:
  - `pub const QUICK_EXIT_WINDOW: Duration = Duration::from_secs(60);` в `src/resume/launcher.rs`.
  - `pub fn runs_in_terminal(source: SessionSource) -> bool` в `src/resume/mod.rs`: `source != SessionSource::ClaudeDesktop`. Модуль `resume` уже выбирает лаунчер по провайдеру и источнику, поэтому знание «какая сессия занимает терминал и может вернуть управление» живёт в нём, а не в `main.rs`. Doc `ResumeMode::Exec` дополнить: пикер в обычном режиме запускает терминальные сессии через `Child`, `Exec` остаётся для Desktop и прямого `--tree` + `--resume-uuid`.
  - `fn is_quick_exit(source: SessionSource, ran_for: Duration) -> bool`: `ccs::resume::runs_in_terminal(source) && ran_for < QUICK_EXIT_WINDOW`.
  - `fn picker_launch_runs_as_child(source: SessionSource, overlay: bool) -> bool`: `overlay || ccs::resume::runs_in_terminal(source)`.
  - `fn picker_follows_resume(result: Result<(), String>, child: bool, overlay: bool, quick_exit: bool) -> bool`: при `Ok` возвращает `overlay || quick_exit`. При `Err` печатает ошибку, `stay = child && wait_for_enter(..)`, без `stay` — `exit(1)`. Параметр `child` заменяет `file_path` и `failed_launch_waits`: Codex — терминальный провайдер, поэтому всегда `child`.
  - `launch_resume(session_id, file_path, source, uuid, child: bool)`: параметр `overlay` переименован в `child`, решения внутри нет: `child` → `resume_child`, иначе `resume`.
  - `resume_outcome` вычисляет `child = picker_launch_runs_as_child(source, overlay)` и передаёт его в `launch_resume` и `picker_follows_resume`. `run_direct_resume` передаёт `overlay` как есть, поэтому прямой запуск `--tree` + `--resume-uuid` без `--overlay` по-прежнему делает `exec`.

- [ ] **Step 1: Написать падающие тесты (заменить `successful_resume_when_deciding_next_step_then_picker_returns_only_in_overlay`)**

```rust
#[test]
fn successful_resume_when_deciding_next_step_then_picker_returns_in_overlay_or_after_quick_exit() {
    assert!(picker_follows_resume(Ok(()), true, true, false));
    assert!(picker_follows_resume(Ok(()), true, false, true));
    assert!(!picker_follows_resume(Ok(()), true, false, false));
}

#[test]
fn cli_session_when_measuring_exit_then_quick_only_below_window() {
    let w = ccs::resume::launcher::QUICK_EXIT_WINDOW;
    assert!(is_quick_exit(SessionSource::CLI, w - Duration::from_millis(1)));
    assert!(!is_quick_exit(SessionSource::CLI, w));
}

#[test]
fn desktop_session_when_measuring_exit_then_never_quick() {
    assert!(!is_quick_exit(SessionSource::ClaudeDesktop, Duration::ZERO));
}

#[test]
fn picker_resume_when_choosing_launch_mode_then_child_unless_desktop_without_overlay() {
    assert!(picker_launch_runs_as_child(SessionSource::CLI, false));
    assert!(!picker_launch_runs_as_child(SessionSource::ClaudeDesktop, false));
    assert!(picker_launch_runs_as_child(SessionSource::ClaudeDesktop, true));
}

// src/resume/mod.rs
#[test]
fn session_source_when_asking_terminal_then_only_desktop_is_not() {
    assert!(runs_in_terminal(SessionSource::CLI));
    assert!(!runs_in_terminal(SessionSource::ClaudeDesktop));
}
```

Ветку `Err` у `picker_follows_resume` юнит-тестами не покрывать: она читает stdin.

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test --bin ccs && cargo test --lib resume::tests::session_source`
Expected: ошибка компиляции из-за сигнатур.

- [ ] **Step 3: Реализовать**

`resume_outcome` засекает `Instant::now()` вокруг `launch_resume(..., child)` и передаёт `is_quick_exit(source, elapsed)`. Вызов в `run_direct_resume` остаётся прежним: в позицию `child` уходит его `overlay`. Doc-комментарий `launch_resume` поправить: режим выбирает вызывающий. В CLAUDE.md пункт 4 переписать: обычный режим теперь запускает терминальных провайдеров дочерним процессом, быстрый выход (< `QUICK_EXIT_WINDOW`) возвращает в тот же `App`, а выход с ошибкой раньше `BOOTSTRAP_WINDOW` у любого провайдера показывается с ожиданием Enter.

- [ ] **Step 4: Полная проверка**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: всё зелёное.

- [ ] **Step 5: Ручная проверка в реальном терминале**

1. `cargo run`, набрать запрос, включить Ctrl+R, открыть вторую группу Enter'ом, сразу `/exit`. Ожидается: ccs с тем же запросом, regex включён, курсор на второй группе, на ней метка `[opened]`.
2. Набрать запрос и сразу, до появления новых результатов, нажать Enter на группе, затем `/exit`. Ожидается: результаты и курсор те же, что были в момент Enter.
3. Открыть сессию, нажать Ctrl+C внутри Claude. Ожидается: ccs жив и вернулся.
4. Открыть сессию, нажать Ctrl+Z внутри Claude, затем `fg`. Ожидается: шелл получил управление после Ctrl+Z, после `fg` снова Claude, после `/exit` ccs.
5. Сразу после `/exit` несколько раз нажать Enter. Ожидается: ccs не открывает сессию заново.
6. В tree mode открыть последнюю строку, `/exit`, снова Enter на той же строке. Ожидается: в каталоге проекта не появился новый fork-файл `*.jsonl`.
7. Подложить в `PATH` `claude`, который делает `exit 1`, и открыть сессию. Ожидается: текст ошибки и `Press Enter to return to ccs:`, после Enter пикер без метки `[opened]`.
8. Открыть сессию, подождать больше 60 с, выйти. Ожидается: шелл.

- [ ] **Step 6: Коммит**

```bash
git commit -m "Return to the search after quickly leaving a resumed session" -- src/resume/launcher.rs src/resume/mod.rs src/main.rs CLAUDE.md
```
