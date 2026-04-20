# AI re-ranking: Enter должен проваливать в сессию после получения результата

## Overview

В TUI-режиме `Ctrl+G` включает AI re-ranking: пользователь вводит запрос → Enter → `claude -p` переранжирует видимые сессии. Ожидание: после того как результат применён, повторный Enter открывает выделенную сессию (resume). Фактическое поведение: Enter всегда запускает новый AI-запрос, провалиться в сессию невозможно — надо выходить из AI-режима через `Esc/Ctrl+G`, теряя переранжирование.

**Цель**: после прихода результата Enter в AI-режиме работает как в обычном режиме — проваливает в выбранную сессию. Если пользователь меняет запрос — Enter снова становится «переранжировать».

**Дополнительная защита**: дроп `result_rx` при правке запроса, чтобы in-flight ответ от предыдущего submit'а не восстановил флаг «результат применён» через `handle_ai_result`.

## Context (from discovery)

Files/components involved:
- `src/tui/state.rs` — `App::handle_action` (AI-перехват клавиш, строки 855-913), `submit_ai_query` (:1211), `handle_ai_result` (:1258), `exit_ai_mode` (:1192), `AiState` struct (:547).
- `src/tui/search_mode.rs` — `on_enter` (:173-176+) с guard'ом `if self.ai.active { return; }` — это и есть блокер.
- `src/tui/render_search.rs` — AI-hints (строки 331-345), hard-coded `[Enter] Rank`.
- `src/ai.rs` — `spawn_ai_rank`, `AiRankResult`, `SessionInfo`.

Related patterns found:
- Three-agent architectural review (GAN-pattern) подтвердил, что прямой вызов `self.on_enter()` из AI-ветки — no-op из-за guard'а. Нужен `on_enter_inner()` helper без guard'а.
- Race между правкой запроса и in-flight AI-ответом: `result_rx` не имеет seq-счётчика (в отличие от `search_seq` в search-pipeline).

Dependencies identified:
- Никаких новых crate'ов не требуется.
- Все вспомогательные функции уже существуют: `App::on_enter` тело (extract), `submit_ai_query` (reuse as-is), `AiState::ranked_count` (flag), `AiState::result_rx` (drop).

## Development Approach

- **Testing approach**: **TDD** — первым делом пишем регрессионный тест, который падает на текущем коде (sentinel для блокера `on_enter` guard'а), потом поэтапно делаем его зелёным.
- Complete each task fully before moving to the next.
- Make small, focused changes — каждая задача ≤1 файл изменений кроме Task 3 (dispatch + search_mode).
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task.
  - Тесты — обязательные deliverable, не опциональные.
  - Покрытие success + error/edge scenarios.
- **CRITICAL: all tests must pass before starting next task** — no exceptions.
- **CRITICAL: update this plan file when scope changes during implementation**.
- Run `cargo test` after each change.
- Maintain backward compatibility: публичный контракт `on_enter` не меняется (по-прежнему no-op при `ai.active`).

## Testing Strategy

- **Unit tests**: встроенные `#[cfg(test)] mod tests` внутри `src/tui/state.rs` и `src/tui/search_mode.rs`. Каждая задача добавляет/обновляет тесты.
- **Integration tests**: в `tests/` не требуется — фикс внутренне-UI, интеграционных хуков нет.
- **E2E tests**: проект не имеет автоматизированного UI-E2E (ratatui TUI). Ручная проверка — в Post-Completion.
- Тесты выполняют assert на публичные поля `App` (`outcome`, `ai.ranked_count`, `ai.result_rx.is_none()`, `ai.query`) — никакого мокинга `claude -p` не нужно, AI-canal в тестах не запускается.

## Progress Tracking

- Mark completed items with `[x]` immediately when done.
- Add newly discovered tasks with ➕ prefix.
- Document issues/blockers with ⚠️ prefix.
- Update plan if implementation deviates from original scope.
- Keep plan in sync with actual work done.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code changes + tests внутри репозитория.
- **Post-Completion** (no checkboxes): ручной прогон `cargo run`, проверка rebuild на Mac/Z270XX (`cargo install --path .`), возможная CI-release через `cargo-dist` tag.

## Implementation Steps

### Task 1: Failing sentinel-test — Enter в AI-режиме после ranked_count=Some

- [x] в `src/tui/state.rs` в `#[cfg(test)] mod tests` добавить тест `ai_mode_enter_with_ranked_count_triggers_resume` (fails until Task 3)
- [x] в тесте: построить `App::new_for_tests()` (или эквивалентную тест-фабрику — если её нет, использовать существующий helper из тестового модуля), включить AI (`enter_ai_mode()`), положить одну `RecentSession` в `recent.filtered`, вручную выставить `self.ai.ranked_count = Some(1)`
- [x] вызвать `app.handle_action(KeyAction::Enter)` и assert'нуть `app.outcome == Some(AppOutcome::Resume(_))`
- [x] убедиться, что тест **падает** на текущем коде (sentinel подтверждает блокер: `on_enter` no-op'ится из-за guard'а `if self.ai.active { return; }`) — зафиксировать падение в комментарии коммита
- [x] `cargo test ai_mode_enter_with_ranked_count_triggers_resume` — ожидаемое: fail. Зелёным станет после Task 3. (подтверждено: `cargo test ... -- --ignored` → panic «Expected Resume …, got None»; `#[ignore]` удерживает CI зелёным до Task 3)
- [x] разметить чекбокс этого теста как `[x] ... (fails until Task 3)` после добавления (skill partial-implementation exception)

### Task 2: Extract `on_enter_inner` helper в search_mode.rs

- [x] в `src/tui/search_mode.rs` переименовать текущее тело `on_enter` (всё что ниже `if self.ai.active { return; }`) в новый метод `pub(crate) fn on_enter_inner(&mut self)`
- [x] публичный `on_enter` оставить тонкой обёрткой: `if self.ai.active { return; } self.on_enter_inner();`
- [x] убедиться, что никакие другие callsite'ы `on_enter` не изменились (grep по репо — должен быть только через `self.on_enter()` и через `KeyAction::Enter => self.on_enter()` в `handle_action`)
- [x] добавить unit-тест `on_enter_guard_respected_in_ai_mode`: `ai.active = true`, `ai.ranked_count = None`, `app.on_enter()` напрямую — `app.outcome == None` (регрессия guard'а). Этот тест защищает обёртку от будущих правок.
- [x] `cargo test on_enter_guard_respected_in_ai_mode` — ожидается зелёный
- [x] sentinel-тест из Task 1 всё ещё красный (маршрутизация не меняется)
- [x] `cargo build && cargo test --all` — ошибок компиляции нет, регрессий в других тестах нет

### Task 3: Маршрутизация Enter в AI-ветке handle_action → on_enter_inner

- [x] в `src/tui/state.rs` в `handle_action` AI-блоке (строки 907-910) заменить ветку `KeyAction::Enter`:
  - если `self.ai.ranked_count.is_some()` → `self.on_enter_inner(); return;`
  - иначе → существующее `self.submit_ai_query(); return;`
- [x] `_ => {}` fallthrough не трогать (Up/Down/Esc/Ctrl+G продолжают работать через основной match)
- [x] sentinel-тест из Task 1 теперь зелёный — обновить его чекбокс с `(fails until Task 3)` на `[x]`
- [x] добавить тест `ai_mode_enter_without_ranked_count_triggers_submit_with_empty_query_is_noop`: `ai.active = true`, `ai.ranked_count = None`, `ai.query` пустой → `handle_action(Enter)` → `outcome == None` и `ai.thinking == false` (submit_ai_query раньше делал guard на пустой query)
- [x] `cargo test ai_mode` — все тесты из AI-модуля зелёные
- [x] `cargo build && cargo test --all` — регрессий нет

### Task 4: Сброс ranked_count и result_rx в 6 query-mutation ветвях

- [x] в `src/tui/state.rs` в `handle_action` AI-блоке для каждой из 6 ветвей, меняющих `self.ai.query` (`InputChar`, `Backspace`, `Delete`, `ClearInput`, `DeleteWordLeft`, `DeleteWordRight`), перед `return` добавить две строки:
  - `self.ai.ranked_count = None;`
  - `self.ai.result_rx = None;`
- [x] ветви перемещения курсора (`MoveHome/End/Left/Right/MoveWord*`) не трогать
- [x] `original_recent_order` / `original_groups_order` не сбрасывать (визуально остаётся последнее ранжирование до нового результата)
- [x] добавить параметризованный тест `ai_query_mutation_clears_rank_and_receiver`:
  - для каждой из 6 KeyAction-ветвей: выставить `ai.ranked_count = Some(3)`, создать фиктивный mpsc-канал и положить `rx` в `ai.result_rx`, вызвать соответствующий `handle_action(...)` → assert `ai.ranked_count.is_none()` && `ai.result_rx.is_none()` && `ai.query.text()` отражает мутацию (для InputChar/Backspace/Delete/ClearInput/DeleteWordLeft/DeleteWordRight)
- [x] добавить negative-тест `ai_cursor_movement_does_not_clear_rank`: `ai.ranked_count = Some(3)`, `handle_action(KeyAction::Left)` → `ai.ranked_count == Some(3)` (курсор-only мутация не сбрасывает флаг)
- [x] `cargo test ai_query_mutation_clears_rank_and_receiver ai_cursor_movement_does_not_clear_rank` — зелёные
- [x] `cargo build && cargo test --all` — регрессий нет

### Task 5: Hint [Enter] Rank → [Enter] Resume условный

- [x] в `src/tui/render_search.rs` строки 331-345 изменить hard-coded `"[Enter] Rank"` на условный литерал: `if app.ai.ranked_count.is_some() { "[Enter] Resume" } else { "[Enter] Rank" }`
- [x] стиль `dim` сохранить, остальные hints (`[↑↓] Navigate`, `[Esc/Ctrl+G] Cancel`) не трогать
- [x] если extraction упрощает тестирование — вынести построение AI-hints в `fn build_ai_hints(app: &AppView<'_>) -> Vec<HintItem>` (pure function); иначе оставить inline (вынесено как `build_ai_hints(ranked_count: Option<usize>) -> Vec<HintItem<'static>>` — pure, принимает только релевантный input)
- [x] добавить unit-тест (в `#[cfg(test)] mod tests` в `render_search.rs` или в `state.rs` при inline): собрать `AppView`, проверить, что при `ranked_count.is_some()` hint-vec содержит `"[Enter] Resume"`, иначе — `"[Enter] Rank"`. Если функция не вынесена — тест через `AppView` + проверка через regex/strings.contains по `Line.spans[..]`
- [x] `cargo test render_ai_hints` — зелёный
- [x] `cargo build && cargo test --all` — регрессий нет

### Task 6: Verify acceptance criteria

- [x] sentinel-тест Task 1 зелёный
- [x] все тесты из Task 2/3/4/5 зелёные
- [x] `cargo fmt --check` — без diff'ов
- [x] `cargo clippy --all-targets --all-features -- -D warnings` — без предупреждений
- [x] `cargo test --all` — все тесты зелёные, включая регрессионные (514 unit + 50 integration = 564 всего; baseline ~556 + новые: 1 sentinel + 1 guard + 1 submit-noop + 1 query-mutation + 1 cursor + 3 render_ai_hints = 8 новых → ~564 ✓)
- [x] проверить, что публичный контракт `on_enter` не изменился: grep callsite'ов `.on_enter()` во всём репо, убедиться, что поведение для `ai.active == true` идентично (no-op) — `src/tui/state.rs:940` (KeyAction::Enter => self.on_enter()) + тестовые вызовы; `search_mode.rs:173-177` сохраняет `if self.ai.active { return; }` guard
- [x] проверить, что `on_enter_inner` — `pub(crate)`, не `pub` (минимальная видимость) — `src/tui/search_mode.rs:180` объявлен как `pub(crate) fn on_enter_inner`

## Technical Details

### Data structures

Никаких новых типов / полей. Переиспользуем:

- `AiState::ranked_count: Option<usize>` — флаг «результат применён».
- `AiState::result_rx: Option<Receiver<AiRankResult>>` — канал ожидания AI-ответа, дропаем при правке запроса.
- `AppOutcome::Resume(ResumeTarget)` — сигнал на resume, выставляется через `on_enter_inner`.
- `KeyAction::Enter` — существующая классификация из `dispatch.rs`.

### Processing flow

Текущее поведение (блокер):
```
Enter в AI-режиме → handle_action.AI-блок → submit_ai_query() → spawn new AI rank
```

Новое поведение:
```
Enter в AI-режиме, ranked_count.is_some() → handle_action.AI-блок → on_enter_inner() → Resume
Enter в AI-режиме, ranked_count.is_none() → handle_action.AI-блок → submit_ai_query() → spawn AI rank
Правка query в AI-режиме → ranked_count = None && result_rx = None → next Enter триггерит rank
```

### Rejected alternatives

- **Inline duplicate `on_enter` body в AI-ветке**: дубль, расхождение с обычным путём.
- **Сбросить `ai.active = false` перед `on_enter`**: `exit_ai_mode` сбрасывает курсор — теряется selection; bare-assign обходит helper — ломает инвариант «AI-state меняется только через enter/exit».
- **`ai_submit_seq: u64` для версионирования ответов**: более строгая защита, но требует нового поля и прокидывания seq через `mpsc`. Выбран «дроп receiver» — одна строка, тот же эффект для описанной гонки.
- **`last_submitted_query: Option<String>` сравнение в Enter-ветке**: чище структурно, но +1 поле и новая ветвь сравнения. Отложено.

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification**:
- Ручной прогон: `cargo run` → Ctrl+G → ввести запрос «rust refactor» → Enter → дождаться появления «ranked N» в статус-баре → стрелками выбрать другую сессию → Enter → должна открыться сессия через `claude --resume` (процесс заменяется).
- Ручной race-sanity: Ctrl+G → ввести запрос → Enter → СРАЗУ начать править запрос до прихода результата → статус «AI thinking…» пропадает → status становится снова просто input. Никаких артефактов от стейл-ранкинга.
- Ручной hint-check: hint в help bar показывает `[Enter] Rank` до первого результата и `[Enter] Resume` после; после правки запроса возвращается на `[Enter] Rank`.

**Rebuild on machines** (после коммита и пуша):
- Mac (local): `cd /Users/vkkovalev/projects/claude-code-fullsearch-rust && git pull --ff-only && cargo install --path . --locked && ccs --version`.
- Z270XX (remote, не пулить в `~/projects/...`!): через временный clone в `/tmp/ccs-build` (см. `CLAUDE.local.md`).

**Release** (optional, когда объединим с несколькими другими фиксами):
- bump версии в `Cargo.toml`, `cargo-dist` в CI соберёт релиз, потом `ccs update` на машинах.
