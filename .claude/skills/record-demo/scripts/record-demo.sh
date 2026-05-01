#!/usr/bin/env bash
# Record demo.gif for ccs using scripted tmux + asciinema + agg.
#
# Depends on: tmux, asciinema, agg, python3 (macOS: brew install tmux asciinema agg).
# Produces: demo.gif in the repo root (CWD).
#
# Flow:
#   1. Start sized tmux pane (120x35), remap prefix away from C-b
#   2. Set neutral PS1 so pane never leaks real $USER
#   3. Launch ./target/release/ccs with CCFS_SEARCH_PATH pointed at fake data
#   4. Drive UI via tmux send-keys; after each action, capture-pane -p -e and
#      wrap the snapshot in an asciinema v2 event
#   5. Kill tmux BEFORE capturing a final post-exit frame (avoid shell prompt leak)
#   6. Convert demo.cast -> demo.gif via agg
#
# Environment overrides:
#   DEMO_DATA   — path with synthetic JSONL fixtures (default /tmp/ccfs-demo/projects)
#   CCS_BIN     — path to ccs binary (default ./target/release/ccs)
#   COLS, ROWS  — terminal size (default 120 x 35)
#   GIF         — output path (default ./demo.gif)

set -euo pipefail

SESSION="${SESSION:-ccs-demo-record}"
CAST="${CAST:-/tmp/ccs-demo.cast}"
GIF="${GIF:-demo.gif}"
COLS="${COLS:-120}"
ROWS="${ROWS:-35}"
DEMO_DATA="${DEMO_DATA:-/tmp/ccfs-demo/projects}"
CCS_BIN="${CCS_BIN:-./target/release/ccs}"

for tool in tmux asciinema agg python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: '$tool' not on PATH; install via 'brew install $tool'" >&2
        exit 1
    fi
done

if [ ! -x "$CCS_BIN" ]; then
    echo "error: $CCS_BIN not found. Run 'cargo build --release' first." >&2
    exit 1
fi

if [ ! -d "$DEMO_DATA" ]; then
    echo "error: $DEMO_DATA does not exist. Run gen-fake-sessions.py first." >&2
    exit 1
fi

tmux kill-session -t "$SESSION" 2>/dev/null || true
rm -f "$CAST" "$GIF"

tmux new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" \
    -e "TERM=xterm-256color" -e "COLORTERM=truecolor"
# C-b collides with ccs tree-view shortcut; move tmux prefix out of the way
tmux set-option -t "$SESSION" prefix C-z
# truecolor: tmux silently downgrades RGB→256 unless terminal-features advertises
# RGB for the pane's TERM. Without this, ccs's purple SELECTION_BG (#4B0082) and
# blue BRANCH_FG (#56C2FF) collapse to a flat grey. terminal-features is server-
# wide; setting it *before* the pane emits any RGB escape is enough.
tmux set-option -gas terminal-features ",xterm-256color:RGB"

# Neutral prompt — in case anything ever drops us to a shell mid-recording
tmux send-keys -t "$SESSION" "clear; export PS1='$ ' COLORTERM=truecolor; clear" Enter
sleep 0.5

# Write asciinema v2 cast header
python3 -c "
import json, time
print(json.dumps({
    'version': 2,
    'width': $COLS,
    'height': $ROWS,
    'timestamp': int(time.time()),
    'env': {'TERM': 'xterm-256color'},
}))
" > "$CAST"

START=$(python3 -c 'import time; print(time.time())')

# Capture one frame: take pane snapshot with ANSI, wrap as an asciinema event
capture() {
    python3 - "$START" "$SESSION" "$CAST" "$ROWS" <<'PYEOF'
import subprocess, json, sys, time
start = float(sys.argv[1])
session = sys.argv[2]
cast_file = sys.argv[3]
rows = int(sys.argv[4])
offset = time.time() - start
result = subprocess.run(
    ["tmux", "capture-pane", "-t", session, "-p", "-e"],
    capture_output=True, text=False,
)
raw = result.stdout.decode("utf-8", errors="replace")
lines = raw.split("\n")
out = "\033[2J\033[H"
for i, line in enumerate(lines[:rows]):
    out += f"\033[{i+1};1H{line}\033[K\n"
with open(cast_file, "a") as f:
    f.write(json.dumps([round(offset, 6), "o", out]) + "\n")
PYEOF
}

# --- Scene 1: Launch → recent sessions screen ---
tmux send-keys -t "$SESSION" "CCFS_SEARCH_PATH=$DEMO_DATA $CCS_BIN" Enter
sleep 2.5
capture
sleep 2.0
capture

# --- Scene 2: Type search query "function" ---
for c in f u n c t i o n; do
    tmux send-keys -t "$SESSION" -l "$c"
    sleep 0.2
done
sleep 3.5  # debounce + search
capture
sleep 0.8
capture

# --- Scene 3: Navigate session groups ---
tmux send-keys -t "$SESSION" Down
sleep 0.6
capture
tmux send-keys -t "$SESSION" Down
sleep 0.6
capture
tmux send-keys -t "$SESSION" Down
sleep 0.8
capture

# --- Scene 4: Expand group with Right ---
tmux send-keys -t "$SESSION" Right
sleep 1.4
capture
sleep 0.5

# Navigate within group
tmux send-keys -t "$SESSION" Down
sleep 0.5
capture
tmux send-keys -t "$SESSION" Down
sleep 0.5
capture
sleep 0.5

# --- Scene 5: Collapse back ---
tmux send-keys -t "$SESSION" Left
sleep 0.7
capture
tmux send-keys -t "$SESSION" Up
sleep 0.4
capture
tmux send-keys -t "$SESSION" Up
sleep 0.4
capture

# --- Scene 6: Tree view (Ctrl+B) ---
tmux send-keys -t "$SESSION" C-b
sleep 2.5
capture
sleep 0.6

# Navigate tree
tmux send-keys -t "$SESSION" Down
sleep 0.35
tmux send-keys -t "$SESSION" Down
sleep 0.35
tmux send-keys -t "$SESSION" Down
sleep 0.35
tmux send-keys -t "$SESSION" Down
sleep 0.5
capture

# Jump between branches (shows [fork] markers in action)
tmux send-keys -t "$SESSION" Right
sleep 0.45
tmux send-keys -t "$SESSION" Right
sleep 0.8
capture

# Preview with Tab
tmux send-keys -t "$SESSION" Tab
sleep 1.5
capture
sleep 0.5
tmux send-keys -t "$SESSION" Tab
sleep 0.4

# Back to search
tmux send-keys -t "$SESSION" Escape
sleep 1.0
capture
sleep 0.3

# Kill tmux BEFORE one more capture — prevents shell prompt from leaking into
# the final frame ($USER would be visible otherwise)
tmux kill-session -t "$SESSION" 2>/dev/null || true

echo "Converting to GIF…"
agg --font-size 14 --font-family Iosevka --theme asciinema --speed 1.1 --last-frame-duration 2 "$CAST" "$GIF"
rm -f "$CAST"

SIZE=$(du -h "$GIF" | cut -f1)
echo "Done! $GIF ($SIZE)"
