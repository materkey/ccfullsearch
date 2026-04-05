#!/usr/bin/env bash
# launch claude --resume in a terminal overlay (tmux/kitty/wezterm).
# usage: launch-resume.sh <session_id> [--cwd <dir>]
# blocks until claude exits, then returns.

set -euo pipefail

SESSION_ID="${1:?usage: launch-resume.sh <session_id> [--cwd <dir>]}"
shift

# parse optional --cwd
WORK_DIR="$(pwd)"
while [ $# -gt 0 ]; do
    case "$1" in
        --cwd) WORK_DIR="$2"; shift 2 ;;
        --cwd=*) WORK_DIR="${1#--cwd=}"; shift ;;
        *) shift ;;
    esac
done

CLAUDE_BIN=$(command -v claude 2>/dev/null || true)
if [ -z "$CLAUDE_BIN" ]; then
    echo "error: claude not found in PATH" >&2
    exit 1
fi

RESUME_CMD="'$CLAUDE_BIN' --resume '$SESSION_ID'"
OVERLAY_TITLE="claude: resume $SESSION_ID"

POPUP_W="${CCS_POPUP_WIDTH:-90%}"
POPUP_H="${CCS_POPUP_HEIGHT:-90%}"

# tmux: display-popup -E blocks until command exits
if [ -n "${TMUX:-}" ] && command -v tmux >/dev/null 2>&1; then
    tmux display-popup -E -e "PATH=$PATH" -w "$POPUP_W" -h "$POPUP_H" \
        -T " $OVERLAY_TITLE " -d "$WORK_DIR" -- sh -c "$RESUME_CMD"
    exit 0
fi

# kitty: overlay with sentinel file for blocking
KITTY_SOCK="${KITTY_LISTEN_ON:-}"
if [ -n "$KITTY_SOCK" ] && command -v kitty >/dev/null 2>&1; then
    SENTINEL=$(mktemp /tmp/ccs-resume-done-XXXXXX)
    rm -f "$SENTINEL"
    trap 'rm -f "$SENTINEL"' EXIT

    KITTY_ARGS=(kitty @ --to "$KITTY_SOCK" launch --type=overlay
        --title="$OVERLAY_TITLE" --cwd="$WORK_DIR" --env "PATH=$PATH")
    if [ -n "${KITTY_WINDOW_ID:-}" ]; then
        KITTY_ARGS+=(--match "id:${KITTY_WINDOW_ID}")
    fi
    KITTY_ARGS+=(sh -c "$RESUME_CMD; touch '$SENTINEL'")

    "${KITTY_ARGS[@]}" >/dev/null 2>&1

    while [ ! -f "$SENTINEL" ]; do sleep 0.3; done
    rm -f "$SENTINEL"
    exit 0
fi

# wezterm: split-pane with sentinel file for blocking
if [ -n "${WEZTERM_PANE:-}" ] && command -v wezterm >/dev/null 2>&1; then
    SENTINEL=$(mktemp /tmp/ccs-resume-done-XXXXXX)
    rm -f "$SENTINEL"
    trap 'rm -f "$SENTINEL"' EXIT

    WEZTERM_PCT="${CCS_POPUP_HEIGHT:-90%}"
    WEZTERM_PCT="${WEZTERM_PCT%%%}"
    PATH="$PATH" wezterm cli split-pane --bottom --percent "$WEZTERM_PCT" \
        --pane-id "$WEZTERM_PANE" --cwd "$WORK_DIR" -- sh -c "$RESUME_CMD; touch '$SENTINEL'" >/dev/null 2>&1

    while [ ! -f "$SENTINEL" ]; do sleep 0.3; done
    rm -f "$SENTINEL"
    exit 0
fi

# fallback: direct run
cd "$WORK_DIR"
exec "$CLAUDE_BIN" --resume "$SESSION_ID"
