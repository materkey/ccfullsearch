# Recent sessions: Ctrl+A показывает Codex-сессии текущего проекта

## Overview

В режиме recent sessions при нажатии Ctrl+A (project filter) Codex-сессии
исчезают из списка, даже когда юзер находится в той же рабочей директории, в
которой эти сессии были созданы. Причина — структурное расхождение, как два
провайдера хранят свои файлы:

- **Claude Code CLI** кладёт сессии в
  `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, где `<encoded-cwd>`
  получается из cwd через `encode_path_for_claude`
  (`src/resume/path_codec.rs:6`). По имени директории однозначно понятно, к
  какому проекту относится сессия.
- **Codex** кладёт сессии в
  `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-...jsonl` (или под кастомный
  `CODEX_HOME`). В пути проекта нет — cwd сохраняется внутри файла, в записи
  `session_meta.payload.cwd`. Хелпер `session::read_codex_session_cwd(path)`
  (`src/session/mod.rs:326`) уже умеет его доставать.

Текущий фильтр в `RecentState::apply_filter` (`src/tui/state.rs:319-329`)
проверяет только префикс `file_path`, поэтому Codex-сессия никогда не
матчится. Цель — при Ctrl+A показывать Codex-сессии, у которых
`session_meta.payload.cwd` соответствует cwd процесса ccs, и устранить
несколько побочных регрессий, выявленных архитектурным обзором.

## Context (from discovery)

- **Reviewer/Validator/Improver pipeline** уже прошёл по предварительному
  дизайну. Артефакты: `~/.claude/plans/recent-sessions-proud-kernighan*.md`.
- **Файлы, которые меняются**: `src/recent.rs`, `src/tui/state.rs`,
  `src/tui/search_mode.rs`, `src/tui/render_search.rs`.
- **Существующие хелперы для переиспользования**:
  - `session::read_codex_session_cwd(path)` — `src/session/mod.rs:326`
  - `path_is_within_project(file, project)` — `src/tui/state.rs:226`
  - `normalize_path_for_prefix_check` — `src/tui/state.rs:221`
  - `SessionProvider::from_path` — `src/session/mod.rs:51`
- **Конвенции**:
  - Тесты — inline `#[cfg(test)] mod tests` (см. `state.rs:1748`,
    `recent.rs:957`, `session/mod.rs:566`).
  - Именование тестов — снейк-кейс, без given-when-then синтаксиса (это
    глобальный CLAUDE.md, но в ccs-проекте `state.rs` / `recent.rs`
    используют `test_<thing>_<behavior>` стиль; следуем стилю файла).
  - `Result<T, String>` — общий тип ошибок.
- **Корневая причина**: `current_project_paths` строится только в Claude-стиле
  через `encode_path_for_claude` + проверку существования директории
  (`state.rs:651-667`); Codex туда не попадает по определению.

## Development Approach

- **Testing approach**: **TDD** — каждый таск сначала добавляет failing
  юнит-тесты, затем минимальную реализацию, доводящую их до зелёного.
- Делать маленькие, сфокусированные коммиты. Один таск = один коммит, если
  тесты в этом таске не «висят» в ожидании следующего.
- **CRITICAL**: каждый таск обязан включать новые / обновлённые тесты для
  затронутого кода. Запускать `cargo test` (либо узкий путь
  `cargo test <module>`) после каждого таска; зелёные тесты — условие
  перехода к следующему таску.
- **CRITICAL**: каждый таск обязан проходить `cargo fmt --check` и
  `cargo clippy --all-targets --all-features -- -D warnings`. Эти команды CI
  применяет к каждому PR (`.github/workflows/ci.yml`), запускать локально
  перед коммитом.
- **CRITICAL**: обновлять этот файл (`docs/plans/...md`), если в процессе
  всплывают неучтённые подзадачи или меняется scope. Использовать ➕ префикс
  для новых пунктов, ⚠️ для блокеров.
- Сохранять backward compatibility публичного CLI и формата picker-вывода.

## Testing Strategy

- **Unit-тесты** обязательны в каждом таске:
  - Чистые хелперы (`session_matches_project`, `paths_share_project`) —
    тестируются изолированно, без `App`, по образцу
    `test_path_is_within_project_rejects_sibling_prefixes`
    (`src/tui/state.rs:1748-1758`).
  - Парсинг (`extract_summary` для Codex) — через `tempfile` фикстуры, как
    `test_recent_session_extracts_codex_session_with_cwd` (паттерн уже есть
    в `recent.rs:1054`, `recent.rs:1104`).
  - Поведенческие — через `App::new` и подмену `recent.all` / `recent.project`
    (паттерн `app.current_project_paths = vec![...]` уже встречается в
    `state.rs:1555`, `2898`, `3156`).
- **E2E тесты**: проект use `assert_cmd` для интеграционных тестов
  бинарника. Эту фичу не нужно покрывать через `assert_cmd` — она целиком
  TUI-side, нет CLI-флагов. Достаточно ручной верификации в шаге Verify.
- **Ручная верификация** обязательна в финальном таске: `cargo run` →
  recent sessions → Ctrl+A → проверить, что Codex-сессии текущего проекта
  остались.

## Progress Tracking

- Отмечать выполненные пункты `[x]` сразу же.
- ➕ — новые подзадачи, всплывшие по ходу.
- ⚠️ — блокеры / обнаруженные проблемы.
- В конце — двинуть план в `docs/plans/completed/`.

## Solution Overview

Подход состоит из пяти кусков, которые ложатся в пять рабочих тасков
(плюс verify + docs):

1. **Расширить `RecentSession`** полем `cwd: Option<String>`, заполнять для
   Codex из `read_codex_session_cwd` (Task 1).
2. **Расширить `App`** полем `current_cwd: Option<String>`, канонизировать
   через `std::fs::canonicalize` (с fallback на raw) — решает macOS
   `/var` ↔ `/private/var` и симлинки в `~/projects` (Task 2). В этом же
   таске обновить `test_toggle_project_filter_no_current_project`, чтобы CI
   не падал между Task 2 и Task 5.
   - **Task 1.5**: симметрично канонизировать `session.cwd` в
     `extract_summary` — иначе на macOS симлинк-фермах Codex (который
     пишет логический путь через `canonicalize_preserving_symlinks`) и
     наш `current_cwd` (через `std::fs::canonicalize`) разойдутся, и
     фильтр упустит сессии. Подробности — Technical Details > «Codex cwd».
3. **Симметричный предикат `session_matches_project`**: для `file_path` —
   старая односторонняя проверка (Claude всегда лежит ВНУТРИ проектной
   директории); для cwd-сравнения — симметричный `paths_share_project`
   (любая сторона может содержать другую — покрывает «ccs в поддиректории
   монорепо» и «session записан из подмодуля») (Task 3).
4. **Union-источник в `apply_filter`** + согласованный `total_count` +
   подсказка в `recent_sessions_status_text` (Task 4):
   - `self.project` ∪ Codex-сессии из `self.all`, дедуп по `session_id`.
     Без union'а после resolve `start_project_load` Codex-сессии
     **исчезают** из выдачи — ровно та регрессия, ради устранения которой
     делается задача.
   - `total_count` сохраняет семантику «pre-filter source size» — считает
     ту же union'ную выборку, чтобы строка «N recent (M hidden by
     filter)» оставалась верной.
   - `recent_sessions_status_text` дописывает хвост «· Codex: ≤100 by
     recency», когда `project_filter && current_cwd.is_some()` — делает
     асимметрию с Claude видимой.
5. **`toggle_project_filter`** — релаксированный early-return и fallback
   `search_paths = all_search_paths` для Codex-only проектов; иначе
   ripgrep остаётся с пустым scope и не находит ничего (Task 5).

## Technical Details

### Codex cwd: что лежит в `payload.cwd` и как его сравнивать

Источник: `~/projects/codex/`, проверено по live-коду, не по документации.

- Codex кладёт в `SessionMeta.cwd` результат `config.cwd().to_path_buf()`
  (`codex-rs/rollout/src/recorder.rs:699`). Сам `config.cwd()` идёт через
  `canonicalize_existing_preserving_symlinks` (`codex-rs/tui/src/lib.rs:678`),
  либо через `AbsolutePathBuf::current_dir()` если cwd не передан явно.
- `canonicalize_preserving_symlinks` отличается от `std::fs::canonicalize`:
  она оставляет «логические» симлинк-пути (`~/projects` остаётся, если это
  симлинк-ферма), но top-level алиасы (`/var → /private/var`) всё равно
  разрезолвливаются (`codex-rs/utils/absolute-path/src/lib.rs:189-197`).
- Сам Codex для матчинга cwd при resume использует
  `paths_match_after_normalization`, которая канонизирует **обе** стороны
  через обычный `std::fs::canonicalize` симметрично
  (`codex-rs/utils/path-utils/src/lib.rs:13-29`).

**Следствие для ccs**: канонизировать только `current_cwd` через
`std::fs::canonicalize` (как в Task 2) **недостаточно**. На macOS с
симлинком `~/projects → /Volumes/data/projects` Codex запишет логический
путь `/Users/me/projects/foo`, а наш `current_cwd` после canonicalize
будет `/Volumes/data/projects/foo` — fil`session_matches_project` вернёт
`false`, фильтр упустит сессию. Нужно симметрично канонизировать **обе**
стороны:

- **`current_cwd`** — `std::fs::canonicalize` один раз в `App::new`
  (Task 2, уже в плане).
- **`session.cwd`** — `std::fs::canonicalize` один раз при
  `extract_summary`, до сохранения в `RecentSession.cwd` (Task 1.5).

Один лишний syscall на Codex-сессию ничтожен по сравнению с
`read_codex_session_cwd`, которая уже читает первые 50 строк файла.

### Структуры данных

```rust
// recent.rs: RecentSession (новое поле)
pub struct RecentSession {
    // ... existing fields ...
    /// Working directory recorded inside the session metadata. Currently
    /// filled only for Codex rollouts (`session_meta.payload.cwd`); Claude
    /// sessions encode the project in the file path itself.
    pub cwd: Option<String>,
}

// state.rs: App (новое поле)
pub struct App {
    // ... existing ...
    /// Canonicalized cwd of the ccs process. Used to match Codex sessions
    /// (their cwd is stored inside the session file, not in the file path).
    pub current_cwd: Option<String>,
}
```

### Чистые хелперы (state.rs, рядом с `path_is_within_project`)

```rust
/// Symmetric "same project" check: true if either path contains the other
/// (or they're equal). Codex sessions can be recorded from any subdir of
/// the project, and ccs can be launched from any subdir too — neither
/// direction is privileged.
fn paths_share_project(a: &str, b: &str) -> bool {
    path_is_within_project(a, b) || path_is_within_project(b, a)
}

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
        (Some(session_cwd), Some(cwd)) => paths_share_project(session_cwd, cwd),
        _ => false,
    }
}
```

### `RecentState::apply_filter`

Новая подпись:
```rust
pub(crate) fn apply_filter(
    &mut self,
    project_filter: bool,
    project_paths: &[String],
    current_cwd: Option<&str>,
    automation_filter: &AutomationFilter,
)
```

Тело (ключевой union):
```rust
let project_filtered: Vec<_> = if project_filter
    && (!project_paths.is_empty() || current_cwd.is_some())
{
    let mut source: Vec<RecentSession> = self.project.clone().unwrap_or_default();
    let seen: HashSet<&str> = source.iter().map(|s| s.session_id.as_str()).collect();
    let extra: Vec<RecentSession> = self
        .all
        .iter()
        .filter(|s| {
            SessionProvider::from_path(&s.file_path) == SessionProvider::Codex
                && !seen.contains(s.session_id.as_str())
        })
        .cloned()
        .collect();
    drop(seen);
    source.extend(extra);
    source
        .into_iter()
        .filter(|s| session_matches_project(&s, project_paths, current_cwd))
        .collect()
} else {
    self.all.clone()
};
```

### `toggle_project_filter` (search_mode.rs)

```rust
pub fn toggle_project_filter(&mut self) {
    if self.current_project_paths.is_empty() && self.current_cwd.is_none() {
        return;
    }
    if self.ai.active {
        self.invalidate_ai_rank();
    }
    self.project_filter = !self.project_filter;
    self.search_paths = if self.project_filter && !self.current_project_paths.is_empty() {
        self.current_project_paths.clone()
    } else {
        self.all_search_paths.clone()
    };
    if self.project_filter && !self.current_project_paths.is_empty() {
        self.recent
            .start_project_load(self.current_project_paths.clone());
    }
    self.apply_recent_sessions_filter();
    if !self.input.is_empty() {
        self.last_keystroke = Some(Instant::now());
        self.typing = true;
    }
}
```

### `App::new` (state.rs)

```rust
// Canonicalize once: macOS resolves `/var` → `/private/var`, and dev boxes
// routinely symlink `~/projects`. Codex stores its `cwd` verbatim, so the
// side that we control (ccs's own cwd) must match the OS-canonical form.
// Fall back to the raw path if canonicalize fails (rare; e.g. cwd removed).
let current_cwd = std::env::current_dir().ok().and_then(|cwd| {
    std::fs::canonicalize(&cwd)
        .ok()
        .unwrap_or(cwd)
        .to_str()
        .map(String::from)
});
```

### `extract_summary` (recent.rs)

В начале функции:
```rust
let cwd = (SessionProvider::from_path(path_str) == SessionProvider::Codex)
    .then(|| session::read_codex_session_cwd(path_str))
    .flatten();
```

Передавать `cwd: cwd.clone()` в первые три `Some(RecentSession { ... })`
(строки 716, 732, 748) и `cwd` без клона в последний (строка 777).

### `total_count` (state.rs)

Цель — сохранить семантику «total = размер pre-filter source», чтобы
`recent_sessions_status_text` (`render_search.rs:216-243`) продолжал
корректно показывать `"N recent sessions (M hidden by filter)"`. После
union'а pre-filter source — это объединение Claude project-load и Codex
из `self.all`:

```rust
/// Total count of sessions in the active source for the status bar
/// (pre `session_matches_project` / pre `automation_filter`). After the
/// union change above, when project_filter is on the source is a union of
/// `self.project` (Claude) and Codex sessions in `self.all`; otherwise
/// `self.all`. This keeps the "X recent (Y hidden by filter)" message
/// in `recent_sessions_status_text` coherent.
pub fn total_count(&self, project_filter: bool) -> usize {
    if project_filter {
        let mut total = self
            .project
            .as_ref()
            .map(|p| p.len())
            .unwrap_or(self.all.len());
        // When `self.project` is loaded, count Codex sessions from `self.all`
        // that are not already in `self.project` (matches the union built in
        // apply_filter). Skip when project hasn't loaded yet — `self.all`
        // already contains Codex sessions in that case.
        if let Some(project) = self.project.as_ref() {
            let seen: HashSet<&str> =
                project.iter().map(|s| s.session_id.as_str()).collect();
            total += self
                .all
                .iter()
                .filter(|s| {
                    SessionProvider::from_path(&s.file_path)
                        == SessionProvider::Codex
                        && !seen.contains(s.session_id.as_str())
                })
                .count();
        }
        total
    } else {
        self.all.len()
    }
}
```

Дублирование с `apply_filter` не выносим: ленивая union'-итерация
быстрая, а абстракция связала бы две функции жёстче, чем нужно. Если
позже появится Codex-аналог `start_project_load`, обе ветки исчезнут
вместе.

### Статус-бар (render_search.rs)

Реальный статус — это `String`, возвращаемый из
`recent_sessions_status_text` (`render_search.rs:216-243`). Никакого
`spans`-буфера тут нет. Дописываем подсказку `· Codex: ≤100 by recency`
к уже сформированной строке, когда `project_filter && current_cwd.is_some()`:

```rust
fn recent_sessions_status_text(app: &AppView) -> Option<String> {
    if !app.input.is_empty() {
        return None;
    }
    if app.recent.is_loading(app.project_filter) {
        return Some("Loading recent sessions...".to_string());
    }
    let total = app.recent.total_count(app.project_filter);
    let shown = app.recent.filtered.len();
    let mut text = if shown > 0 {
        if shown < total {
            format!("{} recent sessions ({} hidden by filter)", shown, total - shown)
        } else {
            format!("{} recent sessions", shown)
        }
    } else if total > 0 {
        format!("0 recent sessions ({} hidden by filter)", total)
    } else {
        "No recent sessions found".to_string()
    };
    if app.project_filter && app.current_cwd.is_some() {
        // start_project_load scans only Claude paths, so Codex sessions for the
        // current project are visible to the filter only while they fit in the
        // global top-RECENT_SESSIONS_LIMIT (= 100) by mtime. Make the limit
        // visible so missing Codex sessions don't look like a bug.
        text.push_str(" · Codex: ≤100 by recency");
    }
    Some(text)
}
```

Существующие тесты `recent_sessions_status_text_*` живут в `mod tests`
этого файла — обновляем их и добавляем новый под подсказку.

## What Goes Where

- **Implementation Steps** — `[ ]` чекбоксы ниже.
- **Post-Completion** — ручная верификация и обновление CHANGELOG, без
  чекбоксов.

## Implementation Steps

### Task 1: Add `cwd` field to `RecentSession` and populate from Codex

**Files:**
- Modify: `src/recent.rs`

- [x] (TDD) добавить в `recent.rs` модуль `tests` тест
      `test_extract_summary_codex_fills_cwd` — построить tempfile Codex
      rollout с `session_meta.payload.cwd = "/tmp/codex-proj"`, проверить,
      что `extract_summary(...)?.cwd == Some("/tmp/codex-proj".into())`.
      Опереться на существующие codex-фикстуры (recent.rs:1054, 1104).
- [x] (TDD) добавить тест `test_extract_summary_claude_cwd_is_none` —
      обычная Claude CLI фикстура; убедиться, что `cwd == None`.
- [x] прогнать `cargo test recent::tests::test_extract_summary_codex_fills_cwd`
      — должен **упасть** (поле ещё не существует). Зафиксировать как
      red phase.
- [x] добавить публичное поле `pub cwd: Option<String>` в `RecentSession`
      (`recent.rs:17-35`) с doc-комментом из Technical Details.
- [x] в `extract_summary` (`recent.rs:615`) до сборки результата вычислить
      `let cwd = (SessionProvider::from_path(path_str) == ...).then(...).flatten();`.
- [x] передать `cwd` (клон в первые три ветки, последний — без клона) во все
      четыре `Some(RecentSession { ... })` (строки 716, 732, 748, 777).
- [x] обновить **все** literal-конструкции `RecentSession { ... }` в `src/`
      — добавить `cwd: None`. По состоянию на план: ~29 сайтов в
      `recent.rs`, `tui/state.rs`, `tui/search_mode.rs`, `tui/render_search.rs`.
      Метод поиска: `cargo build` → пройти по всем «missing field `cwd`»
      ошибкам компилятора, а не глазами. Альтернатива —
      `grep -rn "RecentSession {" src/`.
- [x] прогнать `cargo test recent::tests` — все тесты должны проходить.
- [x] прогнать `cargo fmt --check`, `cargo clippy -- -D warnings` для затронутых
      файлов.

### Task 1.5: Canonicalize `session.cwd` for symmetric matching

**Files:**
- Modify: `src/recent.rs`

**Why this exists** — после сверки с `~/projects/codex/` (см. Technical
Details > «Codex cwd»): Codex пишет cwd через
`canonicalize_existing_preserving_symlinks` (логические симлинк-пути
сохраняются), а Task 2 канонизирует наш `current_cwd` через
`std::fs::canonicalize` (резолвит всё). Без выравнивания обеих сторон на
macOS симлинк-фермах фильтр упустит сессии. Минимальное выравнивание —
прогнать `session.cwd` через тот же `std::fs::canonicalize` при чтении.

- [x] (TDD) добавить тест `test_extract_summary_codex_canonicalizes_cwd` в
      `recent.rs::tests` — кросс-Unix через `tempfile::TempDir` +
      `std::os::unix::fs::symlink`:
  - создать реальную директорию `real`;
  - создать симлинк `link → real`;
  - построить Codex rollout с `payload.cwd = link_path`;
  - `extract_summary(...)?.cwd` должно вернуть `canonicalize(real_path)`,
    т. е. реальный путь, а **не** путь через симлинк.
- [x] (TDD) обновить существующий `test_extract_summary_codex_fills_cwd` —
      использовать абсолютный путь без симлинков (TempDir-based), чтобы
      `canonicalize` его не менял; проверить эквивалентность ожидания.
- [x] прогнать новый тест — должен **упасть** (canonicalize ещё не
      применяется).
- [x] в `extract_summary` (`src/recent.rs`) обернуть результат
      `read_codex_session_cwd`:
      ```rust
      let cwd = (SessionProvider::from_path(path_str) == SessionProvider::Codex)
          .then(|| session::read_codex_session_cwd(path_str))
          .flatten()
          .map(|raw| {
              std::fs::canonicalize(&raw)
                  .ok()
                  .and_then(|p| p.to_str().map(String::from))
                  .unwrap_or(raw)
          });
      ```
      Fallback на сырой путь — если файл/директория удалены или путь не
      резолвится; лучше сохранить хоть что-то, чем ничего.
- [x] прогнать `cargo test recent::tests` — все зелёные.
- [x] `cargo fmt --check`, `cargo clippy -- -D warnings`.

### Task 2: Add `current_cwd` field to `App` with canonicalization

**Files:**
- Modify: `src/tui/state.rs`

- [x] (TDD) в `#[cfg(test)] mod tests` `state.rs` добавить тест
      `test_app_new_captures_current_cwd` — `App::new(vec![tempdir.path()...])`
      из временной директории; проверить, что `app.current_cwd.is_some()`.
      Это поведенческий тест canonicalize fallback пути.
- [x] (TDD) добавить тест `test_app_new_current_cwd_is_canonicalized` —
      кросс-Unix: `tempfile::TempDir` создаёт реальную директорию,
      `std::os::unix::fs::symlink` указывает на неё, переходим в симлинк
      через `std::env::set_current_dir(&link)`, затем `App::new(...)`
      должен вернуть `current_cwd` равный канонической форме (т. е.
      реальной TempDir, а не симлинку). Тест общий, без `#[cfg(macos)]`
      — Linux CI runner тоже должен его запускать.
- [x] прогнать `cargo test state::tests::test_app_new_captures_current_cwd`
      — должен **упасть** (поле не существует).
- [x] добавить публичное поле `pub current_cwd: Option<String>` в `App`
      (`state.rs:629-631`) с doc-комментом.
- [x] в `App::new` (`state.rs:651`) посчитать `current_cwd` через
      `std::env::current_dir().ok().and_then(|cwd| { std::fs::canonicalize(&cwd).ok().unwrap_or(cwd).to_str().map(String::from) })`
      (см. Technical Details).
- [x] **Сразу же** обновить `test_toggle_project_filter_no_current_project`
      (`search_mode.rs:376`): после Task 2 `App::new` будет заполнять
      `current_cwd = Some(...)`, и условие early-return больше не
      сработает по `current_project_paths.is_empty()` одному —
      `app.current_cwd = None` нужно проставлять явно перед вызовом
      `toggle_project_filter()`. Без этой правки CI ляжет между Task 2 и
      Task 5. (Релаксация раннего return'а в самом `toggle_project_filter`
      случается в Task 5; здесь только обновление существующего теста под
      новый `App::new`.)
- [x] прогнать `cargo test state::tests` и `cargo test search_mode::tests`
      — зелёные.
- [x] прогнать `cargo fmt --check`, `cargo clippy -- -D warnings`.

### Task 3: Pure helpers — `paths_share_project` and `session_matches_project`

**Files:**
- Modify: `src/tui/state.rs`

- [x] (TDD) добавить восемь прямых тестов в `state.rs` `mod tests`, рядом с
      `test_path_is_within_project_rejects_sibling_prefixes`:
  - [x] `test_session_matches_project_same_cwd` —
        `session.cwd == current_cwd` → true.
  - [x] `test_session_matches_project_session_in_subdir_of_current` —
        `current_cwd = /repo`, `session.cwd = /repo/sub` → true.
  - [x] `test_session_matches_project_current_in_subdir_of_session` —
        `current_cwd = /repo/sub`, `session.cwd = /repo` → true (ловит
        левостороннюю реализацию).
  - [x] `test_session_matches_project_siblings` — `/repo` vs `/repo-other`
        → false.
  - [x] `test_session_matches_project_no_cwd_either_side` — обе `cwd ==
        None`, `project_paths` пустой → false.
  - [x] `test_session_matches_project_only_current_cwd` — `session.cwd ==
        None`, `current_cwd == Some(...)` → false.
  - [x] `test_session_matches_project_only_session_cwd` — обратный случай
        → false.
  - [x] `test_session_matches_project_file_path_match_overrides_cwd` —
        матч по file_path короткий путь → true даже при cwd-mismatch.
- [x] прогнать `cargo test state::tests::test_session_matches_project` —
      должны **упасть** (хелперов нет).
- [x] добавить `fn paths_share_project(a: &str, b: &str) -> bool` рядом с
      `path_is_within_project` (`state.rs:226`).
- [x] добавить `fn session_matches_project(session: &RecentSession,
      project_paths: &[String], current_cwd: Option<&str>) -> bool` (см.
      Technical Details).
- [x] прогнать `cargo test state::tests::test_session_matches_project` —
      зелёные.
- [x] прогнать `cargo test --lib` целиком, чтобы убедиться, что добавление
      приватных хелперов не сломало чужие тесты.
- [x] `cargo fmt --check`, `cargo clippy -- -D warnings`.

### Task 4: Union filter source + `total_count` + status-text hint

**Files:**
- Modify: `src/tui/state.rs`
- Modify: `src/tui/render_search.rs`

- [x] (TDD) добавить тест `test_apply_filter_keeps_codex_in_all_after_claude_project_load`:
  - построить два `RecentSession`:
    одна Claude (`file_path = "/proj/-Users-x-y/abc.jsonl"`, `cwd = None`),
    одна Codex (`file_path = ".../rollout-...jsonl"`, `cwd =
    Some("/proj")`);
  - положить Claude в `state.project = Some([claude])` и Codex в
    `state.all = vec![codex]`;
  - вызвать `apply_filter(true, &[], Some("/proj"), &AutomationFilter::All)`;
  - утверждать, что `filtered.len() == 2` и Codex присутствует.
- [x] (TDD) добавить тест
      `test_apply_filter_drops_codex_with_unrelated_cwd` — Codex с
      `cwd = Some("/other-proj")` при `current_cwd = Some("/proj")` не
      попадает в `filtered`.
- [x] (TDD) добавить тест `test_total_count_includes_codex_in_union` —
      `state.project = Some([claude])`, `state.all = vec![codex]`;
      `total_count(true)` должно вернуть `2` (Claude из project + Codex
      из all через union).
- [x] (TDD) обновить / добавить тесты на `recent_sessions_status_text` в
      `render_search.rs::tests` (там уже есть аналогичные кейсы):
  - `text_includes_hidden_by_filter_when_some_filtered_out` — при
    `total > shown` строка содержит `"hidden by filter"`.
  - `text_appends_codex_top100_hint_when_project_filter_and_cwd_set` —
    при `app.project_filter && app.current_cwd.is_some()` хвост строки
    содержит `"· Codex: ≤100 by recency"`.
- [x] прогнать новые тесты — **упадут** (apply_filter ещё не union'ит,
      total_count считает иначе).
- [x] расширить подпись `apply_filter` параметром `current_cwd:
      Option<&str>` (см. Technical Details), импортировать
      `SessionProvider` (use `crate::session::SessionProvider;`),
      использовать `HashSet`.
- [x] переписать тело `apply_filter` по схеме union (см. Technical
      Details). Для `seen` использовать `HashSet<String>` с клонами
      `session_id`, чтобы убрать `drop(seen)` и lifetime-гимнастику с
      `&str`:
      ```rust
      let seen: HashSet<String> =
          source.iter().map(|s| s.session_id.clone()).collect();
      ```
      ⚠️ Поправка от реализации: union применяется только когда
      `self.project.is_some()`. Если project ещё не загрузился, source
      падает на `self.all.clone()`, иначе после нажатия Ctrl+A список
      Claude-сессий мигнёт в пустой между toggle и завершением
      `start_project_load`. Тест
      `test_apply_recent_sessions_filter_matches_mixed_separators`
      выявил эту регрессию.
- [x] в `apply_recent_sessions_filter` (`state.rs:1203-1209`) пробросить
      `self.current_cwd.as_deref()`.
- [x] переписать `total_count` по схеме union (см. Technical Details:
      сохраняет семантику «pre-filter source size» и работает с
      `recent_sessions_status_text`).
- [x] обновить `recent_sessions_status_text` (`render_search.rs:216-243`)
      — добавить хвост `· Codex: ≤100 by recency`, когда
      `app.project_filter && app.current_cwd.is_some()` (см. Technical
      Details). Технически это правка для статус-бара, но она ОБЯЗАНА
      произойти в Task 4: иначе тесты на `total_count` не покрывают
      end-to-end вид строки, а Task 6 работает только с подсказкой.
- [x] `grep -n "total_count(" src/` — пройти по всем вызовам, убедиться,
      что никто не сломался от смены семантики union. Зафиксировать ➕,
      если найдётся проблема.
- [x] прогнать `cargo test` целиком — должны проходить и старые, и новые.
- [x] `cargo fmt --check`, `cargo clippy -- -D warnings`.

### Task 5: Relax `toggle_project_filter` + `search_paths` fallback

**Files:**
- Modify: `src/tui/search_mode.rs`

- [x] (TDD) добавить тест `test_toggle_project_filter_codex_only_project`:
  - `app.current_project_paths = vec![]` (нет Claude-директории);
  - `app.current_cwd = Some("/codex-only".into())`;
  - вызвать `app.toggle_project_filter()`;
  - утверждать `app.project_filter == true`, `app.search_paths ==
    app.all_search_paths`.
- [x] (TDD) обновить существующий `test_toggle_project_filter_no_current_project`
      (`search_mode.rs:376`), чтобы он использовал `current_cwd = None`
      явно (раньше неявно подразумевалось).
- [x] прогнать новые / обновлённые тесты — **упадут** (старое поведение).
- [x] расслабить early-return: `if self.current_project_paths.is_empty() &&
      self.current_cwd.is_none() { return; }` (`search_mode.rs:167-169`).
- [x] заменить безусловное `self.search_paths = ...` на вариант с
      fallback на `all_search_paths` (см. Technical Details).
- [x] оставить `start_project_load` под условием
      `!self.current_project_paths.is_empty()` — нет смысла грузить
      пустой список Claude paths.
- [x] прогнать `cargo test search_mode::tests` — зелёные.
- [x] прогнать `cargo test` целиком (на случай регресса в state.rs тестах).
- [x] `cargo fmt --check`, `cargo clippy -- -D warnings`.

### Task 6: Verify acceptance criteria

- [x] прогнать полный `cargo test` — все зелёные (644 + 8 + 6 + 2 + 3 + 9 + 6 + 9 + 10 = 697 тестов).
- [x] прогнать `cargo fmt --check` — без замечаний.
- [x] прогнать `cargo clippy --all-targets --all-features -- -D warnings` — без предупреждений.
- [x] sanity-проверка бинаря: `cargo run -- list` запустился, в выводе
      видны и Codex (`provider: "Codex"`), и Claude (`provider: "Claude"`)
      сессии текущего проекта — recent sessions scanning работает,
      Codex-источники присутствуют. Полная интерактивная Ctrl+A-проверка
      end-to-end покрыта юнит-тестами:
      - `test_apply_filter_keeps_codex_in_all_after_claude_project_load`
        (`state.rs::tests`) — Codex остаётся в filtered после Claude
        project load.
      - `test_apply_filter_drops_codex_with_unrelated_cwd` — чужие
        Codex-сессии скрыты.
      - `test_total_count_includes_codex_in_union` — статус-бар
        «X / Y hidden» считает union корректно.
      - `text_appends_codex_top100_hint_when_project_filter_and_cwd_set`
        (`render_search.rs::tests`) — подсказка «· Codex: ≤100 by recency».
      - `test_toggle_project_filter_codex_only_project`
        (`search_mode.rs::tests`) — Codex-only проект (Claude-директории
        нет): toggle переключает project_filter и оставляет
        search_paths = all_search_paths.
      Интерактивный TUI Ctrl+A без PTY-окружения не симулируется из
      агента; финальный визуальный smoke остаётся за пользователем перед
      релизом (см. Post-Completion).

### Task 7: [Final] Update documentation

- [x] обновить CLAUDE.md в проекте, если появился новый паттерн (например,
      объяснение про union в `apply_filter` стоит зафиксировать в секции
      «Key data flow → Recent sessions»).
- [x] двинуть этот план в `docs/plans/completed/2026-05-11-recent-sessions-codex-ctrl-a.md`.

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes,
informational only*

**Manual verification:**
- Прогнать `cargo run --release` локально и проверить, что задержка
  загрузки recent sessions не вырастает заметно из-за дополнительного
  `read_codex_session_cwd` (rayon-параллельный, читает первые 50 строк, но
  стоит проверить на большом корпусе Codex-сессий).
- Если на машине есть симлинк-фермы (типичный кейс на macOS,
  `/Users/vkkovalev/projects` иногда символическая ссылка), проверить, что
  canonicalize действительно совпадает с тем, что Codex пишет в
  `payload.cwd`.

**Release workflow:**
- После мерджа: использовать skill `release-ccs` для bump-а версии,
  обновления CHANGELOG и публикации релиза через cargo-dist.

**Future enhancement** (отдельный план, не часть этой задачи):
- Расширить `start_project_load`, чтобы он сканировал Codex search paths
  из `all_search_paths` и фильтровал результаты по cwd — это уберёт
  ограничение «top-100» и подсказку из статус-бара.
