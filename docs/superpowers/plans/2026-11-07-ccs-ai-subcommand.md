# `ccs ai` Subcommand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ccs ai [query]` opens the TUI already in AI search, starting the agent when a query is given.

**Architecture:** One public `App` method puts a fresh app into AI mode; `main` parses `Commands::Ai` and calls it on the app `run_tui_loop` builds, so the rest of the TUI lifecycle (hosts, overlay, return after resume) is shared with plain `ccs`.

**Tech Stack:** Rust, clap, ratatui.

**Spec:** `docs/superpowers/specs/2026-11-07-ccs-ai-subcommand-design.md`

## Global Constraints

- No change to the behaviour of plain `ccs`, `ccs pick`, `--overlay`, remote hosts.
- Test names follow given – when – then.
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test` pass.

## Review Focus

- `ccs ai "   "` (blank query): opens AI mode with an empty input, no agent with a blank query.
- `ccs --overlay ai "q"`: overlay flag still honoured.
- `claude` missing from PATH: the error shows in the AI status line, ccs does not exit.

---

### Task 1: `App::start_in_ai_mode`

**Files:**
- Modify: `src/tui/state.rs` (AI mode methods near `enter_ai_mode`; tests in the AI test block near `ai_app()`)

**Interfaces:**
- Produces: `pub fn start_in_ai_mode(&mut self, query: &str)` — enters AI mode, puts the trimmed query into `ai.query`, and submits it through `submit_ai_query` (which is a no-op for an empty query).

- [ ] Write failing tests:
  - `fresh app - start in ai mode with empty query - ai input is active and no agent runs`: `ai.active`, `ai.query.is_empty()`, `!ai.searching()`, `ai.error.is_none()`.
  - `fresh app - start in ai mode with blank query - input is empty and no agent runs`: query `"   "`, same assertions.
- [ ] Run `cargo test start_in_ai_mode` → fails (method missing).
- [ ] Implement.
- [ ] Run tests → pass. Commit.

### Task 2: `Commands::Ai` in `main`

**Files:**
- Modify: `src/main.rs` (`Commands`, `main`, `run_tui_mode`, `run_tui_loop`, tests module)
- Modify: `docs/cli.md` (new `## ccs ai` section before `## ccs pick`), `README.md` (one example line next to `ccs pick`)

**Interfaces:**
- Consumes: `App::start_in_ai_mode(&str)`.
- `Commands::Ai { query: Option<String> }` with doc "Open the TUI in AI search (Ctrl+G), optionally starting it with a query".
- `run_tui_loop(search_paths, tree_target, overlay, ai_query: Option<String>)`: `Some(q)` → `app.start_in_ai_mode(&q)` after `init_app`. `run_subcommand(cmd, overlay)` takes `cli.overlay` and routes `Commands::Ai { query }` to `run_tui_loop(get_search_paths(), None, overlay, Some(query.unwrap_or_default()))`.

- [ ] Write failing parse tests: `ccs ai - parse - ai command without query`; `ccs ai "docker build fix" - parse - query is kept`; `ccs --overlay ai q - parse - overlay set and query q`.
- [ ] Run → fails to compile. Implement. Run → pass.
- [ ] Docs lines. fmt, clippy, full `cargo test`.
- [ ] Manual check: `cargo run -- ai` opens AI input; `cargo run -- ai "docker build fix"` starts the agent. Commit.
