---
name: record-demo
description: Record a demo GIF of the ccs TUI (or any other ratatui/crossterm-based Rust TUI) using a scripted tmux+asciinema+agg pipeline with synthetic session data. Use when the user asks to re-record demo.gif, refresh the demo, or create a new TUI recording.
allowed-tools: [Bash, Read, Write, Edit]
---

# Record ccs Demo GIF

Automated, deterministic pipeline for producing `demo.gif` for the ccs README without leaking real user data.

## When to use

- README's `demo.gif` is stale after UI changes or new features
- Need a clean demo recording that does not expose real project names (e.g. `avito-*`) or the real username (`vkkovalev`)
- Need reproducible recordings that can be re-run from CI or locally

Do NOT use for:
- Recording interactive user sessions live — this is a scripted pipeline, not screen-recording software
- Recording the AI ranking feature (`Ctrl+G`) — it calls `claude -p` as a subprocess; timing is non-deterministic

## Two pipelines

**`record-demo-kitty.sh` (preferred on macOS, full color)** — drives a real
kitty OS-window via remote control and records it with `screencapture -v -l<wid>`.
Captures the actual rendered frame so truecolor / 256-color escapes from
ratatui survive intact, the user's real font (Iosevka 20px from kitty.conf)
is used, and kitty default theme shows up in the GIF.
Requires: kitty (with `allow_remote_control yes` and `listen_on …` set,
both already in the user's kitty.conf), `jq`, `ffmpeg`. macOS-only.

**`record-demo.sh` (legacy / cross-platform)** — drives a tmux pane and
converts the asciinema cast with `agg`. Cannot capture color: ratatui+crossterm
0.28 emit only `\e[1m`/`\e[m` in headless tmux on this stack; the GIF ends
up monochrome regardless of `--theme`/`terminal-features:RGB` tweaks.
Requires: `tmux`, `asciinema`, `agg`, `python3`.

A built binary: `cargo build --release` produces `target/release/ccs`.

## Activation Triggers

- "запиши demo.gif"
- "обнови demo.gif"
- "refresh the demo GIF"
- "re-record the demo"
- "make a new demo for ccs"

## Pipeline overview

```
gen-fake-sessions.py  ->  /tmp/ccfs-demo/projects/...    (JSONL fixtures)
                                     |
record-demo.sh  ->  tmux pane (120x35)
              ->  CCFS_SEARCH_PATH=/tmp/... ./target/release/ccs
              ->  tmux send-keys (scripted input)
              ->  tmux capture-pane -p -e  (snapshot per action)
              ->  python3 JSON-wraps each snapshot into asciinema v2 event
              ->  demo.cast
              ->  agg demo.cast demo.gif
```

## Usage

From the repo root, **inside a kitty window** (preferred — full color):

```bash
${CLAUDE_PLUGIN_ROOT}/.claude/skills/record-demo/scripts/gen-fake-sessions.py
${CLAUDE_PLUGIN_ROOT}/.claude/skills/record-demo/scripts/record-demo-kitty.sh
```

Fallback (monochrome, but works without kitty):

```bash
${CLAUDE_PLUGIN_ROOT}/.claude/skills/record-demo/scripts/record-demo.sh
```

Or after `cp`-ing the scripts to `/tmp/`:

```bash
python3 /tmp/gen-fake-sessions.py   # creates /tmp/ccfs-demo/projects/*
bash /tmp/record-demo.sh            # produces demo.gif in repo root
```

Verify result:

```bash
python3 -c "from PIL import Image; i=Image.open('demo.gif'); n=0
try:
  while True: n+=1; i.seek(i.tell()+1)
except EOFError: pass
print(f'{n} frames, {i.size}')"
```

Eyeball mid-frames:

```bash
python3 -c "from PIL import Image; i=Image.open('demo.gif')
for idx in [5, 12, 20]:
    i.seek(idx); i.convert('RGBA').save(f'/tmp/frame_{idx}.png')"
```

Open the PNGs via `Read` tool or Preview.app.

## Gotchas (learned the hard way)

### Path must contain `/projects/` literal

`extract_project_from_path` in `src/search/ripgrep.rs` looks for the substring `projects/` to extract the project name from the session directory. Fake data must live under `.../projects/-Users-user-projects-<name>/session.jsonl`, otherwise the list shows session IDs instead of project names.

### Remap tmux prefix

tmux's default prefix is `C-b` — same as ccs's tree-view shortcut. **Before sending keys**, remap to `C-z`:

```bash
tmux set-option -t "$SESSION" prefix C-z
```

Otherwise `tmux send-keys C-b` enters tmux command mode instead of reaching the app.

### Hide real shell prompt

After the app exits, tmux pane shows `$USER@hostname dir%` which leaks `vkkovalev`. Either:
- Set a neutral `PS1='$ '` in the tmux pane before launching the app, AND
- Do NOT `capture` after the app exits (capture only frames while the app is still running)

### `tmux capture-pane -e` is the magic flag

Plain `capture-pane -p` loses colors → GIF renders as black screen. `-e` includes ANSI escape codes. The helper Python script in `record-demo.sh` wraps each captured snapshot with `\033[2J\033[H` (clear + home) and emits one asciinema event per snapshot.

### Debounce timing

ccs debounces search at 300ms. After typing a query, wait ≥ 2 seconds before capturing or results won't be rendered yet. The default `send_and_capture` helper uses 0.3s which is enough for cursor moves but NOT for search results.

### Automation filter defaults to Manual

On launch ccs shows `[Manual]` in the search-box border. Fake sessions generated by this skill are detected as `Manual` sessions by the heuristic (no `agent-*` path, no `subagents/`). Do NOT press `Ctrl+H` during the demo — it cycles to `Auto`, which hides all fake sessions → empty search results → boring demo.

### AI ranking (`Ctrl+G`) — skip in scripted demos

Calling `claude -p` hits the network with variable latency (2–15s). No reliable way to capture the "AI thinking…" → "ranked" transition deterministically. If you want to show AI ranking, shoot a separate GIF manually.

## Demo scenes (v0.11.0)

Keep the recording ≈ 14 s / 30–35 frames / 300–500 KB:

1. **Launch** → recent sessions list (2.5 s initial pause for readability)
2. **Type query** `function` (8 chars × 0.2 s + 3 s debounce)
3. **Navigate groups** with `↓` (3 × 0.6 s)
4. **Expand group** with `→` (1.4 s pause) — show sub-matches
5. **Navigate matches** within group (2 × 0.5 s)
6. **Collapse** with `←` and go back up (3 × 0.4 s)
7. **Enter tree mode** `Ctrl+B` (2.5 s)
8. **Navigate tree** with `↓` (4 × 0.35 s)
9. **Jump branches** with `→` (2 × 0.45 s) — shows `[fork]` markers
10. **Preview** with `Tab` (1.5 s pause) → close with `Tab`
11. **Back to search** with `Esc` (1 s)
12. Kill tmux without another capture

## Files in this skill

| Path | Purpose |
|---|---|
| `scripts/gen-fake-sessions.py` | Generates 9 synthetic sessions across 6 projects (weather-app, rust-cli-tool, blog-engine, todo-api, markdown-parser, http-server) at `/tmp/ccfs-demo/projects/` |
| `scripts/record-demo.sh` | Main recorder: tmux + scripted keystrokes + snapshot capture + agg conversion. Writes `demo.gif` to repo root |

## Customisation

- Change output path: edit `GIF=` in `record-demo.sh`
- Change terminal size: edit `COLS` / `ROWS`
- Change theme: `agg --theme` flag (options: `monokai`, `dracula`, `github-dark`, etc)
- Add/remove sessions: edit the `write_session(...)` calls in `gen-fake-sessions.py`
- Target a different TUI binary: replace `./target/release/ccs` in `record-demo.sh` with the desired binary

## Verification checklist before committing

- [ ] File size between 200 KB and 800 KB (bigger → reduce font size or trim scenes)
- [ ] Frame count ≥ 15 (fewer usually means capture skipped some frames due to tight sleeps)
- [ ] Mid-frames visually show: populated recent list / search results with project names / tree view with `[fork]` markers
- [ ] No `avito-*`, `vkkovalev`, `@avito.ru` visible in any extracted PNG frame
- [ ] `README.md` still references `demo.gif` (line 7 as of v0.11.0)

## Skipping AI ranking on purpose

If recording a skill-specific demo that MUST show AI mode, stub the `claude` binary before running the recorder:

```bash
mkdir -p /tmp/ccs-stub
cat > /tmp/ccs-stub/claude <<'STUB'
#!/usr/bin/env bash
# Fake Claude responds instantly with a valid ranking JSON
sleep 1  # simulate thinking
echo '["a1b2c3d4e5f6", "c3d4e5f6a7b8", "e5f6a7b8c9d0"]'
STUB
chmod +x /tmp/ccs-stub/claude
PATH="/tmp/ccs-stub:$PATH" bash /tmp/record-demo.sh
```

Add `tmux send-keys -t "$SESSION" C-g; sleep 1; tmux send-keys -t "$SESSION" -l "weather api"; sleep 0.5; tmux send-keys -t "$SESSION" Enter; sleep 2` into the recording script where you want the AI scene.
