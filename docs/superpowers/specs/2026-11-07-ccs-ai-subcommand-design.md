# `ccs ai` — open the picker straight in AI search

## Why

On a phone the help bar is about 54 columns wide. `[Ctrl+G] AI` is shown
only from 80 columns (`src/tui/render_search.rs`), so AI search is invisible
there, and Ctrl combinations are awkward on a touch keyboard. A word typed
on the command line is easy anywhere.

## Goals

- `ccs ai` opens the TUI already in AI mode with an empty AI input.
- `ccs ai "query"` opens it in AI mode with the query filled in and the agent
  search already running.
- Everything after that behaves exactly as after Ctrl+G: Up/Down, Enter
  resumes, Right/Ctrl+B open the tree, Esc cancels a running search, Ctrl+G
  returns to the normal list, the picker comes back after a quick exit.

## Non-goals

- Mouse/tap support.
- Changing which hints show on narrow screens.
- Any change to `--overlay`, `pick` or remote hosts behaviour.

## Approaches

- **Subcommand `ccs ai [query]`** — picked. Short to type on a phone, same
  shape as `ccs pick [query]`.
- Flag `ccs --ai` — two dashes are slower on a touch keyboard; no gain.
- Clickable hints with mouse capture — more work, breaks native text
  selection on desktop, and it is unknown whether the phone client sends
  taps as mouse events.

## Design

- New `Commands::Ai { query: Option<String> }` in `src/main.rs`.
- It runs the same TUI path as plain `ccs` (same hosts, same outer loop),
  but the `App` is put into AI mode before the first draw via the existing
  `enter_ai_mode`; with a non-empty query the AI input is set and
  `submit_ai_query` starts the agent.
- Docs: a line about `ccs ai` in `docs/cli.md` / README where commands are
  listed.

## Left to the builder

- Exact App entry point for "start in AI mode" (a small public method on
  `App`), and how `main` passes it through `run_tui_loop`.
- Whether `--overlay` combines with `ccs ai` (`ccs --overlay ai`) — fine if
  it falls out naturally, not required.
