# Codex: склейка истории по `history_base` и `ccs show --tail`

## Overview

Codex 0.164 хранит историю треда не в одном файле. Новый rollout-файл не
копирует старые записи, а ссылается на префикс другого файла через
`session_meta.payload.history_base`:

```json
"history_base": {"thread_id": "<rollout id базового файла>",
                 "end_ordinal_exclusive": 610, "end_byte_offset": 3089108}
```

Так бывает в двух случаях:

- **revert**: тред тот же, `session_meta.id` не меняется, новый файл
  называется `rollout-<ts>-<thread>_<rollout>.jsonl`. Хвост старого файла после
  `end_byte_offset` из истории выпадает;
- **пользовательский форк** (`/fork`, Fork в VS Code): новый тред,
  `forked_from_id` указывает на родителя, `history_base` — на его файл. Форк от
  форка даёт цепочку (на этой машине есть цепочки по 5 уровней).

`ccs` поле `history_base` не читает. Что сломано сейчас (проверено на
`01a178da-2e05-7720-b6ba-6b4edd17fc27`):

- `ccs list` показывает revert-продолжение как сессию из 6 сообщений; 609 строк
  истории в базовом файле не видны;
- `ccs search` даёт хитам из продолжения `session_id` =
  `01a178da…_01a17913…` (разбор имени файла не знает суффикса `_<rollout>`):
  в TUI одна сессия разваливается на две группы, а Enter на такой группе
  запускает `codex resume 01a178da…_01a17913…`;
- `ccs show` по продолжению видит только его записи;
- у форков число сообщений, `show` и превью не включают унаследованную
  историю.

Вторая часть — у `ccs` нет ответа на вопрос «на чём остановилась сессия
`<id>`». `ccs show` требует путь к файлу и `--line`/`--uuid` из поиска. Агент со
скиллом `ccs` в итоге разбирает JSONL через python.

Цель:

1. Одна модель истории Codex-сессии: файл = префиксы базовых файлов + свои
   записи. Её используют все, кто читает сессию целиком: подсчёт сообщений,
   `list`, недавние, `show`, `serve`.
2. `ccs show <session_id|file> --tail N` — последние N сообщений текущей ветки
   сессии, в том числе на удалённом хосте. Скилл `ccs` получает отдельный
   сценарий «на чём остановилась сессия».

## Context (from discovery)

- Codex-формат в `ccs`: `session/record.rs` (`parse_codex_*`) — у записей нет
  `uuid`, история линейная; `session/mod.rs` — `SessionProvider::from_path`,
  `extract_codex_session_id_from_path` (`:119`), `extract_session_id`
  (`:132`, берёт `session_meta.payload.id`), `read_codex_session_cwd`.
- Читатели файла сессии целиком, каждый открывает файл сам:
  `search::count_session_messages` (`search/group.rs:118`, его зовут `cli list`,
  `recent.rs:927`, `tui/state.rs:1838`), `cli::extract_session_metadata`
  (`cli.rs:795`), `cli::locate_jsonl_message` (`cli.rs:506`, общий для `ccs show`
  и `ccs serve`), сканеры `recent.rs` (`scan_head`, `scan_tail`, `scan_middle`).
- Группировка поиска: `search/group.rs:group_by_session` — по
  `message.session_id`, `file_path` группы = файл первого хита;
  `ripgrep.rs:resolve_to_parent` уже переносит хиты сабагентов в родительскую
  сессию — тот же приём «хит физически в файле A, принадлежит сессии B».
- Дедуп по `session_id` с сохранением самого нового: `cli.rs:718`,
  `recent.rs:1177`. Базовый файл revert и продолжение имеют один
  `session_meta.id`, поэтому дедуп уже прячет старый файл — нужно только, чтобы
  у продолжения считались полные данные.
- Codex сам ищет базовый файл по rollout id обходом имён в
  `<codex_home>/sessions` и `<codex_home>/archived_sessions`
  (`codex-rs/rollout/src/list.rs:find_rollout_path_by_rollout_id`), а текущий
  файл треда — как самый новый файл с этим thread id в имени
  (`find_thread_path_by_id_str`). Разбор имён:
  `rollout/src/rollout_file_name.rs`.
- Remote: CLI `ccs show --host` (`cli.rs:show_remote`) запускает на хосте
  `ccs show …` напрямую через ssh, мимо протокола `serve`. Протокол `serve`
  для `show` отдаёт одно сообщение для превью TUI (`WireShow { content,
  text_content }`); `remote/server.rs` зовёт те же `locate_jsonl_message` /
  `collect_recent_sessions`, поэтому склейка истории доходит до remote сама.
- Поиск файла по `session_id` уже есть: `session::find_session_file_in_paths`
  (`session/mod.rs:465`), в том числе для Desktop (`audit.jsonl`, поиск по
  содержимому) и Opencode.
- `recent::resolve_summary` (`recent.rs:834`) выбрасывает сессию без заголовка:
  revert-продолжение или форк без своего промпта пропали бы из недавних.
- Скилл `ccs` лежит в двух копиях с разным текстом:
  `.claude/skills/ccs/SKILL.md` и `.agents/skills/ccs/SKILL.md`.
- На этой машине 1346 rollout-файлов, обход имён — ~10 мс; `.jsonl.zst` нет.

## Development Approach

- **testing approach**: TDD — сначала фикстура и падающий тест на поведение
  потребителя, потом код.
- complete each task fully before moving to the next
- make small, focused changes; одна задача = один коммит (рефакторинг отдельно
  от изменения поведения)
- **CRITICAL: every task MUST include new/updated tests** for code changes in
  that task
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test` после каждой задачи
- maintain backward compatibility: сессии без `history_base`, Claude, Desktop и
  Opencode читаются ровно как раньше

## Testing Strategy

- **unit tests**: inline `#[cfg(test)]`, имена новых тестов в форме
  `<что>_when_<условие>_then_<результат>`, как в `tests/cli_hosts.rs`.
- **фикстуры**: rollout-файлы с `history_base` нельзя держать статикой —
  `end_byte_offset` зависит от байтов файла. Тестовый хелпер пишет во временный
  корень `<tmp>/.codex/sessions/YYYY/MM/DD/`: компонент `.codex` обязателен,
  по нему `SessionProvider::from_path` узнаёт Codex; `CODEX_HOME` тесты не
  ставят. Хелпер пишет root-сессию, revert-продолжение и цепочку форк ← форк;
  смещение = длина базового файла после записи включённых строк, потом
  дописываются отброшенные. Хелпер по одному на сторону: в тестах модуля
  `codex_history` и в `tests/common/codex.rs` — как сейчас каждый
  `tests/cli_*.rs` держит свои `setup_*`; в публикуемый крейт тестовый код не
  попадает.
- **integration**: `tests/cli_list.rs`, `tests/cli_show.rs`,
  `tests/cli_search.rs`, удалённый `show` через `tests/fixtures/fake-ssh.sh`;
  `assert_cmd` с
  `CCFS_SEARCH_PATH` на временный корень.
- e2e UI-тестов в проекте нет; TUI проверяется через `render_snapshots.rs` там,
  где меняется вывод.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

**Одна точка, где живёт знание о сегментах.** Новый модуль
`src/session/codex_history.rs`. Только он знает про имена rollout-файлов,
`history_base`, поиск базовых файлов, `codex_home` по пути и то, какой файл
треда текущий. Наружу он отдаёт одно понятие —
историю файла как упорядоченную последовательность строк, у каждой из которых
есть физический адрес `(path, line_number)`. Для всего, что не Codex, и для
Codex-файлов без `history_base` это просто строки самого файла, поэтому
потребители переводятся на него без веток «если Codex».

**Физические адреса не меняются.** `line_number` в выводе поиска и в
`ccs show --line` по-прежнему значит строку в том файле, который указан в
`file_path`. Сквозной «логической» нумерации нет — иначе пришлось бы держать
два смысла одного поля. Строки из базовых файлов в `ccs show` помечаются своим
`file_path`.

**Идентичность сессии.** `session_id` Codex-файла — thread id (из
`session_meta.payload.id`, запасной вариант — имя файла без суффикса
`_<rollout>`). Текущий файл треда — самый новый файл с этим thread id в имени,
порядок как у Codex: время из имени, при равенстве rollout id. Это решает одна
функция в `codex_history`. Через неё идут группа поиска, `list`, недавние,
`show <session_id>` и уже существующие поиски файла по thread id
(`find_session_file_in_paths`, `find_codex_rollout_near`). Если текущий файл
найти нельзя, остаётся сам файл.

**Заголовок** в недавних и в `list` берётся из записей самого файла (первый
свой промпт), чтобы форки одного родителя различались. Если своего промпта ещё
нет, берётся первый промпт полной истории — иначе сессия пропала бы из
недавних. Число сообщений, `show` и `--tail` считаются по полной истории.

**`--tail`** — режим `ccs show` без якоря: последние N сообщений текущей ветки.
Claude — последняя цепочка DAG (`TipStrategy::LastAppended`, как при resume);
Codex — история из `codex_history`; Opencode — последние сообщения из SQLite.

**Протокол `ccs serve` не меняется.** Удалённый `ccs show` идёт через ssh в
удалённый `ccs` напрямую, поэтому для `--tail` на хосте достаточно передать
флаг. Хост со старым `ccs` ответит на `--tail` ошибкой clap, всё остальное с
него работает как раньше.

**Порядок работы** (принцип из https://maevyn.codes/decoherence: сначала одна
модель, потом потребители; не начинать следующий переход, пока не закончен
предыдущий): модель → идентичность → подсчёт и `list` → `show` → цель по
`session_id` → `--tail` → remote → скилл и документация.

## Technical Details

### `src/session/codex_history.rs`

```rust
/// Имя rollout-файла: rollout-<YYYY-MM-DDThh-mm-ss>-<thread>[_<rollout>].jsonl
/// Без `_<rollout>` rollout_id = thread_id (как у Codex).
pub struct RolloutName { pub timestamp: NaiveDateTime, pub thread_id: String, pub rollout_id: String }
impl RolloutName {
    pub fn parse(path: &Path) -> Option<Self>;
    /// «Новее» как у Codex: время из имени, при равенстве rollout_id (UUIDv7).
    pub fn order_key(&self) -> (NaiveDateTime, &str);
}

/// Часть пути до компонента `sessions` или `archived_sessions`.
pub fn codex_home(path: &Path) -> Option<PathBuf>;

/// Кусок истории: файл целиком или его префикс.
pub struct Segment { pub path: PathBuf, pub end_byte: Option<u64> }

/// Сегменты от корня к самому файлу. Для не-Codex и Codex без history_base —
/// один сегмент с end_byte = None.
pub fn resolve_segments(path: &Path) -> Vec<Segment>;

/// Строки истории с физическим адресом (line_number 1-based в своём файле).
pub struct HistoryLine { pub path: Arc<Path>, pub line_number: usize, pub raw: String }
pub fn history_lines(path: &Path) -> impl Iterator<Item = HistoryLine>;

/// Имена rollout-файлов одного codex_home (sessions + archived_sessions).
/// Строится одним обходом на операцию, между операциями не хранится.
pub struct RolloutIndex { /* rollout_id -> path, thread_id -> paths */ }
impl RolloutIndex {
    pub fn scan(codex_home: &Path) -> Self;
    /// Файл треда с наибольшим `RolloutName::order_key`; None — треда нет в индексе.
    pub fn current_rollout_for_thread(&self, thread_id: &str) -> Option<PathBuf>;
}
```

- `codex_home` выводится из пути файла (часть до компонента `sessions` или
  `archived_sessions`), так что `CODEX_HOME` и временные корни в тестах
  работают без отдельной настройки. Это единственный вывод `codex_home`:
  `session::codex_session_root` уходит в `codex_history::codex_home`.
- Текущий файл треда — максимум `RolloutName::order_key`, как в Codex
  (`list.rs:find_thread_path_by_id_from_filenames`). Время в имени — с
  точностью до секунды, поэтому два revert за одну секунду различает rollout
  id (UUIDv7: строки в каноническом виде сравниваются как числа). mtime не
  используется: он меняется при копировании и синхронизации, а Codex смотрит
  только на имя. Нет `codex_home` в пути или треда нет в индексе — потребитель
  оставляет файл, который у него уже есть.
- Поиск файла по rollout id: имя `…-<id>.jsonl` или `…_<id>.jsonl`, сначала
  `sessions`, потом `archived_sessions` — как у Codex.
- Глобального кеша нет. Индекс строится на одну операцию (один поиск, один
  `list`, один `show`, один проход сканера недавних; ~10 мс на 1346 файлов) и
  передаётся тем, кому нужен в рамках операции. Кеш на процесс в долгоживущем
  TUI возвращал бы старый текущий файл треда после нового revert: файл с тем же
  thread id находится, промаха нет, перестройки нет.
- `resolve_segments`/`history_lines` без переданного индекса строят его сами,
  только если у файла есть `history_base`; у остальных файлов обхода нет.
- Рекурсия по `history_base` с защитой: множество посещённых rollout id
  (начинается с rollout id самого файла) и предел глубины (64). Последний
  сегмент — переданный путь как есть, без нормализации.
- Пустой файл (0 байт) кандидатом в текущий не считается: Codex создаёт файл
  продолжения раньше, чем пишет в него `session_meta`. Базовый файл не найден, короче `end_byte_offset` или цикл
  — история обрезается до уже собранного, ошибки нет (как у Codex: «best
  effort»).
- `end_byte_offset` указывает сразу за последней включённой строкой, поэтому
  префикс читается через `Read::take(end_byte)` без разрезания строк.

### Потребители

| Что | Сейчас | После |
|---|---|---|
| `extract_codex_session_id_from_path` | `thread_rollout` | `RolloutName::parse(..).thread_id` |
| `ripgrep::MatchProcessor` | у хита только физический `file_path` | новое поле хита `session_file` — текущий rollout треда; рядом с `resolve_to_parent`, в потоке поиска; `RolloutIndex` один на `codex_home` за поиск, кеш по файлу хита |
| `group_by_session` | `file_path` первого хита | `session_file` первого хита, без него `file_path`; функция остаётся чистой, без IO |
| `find_session_file_in_paths` (Codex) | первый файл обхода с thread id в имени | самый новый из найденных по `RolloutName::order_key` |
| `find_codex_rollout_near` | первый файл обхода, свой поиск `sessions`/`archived_sessions` | `codex_home` + `RolloutIndex::current_rollout_for_thread`; без `codex_home` — самый новый в каталоге файла |
| `count_session_messages` | свой файл | `history_lines` |
| `cli::extract_session_metadata` | свой файл | `session_id` из своего `session_meta`, время и счёт по `history_lines` |
| `recent.rs` | свой файл | заголовок — свой, при его отсутствии первый промпт истории; `message_count` уже идёт через `count_session_messages` |
| `cli::locate_jsonl_message` | свой файл | `history_lines`; якорь `--line` ищется среди строк с `path == file_path` |

### `ccs show`

```
ccs show <TARGET> [--line N | --uuid U | --tail N] [--context K] [--max-chars M] [--host H]
```

- `TARGET` — путь к файлу или `session_id`. Разрешение функцией
  `resolve_show_target(search_paths, target)`: существующий путь или
  синтетический путь Opencode (`<db>#<id>`) берётся как есть. Иначе —
  существующий `session::find_session_file_in_paths` (Claude, Desktop
  `audit.jsonl`, Opencode, Codex); для Codex он уже возвращает текущий rollout
  треда (см. «Потребители»). Ничего не найдено — ошибка
  `no session <id> in search paths`.
- Без `--line`/`--uuid` работает `--tail` (по умолчанию 10). `--tail`
  конфликтует с `--line`, `--uuid` и `--context` (clap `conflicts_with`).
- Каждая строка вывода получает `file_path` своего файла (как строки
  `ccs search`); строки, где он отличается от summary, — унаследованная история. Summary получает `mode: "tail"` и
  `total_messages` для режима tail; `target_line`/`target_uuid` там `null`.
- `session_id` и `file_path` в summary берутся от файла цели: `session_id` из
  его `session_meta.payload.id`, запасной вариант `RolloutName::thread_id`.
  Из сообщений их не берут: хвост форка может целиком лежать в файле
  родителя, и тогда у сообщений thread id родителя. Правило то же, что у
  `list` в Task 3.

### Remote

- `show_remote` передаёт `TARGET` как есть (удалённый `ccs` сам разрешит
  `session_id`), а флаги — по режиму: `--line`/`--uuid` с `--context`, либо
  `--tail` без `--context`. `--max-chars` — в обоих режимах.
- `remote/wire.rs`, `remote/server.rs`, `remote/serve.rs` не меняются,
  `PROTOCOL_VERSION` остаётся 1. Превью в TUI по-прежнему берёт одно сообщение
  по физическому адресу.

## What Goes Where

- **Implementation Steps**: код, тесты, скилл, README, CLAUDE.md в этом репо.
- **Post-Completion**: релиз, обновление `ccs` на Mac и Z270XX, ручная проверка
  на живых сессиях.

## Implementation Steps

### Task 1: Модель истории Codex в `session/codex_history.rs`

**Files:**
- Create: `src/session/codex_history.rs`
- Modify: `src/session/mod.rs` (подключить модуль)
- Create: тестовый хелпер фикстур (место — см. Testing Strategy)

- [ ] тестовый хелпер: пишет root-сессию, revert-продолжение (`<thread>_<rollout>`), форк и форк от форка, считает `end_byte_offset`
- [ ] тесты `RolloutName::parse`: обычное имя, имя с `_<rollout>`, `.jsonl` не Codex, мусор
- [ ] тесты `resolve_segments`: нет `history_base` → один сегмент; revert → 2 сегмента; цепочка из 3 форков → 4 сегмента от корня; базовый файл в `archived_sessions`
- [ ] тесты на отказ: базового файла нет, файл короче `end_byte_offset`, цикл ссылок → история без ошибки, обрезана до собранного
- [ ] тесты `history_lines`: строки базового файла после `end_byte` не попадают; `line_number` физический в своём файле; не-Codex файл = его строки
- [ ] тест `RolloutIndex::current_rollout_for_thread`: из двух файлов треда выбирается более новый по времени в имени; два revert за одну секунду → больший rollout id; файл со старым именем и свежим mtime не выбирается; треда нет в индексе → None
- [ ] тест `codex_home`: путь под `sessions` и под `archived_sessions` → общий корень; путь без этих компонентов → None
- [ ] тест: индекс, построенный после появления нового revert-файла того же треда в том же процессе, возвращает новый файл (глобального кеша нет)
- [ ] реализовать `RolloutName`, `codex_home`, `RolloutIndex`, `resolve_segments`, `history_lines`
- [ ] run tests - must pass before next task

### Task 2: Идентичность Codex-сессии в поиске и resume

**Files:**
- Modify: `src/session/mod.rs` (`extract_codex_session_id_from_path`, `find_session_file_in_paths`, `find_codex_rollout_near`, убрать `codex_session_root`)
- Modify: `src/search/ripgrep.rs` (`RipgrepMatch.session_file`, `MatchProcessor`)
- Modify: `src/search/group.rs` (`group_by_session`)
- Modify: `tests/cli_search.rs`

- [ ] тест: `extract_codex_session_id_from_path` для `…-<thread>_<rollout>.jsonl` возвращает `<thread>`
- [ ] тест `MatchProcessor`: хит в базовом файле revert получает `session_file` = продолжение; путь без `codex_home` → `session_file` = свой файл
- [ ] тест `group_by_session` (без файлов на диске): хиты одного `session_id` → одна группа, `file_path` группы = `session_file`; хиты без `session_file` → `file_path` первого хита, как раньше
- [ ] тест CLI: хит только в базовом файле → `file_path` группы всё равно продолжение (resume и превью открывают текущий файл)
- [ ] тест: `file_path` и `line_number` отдельных хитов остаются физическими
- [ ] тест `find_session_file_in_paths`: thread id с базовым файлом и продолжением → продолжение
- [ ] тест `find_codex_rollout_near`: у родителя сабагента есть revert → продолжение; существующие тесты `find_codex_rollout_near` зелёные
- [ ] реализовать: `RolloutName` в `extract_codex_session_id_from_path`; `session_file` в `MatchProcessor` (один `RolloutIndex` на `codex_home` за поиск, кеш по файлу хита); Codex-ветки `find_session_file_in_paths` и `find_codex_rollout_near` через `codex_history`. Аргумент `codex resume` становится thread id без отдельной правки
- [ ] run tests - must pass before next task

### Task 3: Подсчёт сообщений и `ccs list` по полной истории

**Files:**
- Modify: `src/search/group.rs` (`count_session_messages`)
- Modify: `src/cli.rs` (`extract_session_metadata`)
- Modify: `tests/cli_list.rs`

- [ ] тест: `count_session_messages` для revert-продолжения = сообщения префикса + свои; хвост базового файла после `end_byte` не считается
- [ ] тест: для форка от форка счёт включает обе унаследованные части
- [ ] тест `ccs list`: revert-пара даёт одну строку с полным счётом и `file_path` продолжения; у форка `session_id` свой, а не корня
- [ ] тест: Claude-сессии и Codex без `history_base` — счёт как раньше
- [ ] перевести оба читателя на `history_lines`; `session_id` брать из `session_meta` своего файла
- [ ] run tests - must pass before next task

### Task 4: Недавние сессии: заголовок свой, счёт полный

**Files:**
- Modify: `src/recent.rs` (`resolve_summary` и сканер начала файла)

- [ ] тест: у форка заголовок — первый промпт самого форка, `message_count` — по полной истории
- [ ] тест: revert-пара в недавних — одна строка, путь продолжения
- [ ] тест: продолжение и форк без своего промпта не пропадают из недавних, заголовок — первый промпт полной истории
- [ ] реализовать запасной заголовок: если у файла нет своего промпта, взять первый промпт из `history_lines` (без проверки на Codex: у файла без `history_base` история = сам файл)
- [ ] run tests - must pass before next task

### Task 5: `ccs show` по полной истории

**Files:**
- Modify: `src/cli.rs` (`locate_jsonl_message`, вывод строк)
- Modify: `tests/cli_show.rs`

- [ ] тест: `ccs show <продолжение> --line <первая своя строка> --context 3` показывает и строки из базового файла, у них `file_path` базового файла
- [ ] тест: `--line` ищется только среди строк запрошенного файла (номер строки базового файла не совпадает случайно)
- [ ] тест: `ccs show <базовый файл> --line N` работает как раньше
- [ ] перевести `locate_jsonl_message` на `history_lines`, добавить `file_path` в каждую строку вывода
- [ ] run tests - must pass before next task

### Task 6: Цель `ccs show` по `session_id`

**Files:**
- Modify: `src/main.rs` (аргумент `TARGET`)
- Modify: `src/cli.rs` (`resolve_show_target`)
- Modify: `tests/cli_show.rs`

- [ ] тесты `resolve_show_target`: путь как есть; Claude id; Desktop id → `audit.jsonl`; Codex thread id → текущий rollout, а не базовый файл; Opencode id; не найдено → ошибка с id
- [ ] тест CLI: `ccs show <codex thread id> --line N` находит файл продолжения
- [ ] реализовать `resolve_show_target` поверх `session::find_session_file_in_paths` (текущий rollout он уже возвращает после Task 2)
- [ ] run tests - must pass before next task

### Task 7: `ccs show --tail N`

**Files:**
- Modify: `src/main.rs` (флаг, конфликты)
- Modify: `src/cli.rs`
- Modify: `tests/cli_show.rs`

- [ ] тест: без якоря выводятся последние 10 сообщений, summary с `mode: "tail"` и `total_messages`
- [ ] тест: Claude-сессия с ветками — хвост последней цепочки, сообщения отброшенной ветки не попадают
- [ ] тест: Codex revert-продолжение с 2 своими сообщениями и `--tail 5` — 3 сообщения берутся из базового файла
- [ ] тест: форк без своих сообщений, `--tail` → в summary `session_id` = thread id форка и `file_path` = файл форка, хотя все сообщения хвоста из файла родителя
- [ ] тест: Opencode `--tail`
- [ ] тест: `--tail` вместе с `--line`/`--uuid`/`--context` — ошибка clap
- [ ] заменить тест `show_requires_line_or_uuid` (`tests/cli_show.rs`): без якоря теперь режим tail
- [ ] убрать ошибку `pass --line or --uuid` и реализовать tail; `session_id` и `file_path` summary — от файла цели
- [ ] run tests - must pass before next task

### Task 8: Remote: `ccs show --tail --host`

**Files:**
- Modify: `src/cli.rs` (`show_remote`)
- Modify: тест удалённого `show` с `CCS_SSH=tests/fixtures/fake-ssh.sh` (в том файле `tests/`, где уже проверяется `show --host`)

- [ ] тест: `ccs show <id> --tail 3 --host <h>` вызывает на хосте `ccs show <id> --tail 3 --max-chars …` без `--context`; сравнивается весь argv, не префикс
- [ ] тест: `ccs show <id> --host <h>` без якоря передаёт `--tail 10` явно (иначе удалённый ccs ответит `pass --line or --uuid`)
- [ ] тест: `ccs show <file> --line N --host <h>` передаёт `--line N --context K` как раньше, без `--tail`; весь argv
- [ ] реализовать передачу флагов по режиму; `remote/wire.rs`, `remote/server.rs`, `remote/serve.rs` не трогать
- [ ] run tests - must pass before next task

### Task 9: Verify acceptance criteria

- [ ] на копии реальной пары `01a178da…` (во временном корне): `ccs list` — одна строка с полным счётом; `ccs search` — одна группа; `ccs show 01a178da-2e05-7720-b6ba-6b4edd17fc27 --tail 10` — конец сессии
- [ ] на копии цепочки форков: счёт у каждого форка включает предков, заголовки разные
- [ ] Claude, Desktop, Opencode: вывод `list`/`show` не изменился (существующие тесты зелёные)
- [ ] run full test suite: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`

### Task 10: [Final] Update documentation

- [ ] `.claude/skills/ccs/SKILL.md` и `.agents/skills/ccs/SKILL.md`: сценарий «на чём остановилась сессия» — `ccs show <session_id> --tail 10`; `show` принимает `session_id`; строки с чужим `file_path` — унаследованная история; убрать совет читать JSONL через Read там, где теперь хватает `show`
- [ ] README: `ccs show --tail`, цель по `session_id`
- [ ] CLAUDE.md: `session/codex_history.rs` в Architecture, абзац в Key data flow про сегменты истории Codex
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

**Ручная проверка:**
- реальные `~/.codex/sessions` на Mac: пара `01a178da…`, пара `01a1506b…`,
  цепочка форков `01a0a58e-76bd…` → … → `01a08bac…`: `ccs list`, TUI
  (недавние, поиск, превью, Enter → `codex resume <thread id>` открывает
  нужный тред);
- `ccs show <id> --tail 10 --host Z270XX` после обновления `ccs` на Z270XX.

**Релиз:**
- протокол `ccs serve` не меняется, машины можно обновлять по одной; удалённый
  `--tail` заработает на хосте после обновления `ccs` на нём (до этого — ошибка
  clap `unexpected argument '--tail'`).

**Вне этого плана (известные ограничения):**
- дерево (`Ctrl+B`/`--tree`) для Codex не работает и сейчас: у записей Codex
  нет `uuid`, дерево пустое;
- хиты поиска в отброшенном хвосте базового файла revert показываются как часть
  сессии, без пометки «вне текущей ветки»;
- хиты по унаследованному тексту форка относятся к родительской сессии: текст
  физически лежит только в её файле;
- сжатые rollout-файлы `.jsonl.zst` `ccs` не видит вообще.
