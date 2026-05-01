#!/usr/bin/env bash
# Record demo.gif by driving a real kitty OS window via remote control and
# grabbing it with macOS `screencapture -l<window-id>`. Unlike the tmux+agg
# pipeline (record-demo.sh), this captures the actual rendered frame so:
#   - truecolor and 256-color escapes from ratatui survive
#   - the user's real font (Iosevka 20px from kitty.conf) is used
#   - kitty default theme is what shows up in the GIF
#
# Depends on: kitty (with allow_remote_control + listen_on), jq, screencapture
# (macOS), ffmpeg.
# Produces: demo.gif in CWD.
#
# Environment overrides:
#   DEMO_DATA       — path with synthetic JSONL fixtures (default /tmp/ccfs-demo/projects)
#   CCS_BIN         — path to ccs binary (default ./target/release/ccs)
#   GIF             — output path (default ./demo.gif)
#   FPS             — frames per second sampled from the recording (default 10)
#   COLS, ROWS      — terminal columns/rows (default 120 x 35)
#   GIF_WIDTH       — output GIF width in px (default 720)
#   DEMO_DURATION   — hard cap for screencapture in seconds (default 18). Must
#                     exceed the sum of scene `sleep`s, currently ~17.5 s.

set -euo pipefail

GIF="${GIF:-demo.gif}"
COLS="${COLS:-120}"
ROWS="${ROWS:-35}"
FPS="${FPS:-10}"
DEMO_DATA="${DEMO_DATA:-/tmp/ccfs-demo/projects}"
CCS_BIN="${CCS_BIN:-./target/release/ccs}"
# kitty launches the child process in its own cwd, not ours, so resolve to absolute
case "$CCS_BIN" in /*) ;; *) CCS_BIN="$PWD/$CCS_BIN" ;; esac
TITLE="ccs-demo-record"
TMP_BASE="${TMPDIR:-/tmp}/ccs-demo.$$"
MOV="$TMP_BASE.mov"
PALETTE="$TMP_BASE-palette.png"

for tool in kitty jq screencapture ffmpeg rg; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: '$tool' not on PATH" >&2
        exit 1
    fi
done
# ccs spawns rg as a child; kitty does not inherit our shell PATH, so pass the
# directory containing rg (plus our current PATH) into the launched env below.
RG_DIR="$(dirname "$(command -v rg)")"

if [ -z "${KITTY_LISTEN_ON:-}" ]; then
    echo "error: KITTY_LISTEN_ON not set; run this script from inside a kitty window" >&2
    echo "with allow_remote_control=yes and listen_on configured" >&2
    exit 1
fi

if [ ! -x "$CCS_BIN" ]; then
    echo "error: $CCS_BIN not found. Run 'cargo build --release' first." >&2
    exit 1
fi

if [ ! -d "$DEMO_DATA" ]; then
    echo "error: $DEMO_DATA does not exist. Run gen-fake-sessions.py first." >&2
    exit 1
fi

KITTY="kitty @ --to=$KITTY_LISTEN_ON"

# Cleanup helper — always close the demo window and wipe temp files even on
# script failure. EXIT trap return value is ignored by bash, hence `exit $rc`.
cleanup() {
    local rc=$?
    [ -n "${GRABBER_PID:-}" ] && kill "$GRABBER_PID" 2>/dev/null || true
    [ -n "${WIN_ID:-}" ] && $KITTY close-window --match "id:$WIN_ID" 2>/dev/null || true
    rm -f "$MOV" "$PALETTE"
    exit $rc
}
trap cleanup EXIT

rm -f "$GIF" "$MOV" "$PALETTE"

# ----- Launch isolated kitty OS window running ccs -----
# --keep-focus stops the new window from stealing focus (so user can keep
# typing in their main kitty session). --type=os-window creates a top-level
# macOS window with its own platform_window_id (=CGWindowID), consumed by
# `screencapture -l<id>`.
# kitty launch prints the new window's internal id to stdout — capture it
# and use it for all subsequent --match calls. Title-based matching is
# brittle because shell prompt indicators (fish ⠹ spinner) prepend chars.
# PATH is critical: ccs spawns rg, and kitty launch does not inherit our shell's
# PATH (only the env it was started with). Force /opt/homebrew/bin (and friends).
WIN_ID=$($KITTY launch --type=os-window --title="$TITLE" --keep-focus \
    --env "CCFS_SEARCH_PATH=$DEMO_DATA" \
    --env "CCS_DEMO=1" \
    --env "PATH=$RG_DIR:$PATH" \
    -- "$CCS_BIN")

if ! [[ "$WIN_ID" =~ ^[0-9]+$ ]]; then
    echo "error: kitty launch did not return a numeric window id (got: '$WIN_ID')" >&2
    exit 1
fi

# Give kitty a moment to map the window and ccs a moment to render
sleep 1.8

# Resize to the requested cell grid (cells, not pixels).
$KITTY resize-os-window --match "id:$WIN_ID" \
    --action=resize --unit=cells --width="$COLS" --height="$ROWS" >/dev/null
sleep 0.6

# Get the macOS CGWindowID for screencapture
WID=$($KITTY ls | jq -r --argjson w "$WIN_ID" '
    .[] | select(.tabs[].windows[] | .id == $w) | .platform_window_id')

if [ -z "$WID" ] || [ "$WID" = "null" ]; then
    echo "error: failed to find platform_window_id for kitty window $WIN_ID" >&2
    exit 1
fi
echo "demo window: kitty_id=$WIN_ID platform_window_id=$WID"

# ----- Background video recorder -----
# screencapture -v -l<wid> records video of the named window directly. This
# is much smoother than looping `screencapture -t png` (which capped out at
# ~1fps because each invocation re-acquires the window). -V caps the
# recording length so we have a hard upper bound; the foreground driver
# kills the recorder via SIGINT once scenes finish, which screencapture
# handles cleanly (flushes the .mov file).
DEMO_DURATION="${DEMO_DURATION:-18}"
screencapture -x -o -v -V "$DEMO_DURATION" -l"$WID" "$MOV" &
GRABBER_PID=$!
sleep 0.4   # let recorder hook into the window before scenes start

# Helper for sending keys/text to the demo window
send() { $KITTY send-key --match "id:$WIN_ID" "$@" >/dev/null; }
type_str() { $KITTY send-text --match "id:$WIN_ID" "$@" >/dev/null; }

# ----- Scenes -----
# Times below are wall-clock pauses between actions; each pause lets the
# grabber accumulate frames showing that state.

sleep 2.0   # initial: recent sessions on screen

# Type "function"
type_str "function"
sleep 3.5   # debounce + search results render

# Navigate down through groups
for _ in 1 2 3; do send down; sleep 0.6; done

# Expand 3rd group with Right
send right
sleep 1.4

# Move within group
send down; sleep 0.5
send down; sleep 0.5

# Collapse with Left
send left
sleep 0.7
send up; sleep 0.4
send up; sleep 0.4

# Tree view (Ctrl+B)
send ctrl+b
sleep 2.3

# Navigate tree
for _ in 1 2 3 4; do send down; sleep 0.35; done

# Jump branches
send right; sleep 0.45
send right; sleep 0.8

# Preview with Tab, then close
send tab
sleep 1.5
send tab
sleep 0.4

# Back to search
send escape
sleep 1.0

# ----- Stop recorder and close window -----
# SIGINT (not SIGTERM) — screencapture catches it, finalises the mov header,
# and exits cleanly. SIGTERM tends to leave a truncated/unplayable file.
kill -INT "$GRABBER_PID" 2>/dev/null || true
wait "$GRABBER_PID" 2>/dev/null || true
GRABBER_PID=""

$KITTY close-window --match "id:$WIN_ID" >/dev/null
sleep 0.3

if [ ! -s "$MOV" ]; then
    echo "error: recording $MOV is missing or empty" >&2
    exit 1
fi
echo "recorded $(du -h "$MOV" | cut -f1)"

# ----- Convert mov → gif via single-pass palette -----
# split[a][b]; [a]→palettegen; [b]→paletteuse — one decode + one fps/scale pass
# instead of two. 64-color palette + no dithering keeps the file ~400 KB while
# ratatui's flat color blocks survive cleanly (no dither artifacts).
echo "encoding GIF…"
GIF_WIDTH="${GIF_WIDTH:-720}"
ffmpeg -y -i "$MOV" -filter_complex \
    "fps=$FPS,scale=$GIF_WIDTH:-2:flags=lanczos,split[a][b];[a]palettegen=max_colors=64[p];[b][p]paletteuse=dither=none" \
    "$GIF" 2>/dev/null

SIZE=$(du -h "$GIF" | cut -f1)
echo "Done! $GIF ($SIZE)"
