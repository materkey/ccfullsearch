# Research: Claude Code source -> ccfullsearch improvements

**Date**: 2026-04-02
**Source**: ~/projects/claude-code/src (parallel agent research)

---

## P0 — Correctness bugs (ccfullsearch делает неправильно)

### 1. Leaf finding сломан
**Сейчас**: ccfullsearch берёт `last_uuid` (последний uuid в файле) как tip.
**Правильно**: Claude Code находит terminal messages (uuid не в множестве parentUuids), затем для каждого идёт назад к ближайшему user/assistant — это leaf.
- Последняя строка может быть metadata record, attribution-snapshot или sidechain.
- С flag `tengu_pebble_leaf_prune`: leaf только если нет user/assistant children.
- **Файл**: `sessionStorage.ts:3720-3786`

### 2. Нет `logicalParentUuid`
compact_boundary имеет `parentUuid: null`, но `logicalParentUuid` сохраняет логическую связь. ccfullsearch не читает это поле — дерево разрывается на точках компакции.
- **Файл**: `sessionStorage.ts:993-1083` (insertMessageChain)

### 3. Нет recovery параллельных tool results
Когда Claude выполняет параллельные tools, каждый получает отдельный assistant message с одинаковым `message.id` но разными uuid. Только одна ветка отслеживается через parentUuid. Claude Code восстанавливает orphaned siblings через `recoverOrphanedParallelToolResults`.
- **Файл**: `sessionStorage.ts:2118-2206`

### 4. Нет legacy progress bridge
Старые транскрипты содержат `progress` type записи в parentUuid цепочке. Claude Code мостит через них; ccfullsearch ломает цепочку на таких записях.
- **Файл**: `sessionStorage.ts:3472-3813` (loadTranscriptFile — progress bridge logic)

### 5. Tree view не учитывает system/attachment записи
ccfullsearch обрабатывает только `user`, `assistant`, `summary` в дереве. Пропускает:
- `system` записи (включая `compact_boundary`, `microcompact_boundary`) — участники DAG
- `attachment` записи — тоже участники DAG
Это ломает chain walk.

---

## P1 — Высокоценные фичи

### 6. Metadata extraction из хвоста JSONL
Claude Code читает только first+last 64KB (`LITE_READ_BUF_SIZE = 65536`) для быстрого извлечения метаданных. ccfullsearch парсит весь файл.

**Метаданные в хвосте файла** (отдельные записи без uuid):

| type | поле | описание |
|------|------|----------|
| `custom-title` | `customTitle` | Пользовательское название сессии |
| `ai-title` | `aiTitle` | AI-сгенерированное название |
| `last-prompt` | `lastPrompt` | Последний промпт (лучше first user msg для resumed) |
| `summary` | `summary` + `leafUuid` | AI-саммари разговора |
| `tag` | `tag` | Пользовательский тег (для поиска/фильтрации) |
| `pr-link` | `prNumber`, `prUrl`, `prRepository` | Привязанный GitHub PR |
| `task-summary` | `summary` | Что агент делает сейчас |
| `agent-name` | `agentName` | Кастомное имя агента |
| `agent-color` | `agentColor` | Цвет агента |
| `agent-setting` | `agentSetting` | Определение агента |
| `mode` | `mode` | `coordinator` / `normal` |

**Приоритет заголовка сессии** (как у Claude Code): `agentName > customTitle > summary > firstPrompt`

### 7. Subagent transcript discovery
Агентские транскрипты хранятся в:
- `~/.claude/projects/<project>/<sessionId>/subagents/agent-<agentId>.jsonl`
- Старый формат: `~/.claude/projects/<project>/agent-<agentId>.jsonl`
- Sidecar: `agent-<agentId>.meta.json` с `{agentType, worktreePath?, description?}`

ccfullsearch пропускает `agent-*` файлы — стоит индексировать и показывать связь parent->agent.

### 8. Thinking block search
Thinking блоки хранятся inline в `message.content[]`:
```json
{"type": "thinking", "thinking": "reasoning text...", "signature": "..."}
```
Всегда сохраняются в JSONL, никогда не фильтруются. Содержат rationale/reasoning — ценны для поиска.

### 9. Resume CLI flags
ccfullsearch запускает `claude --resume <session-id>`. Доступны ещё:
- `--continue` — последняя сессия в текущей директории
- `--fork-session` — создать новый session ID при resume (вместо нашего fork.rs!)
- `--from-pr [value]` — resume по PR номеру
- `--rewind-files <user-message-id>` — откатить файлы до состояния на момент сообщения
- `--session-id <uuid>` — использовать конкретный ID (с --fork-session)
- Resume по custom title: если value не UUID, ищет точное совпадение по customTitle

**Важно**: `--fork-session` — встроенный fork в Claude Code! Потенциально заменяет наш fork.rs.

### 10. Content block type awareness
Все типы content blocks:
- `text` — текст сообщения
- `thinking` — reasoning (с `signature`)
- `redacted_thinking` — redacted reasoning (с `data`)
- `tool_use` — вызов инструмента (`name`, `id`, `input`, `caller`)
- `tool_result` — результат (`tool_use_id`, `content`, `is_error`)
- `image` — base64 изображение (ОГРОМНОЕ, надо пропускать при поиске)

MCP tools: имя в формате `mcp__<server>__<tool>` — парсится тривиально.

---

## P2 — Полезные улучшения

### 11. Image data skipping
base64 изображения хранятся inline и делают файлы огромными. При поиске/парсинге стоит пропускать `"type":"image"` блоки.

### 12. isMeta / isCompactSummary / isVirtual флаги
- `isMeta: true` — системные/синтетические сообщения (nudges, continuation prompts)
- `isCompactSummary: true` — саммари компакции, не реальный ввод
- `isVirtual: true` — REPL inner calls, display-only

Эти должны визуально отличаться или фильтроваться в tree view.

### 13. Worktree-aware session discovery
Claude Code сканирует git worktree пути для поиска сессий (`listSessionsImpl.ts:309-401`, `sessionStoragePortable.ts:347-380`). ccfullsearch не делает этого.

### 14. Полезные поля на каждом сообщении
- `entrypoint` — cli/sdk-ts/sdk-py (тип клиента)
- `version` — версия Claude Code
- `cwd` — рабочая директория
- `slug` — slug сессии для планов
- `origin` — `human`/`task-notification`/`coordinator`/`channel`
- `agentId` — ID агента для sidechain
- `requestId` — API request ID (на assistant)

### 15. Error/retry записи
- `api_error` — API ошибки
- `api_retry` — retry с attempt/max_retries/delay
- `rate_limit_event` — rate limiting info
- `tool_result` с `is_error: true` — ошибки инструментов

### 16. Stats cache
`~/.claude/stats-cache.json` содержит агрегированную статистику: dailyActivity, modelUsage, totalSessions, totalMessages, hourCounts и т.д.

### 17. Hook execution traces
Hooks оставляют записи: `hook_started`, `hook_progress`, `hook_response`, `stop_hook_summary`.

### 18. Plan storage
Планы хранятся как `ExitPlanMode` tool_use блоки: `{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"..."}}`.

### 19. Context collapse (marble-origami)
Обфусцированное название для context collapse:
- `marble-origami-commit` — коммиты контекстного коллапса
- `marble-origami-snapshot` — снапшоты staged состояния

### 20. Microcompaction
`microcompact_boundary` — лёгкая компакция, заменяет большие tool results стабами. Метаданные о том, какие tools были компактированы.

---

## Ключевые файлы в ~/projects/claude-code/src

| Файл | Содержание |
|------|------------|
| `types/logs.ts:297-318` | Entry union — все типы записей |
| `types/logs.ts:221-231` | TranscriptMessage fields |
| `types/logs.ts:8-17` | SerializedMessage fields |
| `utils/sessionStorage.ts:2069-2206` | buildConversationChain + parallel recovery |
| `utils/sessionStorage.ts:3472-3813` | loadTranscriptFile + progress bridge |
| `utils/sessionStorage.ts:3720-3786` | Leaf finding |
| `utils/sessionStorage.ts:993-1083` | insertMessageChain (write) |
| `utils/sessionStorage.ts:247-258` | getAgentTranscriptPath |
| `utils/sessionStoragePortable.ts:717-793` | readTranscriptForLoad (chunked) |
| `utils/sessionStoragePortable.ts:311-319` | sanitizePath |
| `utils/conversationRecovery.ts:416-440` | loadMessagesFromJsonlPath |
| `utils/conversationRecovery.ts:456-597` | loadConversationForResume |
| `utils/messages.ts:4335-4603` | System message creators |
| `utils/cost-tracker.ts:143-175` | Cost tracking (NOT in JSONL) |
| `main.tsx:988-1000` | CLI resume flags |
| `entrypoints/sdk/coreSchemas.ts:414-467` | Tool schemas |
