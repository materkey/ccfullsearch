# TUI guide

`ccs` opens on recent sessions. Type anything to switch into search mode.

Main keys:

- `Up` / `Down`: move through sessions or messages.
- `Right` / `Left`: expand a search result so `Up` / `Down` walk its matches, then collapse it again. `Right` expands only with the input cursor at the end of the query.
- `Enter`: resume the selected session or selected tree message.
- `Tab` / `Ctrl+V`: show or hide the preview of a search result (in the tree: `Tab`).
- `Ctrl+B`: open the conversation tree.
- `Ctrl+A`: toggle current-project filtering.
- `Ctrl+H`: switch between manual, automated, and all sessions.
- `Ctrl+L`: switch between all machines, this machine, remote machines and each remote host (only with remote hosts configured, see [Remote hosts](remote-hosts.md)).
- `Ctrl+R`: toggle regex search.
- `Ctrl+G`: find a session across the whole history by describing it (AI search; Opencode sessions are left out).
- `Ctrl+C`: clear the current query, or quit when the query is empty.
- `Esc`: close preview or quit.

Text editing also supports `Home`, `End` / `Ctrl+E`, `Delete`, `Ctrl+W`, `Alt+Backspace`, `Alt+D`, `Alt+B` / `Alt+Left`, `Alt+F` / `Alt+Right`, and `Ctrl+Left` / `Ctrl+Right`.

## Tree view

Tree view shows conversation branches, fork points, latest-chain markers, and compaction boundaries.

For Claude Code sessions, resuming from a non-latest tree message creates a forked JSONL transcript and resumes that branch. This lets you continue from the exact point you selected instead of the latest leaf.

Selecting a message works for Claude Code sessions only. Opencode sessions open at their latest point whichever message you select, Codex sessions have no tree (the view says `No displayable messages in this session`), and for Claude Desktop sessions `Enter` only opens the app on macOS.

Useful keys in tree view:

- `Left` / `Right`: jump to the previous or next branch point.
- `Tab`: toggle message preview.
- `Enter`: resume from the selected message.
- `Ctrl+C`, `b`, or `Esc`: return to search.
- `q`: quit.

## AI search

Press `Ctrl+G`, describe the session in your own words (any language), then press `Enter`. `ccs` starts `claude -p` as a search agent over your whole history, not only the sessions on screen, though it cannot return Opencode sessions. The agent may only run `ccs search` and `ccs show`; it tries several phrasings and returns up to five sessions, each with a short reason.

A search takes about half a minute. While it runs, the status bar shows the queries the agent is trying, and `Esc` stops it. In the results, `Enter` opens a session at its latest point, `→` or `Ctrl+B` opens its tree, typing edits the query, and `Esc` returns to the list you came from.

The agent runs with lean flags (haiku by default, no MCP servers, plugins or settings, `--no-session-persistence`), so searches do not add sessions to your history.

## Configuration

`ccs` reads `$XDG_CONFIG_HOME/ccs/config.toml`, or `~/.config/ccs/config.toml` when `XDG_CONFIG_HOME` is not set (macOS included). The file is optional:

```toml
[agent_search]
model = "claude-haiku-5-5"   # optional, this is the default
effort = "medium"            # optional: low | medium | high | xhigh | max; not passed by default
```

The file is read on every `Ctrl+G` search, so `[agent_search]` edits apply without restarting `ccs`. A typo inside `[agent_search]` or an unknown `effort` value is shown in the status bar instead of starting the search. The model name is not checked: an error from `claude` is shown as is.
