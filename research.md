# Research: Поддержка Codex CLI в CCS (Claude Code Session Search)

**Дата**: 2026-03-04
**Цель**: Анализ фич CCS, архитектуры OpenAI Codex CLI и план по добавлению поддержки Codex-сессий в CCS.

## Содержание

1. [Текущие фичи CCS](#1-текущие-фичи-ccs-claude-code-session-search)
2. [OpenAI Codex CLI: архитектура и фичи](#2-openai-codex-cli-архитектура-и-фичи)
3. [Маппинг фич и план поддержки Codex CLI](#3-маппинг-фич-и-план-поддержки-codex-cli)
4. [Выводы](#4-выводы)

---

## 1. Текущие фичи CCS (Claude Code Session Search)

CCS -- это инструмент для полнотекстового поиска и навигации по сессиям Claude Code (CLI и Desktop). Написан на Rust, использует TUI-интерфейс на базе ratatui. Бинарное имя: `ccs`.

### 1.1 Полнотекстовый поиск по сессиям

**Движок поиска** -- ripgrep (`rg`), вызываемый как subprocess с JSON-выводом (`--json`).

Ключевые детали реализации (`src/search/ripgrep.rs`):

- **Два режима поиска**: literal (по умолчанию, `--fixed-strings`) и regex (переключается Ctrl+R). В literal-режиме поиск case-insensitive через `.to_lowercase()`, в regex -- через `RegexBuilder::new().case_insensitive(true)`.
- **Post-filtering**: ripgrep ищет по всей JSONL-строке (включая метаданные), но CCS дополнительно проверяет, что запрос найден именно в `msg.content`, отфильтровывая false positives из sessionId, path и др. метаданных.
- **Debounce**: 300ms задержка после последнего нажатия клавиши перед запуском поиска (`DEBOUNCE_MS = 300` в `src/tui/app.rs`).
- **Async background search**: поиск выполняется в отдельном потоке через `mpsc::channel`. Основной UI-поток не блокируется. Результаты поздних запросов игнорируются, если query или regex_mode изменились.
- **Multi-path search**: поиск выполняется по нескольким директориям последовательно через `search_multiple_paths()`.
- **Лимит**: `--max-count 1000` на файл (ripgrep).

**Пути поиска** (`src/main.rs:get_search_paths()`):

1. Если задана переменная `CCFS_SEARCH_PATH` -- используется она.
2. Иначе автоматически:
   - CLI-сессии: `~/.claude/projects/` (JSONL-файлы по имени session_id)
   - Desktop-сессии: `~/Library/Application Support/Claude/local-agent-mode-sessions/` (macOS)
3. Fallback: `~/.claude/projects/`

**Санитизация контента** (`sanitize_content()`):
- Удаление ANSI escape-последовательностей (CSI, OSC, DEC, SS2/SS3)
- Удаление control-символов (кроме `\n` и `\t`)
- Преобразование `\r\n` в `\n`, standalone `\r` в пробел

**Контекст совпадения** (`extract_context()`):
- Извлечение N символов вокруг найденного совпадения
- UTF-8-safe (поддержка кириллицы и других многобайтовых символов)
- Case-insensitive поиск позиции

### 1.2 Парсинг JSONL-сессий

**Dual-format support** (`src/search/message.rs`):

| Поле         | Claude Code CLI             | Claude Desktop                |
|-------------|----------------------------|-------------------------------|
| session ID  | `sessionId`                 | `session_id`                  |
| timestamp   | `timestamp`                 | `_audit_timestamp`            |
| branch      | `branch` / `gitBranch`      | отсутствует                   |
| uuid        | `uuid`                      | `uuid`                        |
| parentUuid  | `parentUuid`                | `parentUuid`                  |

**Извлечение контента** (`Message::extract_content()`):
- Поддержка plain string (`"content": "text"`) и массива блоков (`"content": [...]`)
- Из массива извлекаются:
  - `text` блоки -- текстовое содержимое
  - `tool_use` блоки -- JSON-строка из поля `input` (для поиска по аргументам инструментов)
  - `tool_result` блоки -- текст или JSON результата
- Блоки объединяются через `\n`

**Фильтрация**: пропускаются строки с type != "user"/"assistant" (например, "summary", "progress", "system").

**Группировка** (`src/search/group.rs`):
- Результаты группируются по `session_id` в `SessionGroup`
- Группы сортируются по новизне (newest first)
- Внутри группы совпадения также отсортированы newest first

**Извлечение имени проекта** (`extract_project_from_path()`):
- CLI: `/.../.claude/projects/-Users-user-projects-myapp/session.jsonl` -> `myapp` (через rfind `-projects-`)
- Desktop nested: `/.../-sessions-wizardly-vibrant-dirac/session.jsonl` -> `wizardly-vibrant-dirac`
- Desktop audit: `.../local_40338476-.../audit.jsonl` -> `Desktop:40338476`
- Desktop metadata: чтение `title` из сиблинг-файла `local_xxx.json`

**SessionSource** enum: `ClaudeCodeCLI` / `ClaudeDesktop`, определяется по наличию `local-agent-mode-sessions` в пути файла.

### 1.3 Визуализация дерева сессии (DAG)

Модуль `src/tree/mod.rs` реализует парсинг и визуализацию структуры сессии как DAG (directed acyclic graph) с ветвлениями.

**Парсинг DAG** (`SessionTree::from_file()`):
- Читает все строки JSONL, извлекая `uuid`, `parentUuid`, `type`, `timestamp`
- Строит `HashMap<uuid, DagNode>` и `HashMap<parent_uuid, Vec<child_uuid>>`
- Поддерживает типы: `user`, `assistant` (displayable), `summary` (compaction), `progress`/`system` (intermediate, скрытые)

**Display graph** -- отдельный граф из displayable-узлов:
- Промежуточные узлы (progress, system) коллапсируются: их потомки подключаются к ближайшему displayable-предку
- Реализовано через `find_displayable_parent()` с кэшированием
- Сортировка children по `line_index` для стабильного порядка

**Latest chain** -- цепочка от последнего uuid в файле до корня через parentUuid:
- Строится функцией `build_latest_chain()`
- Используется для визуального выделения текущей ветки
- Latest-chain branch отображается первой при DFS-обходе

**Compaction events** -- события автокомпакции контекста:
- Записи type="summary" с uuid показываются как специальные узлы (role="compaction")
- Детекция по содержимому: "being continued from a previous conversation that ran out of context", "/compact"

**ASCII art visualization** (`build_graph_symbols()`):
- Multi-column layout с `active_columns` tracking
- Символы: `*` для текущего узла, `|` для активных parallel branches
- Ветки выделяются цветом: yellow для latest chain, dark gray для abandoned branches

**Branch points** -- узлы с >1 child в display graph. Навигация Left/Right прыгает между branch points.

### 1.4 Fork-aware Resume

Модуль `src/resume/mod.rs` реализует возобновление сессий с поддержкой ветвлений.

**Определение необходимости fork**:
- Если выбранное сообщение (по uuid) не находится на latest chain -- создается fork
- `is_on_latest_chain()` строит цепочку от последнего uuid и проверяет принадлежность
- Fork поддерживается только для CLI-сессий

**Создание fork** (`create_fork()`):
1. Строит `uuid_to_parent` map из всех записей
2. Проходит от target_uuid назад до корня, собирая `branch_uuids`
3. Фильтрует строки JSONL: включает только строки с uuid из branch_uuids + строки без uuid (metadata)
4. Генерирует новый session_id (UUID v4)
5. Заменяет sessionId/session_id в каждой включаемой строке
6. Записывает новый JSONL файл в ту же директорию

**Декодирование project path** (`decode_project_path()`):
- Strategy 1: **Filesystem walking** (`walk_fs_for_path()`). Рекурсивно обходит файловую систему от `/`, энкодируя каждое имя директории по правилам Claude CLI и сравнивая с целевым encoded-именем.
- Strategy 2: Маркер `-projects-` -- наивное декодирование через `rfind("-projects-")`
- Strategy 3: Полная замена `-` на `/`, `--` на `/.` (hidden directories)

**Resume execution**:
- CLI: `exec` (замена процесса) вызов `claude --resume <session_id>` из правильной project directory
- Desktop: `exec` вызов `open -a Claude` (macOS)

### 1.5 TUI (Terminal User Interface)

Построен на **ratatui** + **crossterm** (`src/tui/ui.rs`, `src/tui/app.rs`).

**Layout** (search mode):
- Header: "Claude Code Session Search"
- Input field: с индикацией regex mode ("[Regex]") и cursor position
- Status bar: количество результатов, группы, статус поиска ("Searching...", "Typing...")
- Result list: сгруппированные результаты с expand/collapse
- Help bar: keyboard shortcuts

**Layout** (tree mode):
- Header: session info (session_id, source, branch count, message count)
- Tree list: rows с graph symbols, role badge, timestamp, content preview
- Help bar: tree-specific shortcuts

**Keyboard shortcuts** (search mode):

| Клавиша | Действие |
|---------|----------|
| Esc | Выход |
| Ctrl+C | Очистка ввода / выход (если пусто) |
| Ctrl+R | Toggle regex mode |
| Ctrl+B | Вход в tree mode |
| Up/Down | Навигация по группам/совпадениям |
| Left/Right | Collapse/expand группы |
| Tab | Toggle preview mode |
| Enter | Resume выбранной сессии |
| Alt+Left/Right, Alt+b/f | Word-level cursor movement |
| Alt+Backspace, Ctrl+W | Delete word left |
| Alt+d | Delete word right |
| Ctrl+A/E, Home/End | Начало/конец строки |

**Keyboard shortcuts** (tree mode):

| Клавиша | Действие |
|---------|----------|
| Esc / b | Выход из tree mode |
| Up/Down | Навигация по узлам |
| Left/Right | Прыжок к prev/next branch point |
| Tab | Toggle preview |
| Enter | Resume от выбранного узла (с fork если нужно) |
| q | Полный выход |

### 1.6 CLI Mode

Команда `ccs` поддерживает CLI-подкоманды (`src/cli.rs`, `src/main.rs`):

**`ccs search <query> [--regex] [--limit N]`**:
- Выполняет поиск и выводит результаты как JSONL на stdout
- Каждая строка: `{"session_id", "project", "source", "file_path", "timestamp", "role", "content"}`
- По умолчанию limit=100

**`ccs list [--limit N]`**:
- Перечисляет все сессии с метаданными
- Вывод JSONL: `{"session_id", "project", "source", "file_path", "last_active", "message_count"}`
- Сортировка по last_active descending

**`ccs --tree <path|session_id>`**:
- Запуск TUI сразу в tree mode для конкретной сессии

**Без аргументов**: запуск интерактивного TUI в search mode.

**Интеграция с Claude Code skills**: CLI-output в JSONL позволяет Claude Code использовать CCS как инструмент для поиска по истории.

---

## 2. OpenAI Codex CLI: архитектура и фичи

### 2.1 Общий обзор

OpenAI Codex CLI -- open-source (Apache-2.0) агент для разработки, работающий в терминале. Изначально написан на TypeScript/React/Node.js, в 2025 году переписан на Rust (workspace `codex-rs`, ~65 crate'ов). На момент марта 2026 -- 63k+ stars на GitHub, ~386 contributors.

Ключевые характеристики:
- **Модель по умолчанию**: `gpt-5.3-codex` (для ChatGPT Pro -- `gpt-5.3-codex-spark`)
- **Текущая версия**: 0.107.0 (2026-03-02)
- **Установка**: npm (`npm install -g @openai/codex`), Homebrew cask, pre-built бинарники
- **Поддержка ОС**: macOS, Linux, Windows (через WSL/нативно с ограничениями)
- **Репозиторий**: [github.com/openai/codex](https://github.com/openai/codex)

### 2.2 Трёхуровневая архитектура

Codex CLI построен на строгой трёхуровневой архитектуре с разделением протокола, бизнес-логики и UI.

#### Protocol layer (`codex-protocol` crate)

Определяет контракт взаимодействия между UI и Core через два базовых типа:

- **`Op` (Operations)** -- действия, инициированные пользователем:
  - `UserTurn`, `Interrupt`, `ExecApproval`, `ReviewRequest`, `Compact`, `Undo`, `UserShellCommand`

- **`EventMsg`** -- события от агента:
  - `TurnStarted` / `TurnEnded`, `AgentMessage`, `ExecCommandBegin` / `ExecOutputDelta` / `ExecCommandEnd`, `ApprovalRequest`, `TurnDiffEvent`

#### Core layer (`codex-core` crate)

- **Session** -- управление сессией (conversation_id, SessionState, ContextManager)
- **ModelClient** -- общение с API (WebSocket / HTTP, fallback на SSE)
- **ToolRouter** -- диспетчеризация выполнения инструментов
- **ContextManager** -- управление историей разговора и учёт токенов
- **RolloutRecorder** -- персистентная запись в JSONL

#### UI layer (несколько crate'ов)

| Crate | Назначение |
|-------|-----------|
| `codex-tui` | Интерактивный TUI на базе ratatui |
| `codex-exec` | Headless режим для CI/CD |
| `codex-app-server` | JSON-RPC 2.0 сервер для IDE (VS Code, Cursor, Windsurf) |
| `codex-mcp-server` | MCP-сервер -- Codex как инструмент для других агентов |
| `codex-cli` | Мультитул, объединяющий все через субкоманды |

#### Queue-based коммуникация

```
Client --> codex.submit(Op) --> submission_loop
                                    |
                               Codex instance
                                    |
Client <-- codex.next_event() <-- Event stream
```

### 2.3 Управление сессиями

#### Формат хранения

Каждая сессия сохраняется как JSONL rollout-файл.

**Путь**: `~/.codex/sessions/YYYY/MM/DD/rollout-YYYY-MM-DDTHH-MM-SS-<id>.jsonl`

Сессии идентифицируются по UUID (ThreadId). `RolloutRecorder` записывает иммутабельные события по мере их появления.

#### Resume (возобновление)

```bash
codex resume          # Picker с недавними сессиями
codex resume --last   # Последняя сессия из текущей директории
codex resume --all    # Последняя сессия без фильтра по директории
codex resume <ID>     # Конкретная сессия по UUID
```

Также: `codex exec resume --last` и `/resume` внутри TUI.

#### Fork (ветвление)

```bash
codex fork          # Picker для выбора сессии
codex fork --last   # Fork последней сессии
```

Fork создаёт новый thread на основе истории. Также через `/fork` в TUI.

#### Compact (компактификация)

`/compact` суммаризирует историю для освобождения токенов. Автоматическая компактификация при приближении к лимиту контекстного окна.

### 2.4 Режимы работы

#### Уровни подтверждения

| Режим | Описание |
|-------|----------|
| **Auto** (default) | Чтение, редактирование, выполнение в пределах рабочей директории |
| **Read-only** | Консультативный режим, без изменений |
| **Full Access** | Полный доступ без подтверждений |

#### OS-enforced песочница

| Платформа | Технология |
|-----------|-----------|
| **macOS** | Seatbelt (`sandbox-exec`) |
| **Linux** | Landlock (kernel 5.13+) + seccomp-BPF |
| **Windows** | AppContainer |

#### Non-interactive режим (`codex exec`)

```bash
codex exec "задача"              # Базовый запуск
codex exec --json "задача"       # JSONL-вывод событий
codex exec --full-auto "задача"  # С auto-approvals
codex exec --ephemeral "задача"  # Без сохранения rollout-файла
```

#### Cloud режим (`codex cloud`)

Выполняет задачи в облачном sandbox OpenAI с предзагруженным репозиторием.

### 2.5 Интеграции

- **MCP-клиент**: подключение внешних MCP-серверов (STDIO, Streamable HTTP)
- **MCP-сервер** (`codex mcp-server`): два инструмента `codex()` и `codex-reply()`
- **Multi-agent** (экспериментальный): конфигурация ролей в `config.toml`, thread forking в sub-agents
- **IDE**: JSON-RPC app-server для VS Code, Cursor, Windsurf
- **GitHub Action**: `openai/codex-action`
- **AGENTS.md**: аналог `CLAUDE.md` для настройки поведения агента

### 2.6 Slash-команды

| Команда | Описание |
|---------|----------|
| `/model` | Переключение модели |
| `/plan` | Plan mode |
| `/new` | Новый разговор |
| `/resume` | Возобновление сессии |
| `/fork` | Клонирование разговора |
| `/compact` | Суммаризация для экономии токенов |
| `/permissions` | Настройка подтверждений |
| `/review` | Анализ изменений |
| `/diff` | Git-изменения |
| `/mention` | Прикрепление файлов |
| `/agent` | Переключение sub-agent threads |
| `/mcp` | Список MCP-серверов |
| `/ps` | Фоновые терминалы |
| `/status` | Модель, policy, токены |

---

## 3. Маппинг фич и план поддержки Codex CLI

### 3.1 Сравнение форматов хранения сессий

| Аспект | Claude Code (CLI) | Claude Desktop | Codex CLI |
|--------|-------------------|----------------|-----------|
| Путь хранения | `~/.claude/projects/<encoded-path>/<session-id>.jsonl` | `~/Library/Application Support/Claude/local-agent-mode-sessions/...` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` |
| ID сессии | Поле `sessionId` в каждой строке | Поле `session_id` в каждой строке | `ThreadId` (UUID v7, `thr_*`); `session_index.jsonl` |
| Timestamp | `timestamp` (ISO 8601) | `_audit_timestamp` (ISO 8601) | Unix epoch seconds (integer) |
| Branch/Fork | `uuid`/`parentUuid` DAG внутри файла | Нет branching; линейная | Fork = новый файл с `forked_from_id` |
| Группировка | По проекту (encoded path) | По session UUID | По дате (YYYY/MM/DD); CWD в `SessionMeta` |
| Формат сообщений | Flat JSON: `{"type":"user", "message":{...}}` | Аналогично CLI | Envelope: `{"type":"EventMsg", "payload":{...}}` |
| Индекс сессий | Нет | Нет | `session_index.jsonl` |

### 3.2 Детальный разбор формата Codex JSONL

#### Структура rollout-файла

Каждая строка обернута в envelope `RolloutLine`:

```json
{"type": "<variant>", "payload": {...}}
```

**Первая строка** -- всегда `SessionMeta`:
```json
{"type": "SessionMeta", "payload": {"cwd": "/path/to/project", "model": "gpt-5.3-codex", "base_instructions": "..."}}
```

**Последующие строки** -- `RolloutItem`:
1. **`ResponseItem`** -- ответы модели (assistant messages, tool calls, результаты)
2. **`EventMsg`** -- события сессии:
   - `TurnStarted` / `TurnEnded`
   - `AgentMessage` / `UserMessage`
   - `ExecCommandBegin` / `ExecOutputDelta` / `ExecCommandEnd`
   - `ApprovalRequest`

#### Ключевые отличия от Claude Code

1. **Envelope pattern**: Codex оборачивает в `{"type", "payload"}`, Claude хранит flat JSON
2. **Нет `uuid`/`parentUuid`**: история строго линейная (append-only)
3. **Fork = новый файл**: копирование истории в новый thread, а не фильтрация DAG
4. **Timestamp**: Unix epoch (integer) vs ISO 8601 (string)
5. **Метаданные**: CWD, model в первой строке, а не в каждом сообщении

### 3.3 Фичи CCS: что нужно адаптировать для Codex

#### 1. Полнотекстовый поиск

- Ripgrep-поиск работает "из коробки" -- `*.jsonl` glob захватит и Codex-файлы
- **Проблема**: `Message::from_jsonl()` ожидает flat JSON, а Codex использует envelope
- **Решение**: расширить `from_jsonl()` с детекцией формата по наличию поля `payload`
- Нужна адаптация `extract_content()` для Codex response items

#### 2. Парсинг сообщений

- Новый парсер для envelope-формата: извлечь `payload`, определить тип
- `session_id`: из имени файла или `SessionMeta`
- `timestamp`: конвертация из Unix epoch
- `uuid`/`parent_uuid`: **отсутствуют** -- Codex линеен
- `role`: `UserMessage` -> "user", `AgentMessage` -> "assistant"

**Рекомендуемый подход**: Strategy pattern

```rust
pub fn from_jsonl(line: &str, line_number: usize) -> Option<Self> {
    let json: serde_json::Value = serde_json::from_str(line).ok()?;

    // Codex envelope detection
    if json.get("payload").is_some() {
        return Self::from_codex_jsonl(&json, line_number);
    }

    // Existing Claude Code / Desktop parsing...
}
```

#### 3. Группировка по сессиям

- Codex организован по **дате**, не по проекту
- Project name нужно извлекать из `SessionMeta.cwd` (первая строка файла)
- **Оптимизация**: кешировать `session_index.jsonl` при старте

#### 4. Дерево сессии (DAG)

- Codex не содержит `uuid`/`parentUuid` -- дерево всегда **линейное**
- Генерировать synthetic UUIDs для совместимости с `SessionTree`
- Inter-file fork graph через `forked_from_id` в `SessionMeta` (advanced)

#### 5. Fork-aware Resume

- `codex resume <SESSION_ID>` и `codex fork <SESSION_ID>` -- встроенные команды
- CWD из `SessionMeta`, `--all` flag для сессий из другой директории
- **Упрощение**: не нужно создавать fork-файлы вручную

#### 6. Project path

- Codex хранит CWD напрямую в `SessionMeta` -- reverse-encoding не нужен

#### 7. Desktop support

- Не применимо -- у Codex нет desktop-приложения

### 3.4 Конкретный план реализации

#### Фаза 1: Базовый парсинг и поиск (MVP)

**1.1 Расширение `SessionSource` enum**:
```rust
pub enum SessionSource {
    ClaudeCodeCLI,
    ClaudeDesktop,
    CodexCLI,  // NEW
}
```

**1.2 Парсинг Codex JSONL в `message.rs`**:
- Определить формат: если есть `"payload"` на верхнем уровне -> Codex
- Извлечь payload, определить тип (`UserMessage`/`AgentMessage`)
- Сконвертировать в общий `Message` struct

**1.3 Добавление Codex search paths**:
- `~/.codex/sessions/` к списку путей в `search_multiple_paths()`

**1.4 Адаптация `extract_project_from_path()`**:
- Для Codex: дата из пути + CWD из first-line

**Сложность**: Средняя

#### Фаза 2: Resume и Fork для Codex

**2.1** Поиск бинарника: `which::which("codex")`

**2.2** Resume:
```rust
fn resume_codex(session_id: &str, cwd: &str) -> Result<(), String> {
    let codex_path = which::which("codex")?;
    Command::new(&codex_path)
        .current_dir(cwd)
        .args(["resume", session_id])
        .exec();
}
```

**2.3** Fork: делегировать `codex fork <SESSION_ID>`

**Сложность**: Низкая

#### Фаза 3: Визуализация

**3.1** Линейный tree view с synthetic UUIDs
**3.2** Inter-file fork graph через `forked_from_id` (advanced)

**Сложность**: Средняя / Высокая

#### Фаза 4: Оптимизация и UX

- Кеширование `session_index.jsonl`
- Mixed results из Claude и Codex в одном списке с source indicators
- Фильтрация по source: `--source codex|claude|all`

### 3.5 Архитектура

```
                    +------------------+
                    |    ripgrep       |  (общий для всех форматов)
                    |    search        |
                    +--------+---------+
                             |
                    +--------+---------+
                    | parse_ripgrep    |  (общий парсер rg JSON output)
                    | _json()          |
                    +--------+---------+
                             |
              +--------------+--------------+
              v              v              v
     +------------+  +------------+  +------------+
     | Claude CLI |  |  Desktop   |  |  Codex CLI |
     |  parser    |  |   parser   |  |   parser   |
     +------------+  +------------+  +------------+
              |              |              |
              +--------------+--------------+
                             v
                    +------------------+
                    |    Message       |  (единая структура)
                    |    struct        |
                    +------------------+
```

### 3.6 Gap-анализ

| Фича Codex | Поддержка в CCS | Комментарий |
|------------|-----------------|-------------|
| Полнотекстовый поиск | Требует парсер | Envelope-формат нужно распаковывать |
| Resume по ID | Просто | `codex resume <id>` |
| Fork | Просто | `codex fork <id>` -- встроенная |
| DAG/branching внутри файла | Не применимо | Codex линеен |
| Inter-session fork graph | Сложно | Парсить `forked_from_id` |
| Phase annotation | Не реализовано | `commentary`/`final_answer` -- GPT-5.3 специфика |
| Tool execution logs | Частично | `ExecCommand*` детальнее, чем Claude `tool_use` |
| Token usage | Не реализовано | Codex хранит; CCS не отображает |
| Sub-agent sessions | Сложно | `SessionSource::SubAgent` с nesting |
| Archive/unarchive | Не реализовано | `thread/archive` перемещает файлы |
| `session_index.jsonl` | Нужна поддержка | Быстрый listing без чтения файлов |

### 3.7 Приоритизация

**P0 (must-have для первого релиза)**:
1. `SessionSource::CodexCLI` enum variant
2. Парсер Codex envelope JSONL -> `Message`
3. Добавление `~/.codex/sessions/` к search paths
4. `codex resume` integration

**P1 (следующая итерация)**:
5. Tree view для линейных Codex-сессий
6. Project name из SessionMeta CWD
7. `codex fork` integration
8. Source фильтрация в TUI

**P2 (nice-to-have)**:
9. Inter-session fork graph
10. `session_index.jsonl` кеширование
11. Phase annotations
12. Sub-agent session support
13. Archived sessions support

---

## 4. Выводы

### Ключевые находки

1. **Архитектуры совместимы**: CCS уже поддерживает два формата (CLI/Desktop) через `SessionSource` enum. Добавление третьего (`CodexCLI`) следует тому же паттерну.

2. **Основная сложность -- парсер**: Codex использует envelope-формат (`{"type", "payload"}`), требующий нового парсера в `message.rs`. Это ~200 строк кода с тестами.

3. **Упрощения для Codex**:
   - Нет DAG (линейные сессии) -- tree view проще
   - Нет path encoding -- CWD хранится напрямую
   - Fork/resume -- встроенные команды Codex CLI

4. **Усложнения для Codex**:
   - Группировка по дате, а не по проекту -- нужен `session_index.jsonl`
   - Timestamp в Unix epoch (не ISO 8601)
   - Session ID не в каждой строке -- нужен контекст из SessionMeta

5. **Объём работы**: MVP (фазы 1-2) оценивается как 1-2 дня работы. Основные изменения в 3 файлах: `message.rs`, `ripgrep.rs`, `resume/mod.rs`.

### Рекомендуемый следующий шаг

Начать с Фазы 1: парсер Codex JSONL + search paths. Это даёт видимый результат (Codex-сессии появляются в поиске) с минимальными изменениями в архитектуре.
