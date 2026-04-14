# Fix preview line + content block parsing

## Overview
- Preview lines in search results show raw JSON/base64 from tool_result blocks instead of readable text
- Root cause: `parse_content_blocks` in record.rs doesn't handle array content in tool_result and silently drops new content block types (image, document, connector_text, etc.)

## Context (from discovery)
- `src/session/record.rs:269-274` — `tool_result.content` can be an array with nested image/document blocks; current code does `serde_json::to_string(c)` producing huge base64 JSON strings
- `src/session/record.rs:286` — `_ => {}` silently drops image, document, connector_text, redacted_thinking, server_tool_use block types
- `src/search/message.rs:44` — `Message::from_jsonl` uses `ContentMode::Full` which includes raw JSON in msg.content
- `src/tui/render_search.rs:525` — preview line uses `msg.content` (Full mode) directly → shows garbage
- `ContentMode::TextOnly` already exists at record.rs:307 and renders only Text blocks
- 23 test sites construct `Message { ... }` literals directly in group.rs, render_search.rs, state.rs, search_mode.rs

## Development Approach
- **Testing approach**: Regular — fix parsing + add text_content field + update tests
- Complete each task fully before moving to the next

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Fix parse_content_blocks in record.rs
- [x] In `src/session/record.rs` function `parse_content_blocks` (line 269-279), replace the `tool_result` arm: instead of `serde_json::to_string(c)` for non-string content, iterate the array extracting text blocks and using `[image]`/`[document]` placeholders for binary content
- [x] In the same `match item_type` block (before `_ => {}` at line 286), add cases for: `image` → `ContentBlock::ToolResult("[image]".to_string())`, `document` → `ContentBlock::ToolResult("[document]".to_string())`, `redacted_thinking` → `ContentBlock::Thinking("[redacted]".to_string())`, `server_tool_use` → `ContentBlock::ToolUse { name, input: String::new() }`, `connector_text` → `ContentBlock::Text(text)`
- [x] Verify: `cargo build` succeeds, `cargo test` passes

### Task 2: Add text_content field to Message
- [ ] In `src/search/message.rs`, add `Default` to derive macro on Message struct (line 5): `#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]`
- [ ] Add field `pub text_content: String` to Message struct after `content` field
- [ ] In `from_jsonl` method (line 44), compute `let text_content = SessionRecord::render_content(&content_blocks, &ContentMode::TextOnly);` and add it to the Message constructor
- [ ] Update 23 test literals to add `..Default::default()` at the end of each Message struct literal in: `src/search/group.rs` (lines 129, 304, 385, 430), `src/tui/render_search.rs` (lines 1050, 1261, 1302, 1359, 1371, 1480, 1571, 1583, 1678, 1689, 1753, 1789, 1988, 2032, 2077), `src/tui/state.rs` (lines 1419, 1463, 1508), `src/tui/search_mode.rs` (line 616)
- [ ] Verify: `cargo build` succeeds, `cargo test` passes

### Task 3: Fix preview rendering in render_search.rs
- [ ] In `src/tui/render_search.rs` (lines 520-536), replace the preview block: instead of `group.first_match()` + `msg.content`, iterate `group.matches` to find first match with non-empty `msg.text_content`, then use `sanitize_content(&msg.text_content)` for display
- [ ] Verify: `cargo build` succeeds, `cargo test` passes, `cargo clippy --all-targets --all-features -- -D warnings` passes

### Task 4: Commit and push
- [ ] Commit changes with descriptive message
- [ ] Push to current branch

## Post-Completion
- Run `cargo run -- search "as@"` and verify preview shows readable text instead of JSON
- Run `cargo run` and browse sessions to verify tree mode and search still work correctly
