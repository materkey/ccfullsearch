#!/usr/bin/env bash
# resume a session in a terminal overlay (tmux/kitty/wezterm).
# usage: launch-resume.sh <session_id> --file <file_path> [--uuid <uuid>] [--host <host>]
#        launch-resume.sh <session_id> [--cwd <dir>]
# with --file, `ccs resume` picks the agent (Claude, Codex, Opencode), the
# project directory and the branch at --uuid; with --host (a remote host from
# `ccs pick`) it opens the session on that machine over ssh. without --file it
# falls back to `claude --resume` in --cwd.
# blocks until the session exits, then returns.

set -euo pipefail

SESSION_ID="${1:?usage: launch-resume.sh <session_id> --file <file_path> [--uuid <uuid>] [--host <host>]}"
shift

# parse optional --cwd / --host / --file / --uuid
WORK_DIR="$(pwd)"
HOST=""
FILE=""
UUID=""
while [ $# -gt 0 ]; do
    case "$1" in
        --cwd) WORK_DIR="$2"; shift 2 ;;
        --cwd=*) WORK_DIR="${1#--cwd=}"; shift ;;
        --host) HOST="$2"; shift 2 ;;
        --file) FILE="$2"; shift 2 ;;
        --uuid) UUID="$2"; shift 2 ;;
        *) shift ;;
    esac
done

# single-quote a value for `sh -c`
q() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

CCS_BIN=$(command -v ccs 2>/dev/null || true)
if [ -n "$HOST" ] && [ "$HOST" != "local" ] && { [ -z "$CCS_BIN" ] || [ -z "$FILE" ]; }; then
    echo "error: remote resume needs ccs in PATH and --file" >&2
    exit 1
fi

if [ -n "$CCS_BIN" ] && [ -n "$FILE" ]; then
    RESUME_CMD="$(q "$CCS_BIN") resume --session-id $(q "$SESSION_ID") --file $(q "$FILE")"
    if [ -n "$UUID" ]; then
        RESUME_CMD="$RESUME_CMD --uuid $(q "$UUID")"
    fi
    OVERLAY_TITLE="resume $SESSION_ID"
    if [ -n "$HOST" ] && [ "$HOST" != "local" ]; then
        RESUME_CMD="$RESUME_CMD --host $(q "$HOST")"
        OVERLAY_TITLE="$OVERLAY_TITLE on $HOST"
    fi
else
    CLAUDE_BIN=$(command -v claude 2>/dev/null || true)
    if [ -z "$CLAUDE_BIN" ]; then
        echo "error: claude not found in PATH" >&2
        exit 1
    fi
    RESUME_CMD="$(q "$CLAUDE_BIN") --resume $(q "$SESSION_ID")"
    OVERLAY_TITLE="claude: resume $SESSION_ID"
fi

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

    if ! "${KITTY_ARGS[@]}" >/dev/null 2>&1; then
        echo "error: kitty overlay launch failed" >&2
        exit 1
    fi

    TIMEOUT=3600  # 1 hour max wait
    ELAPSED=0
    while [ ! -f "$SENTINEL" ] && [ "$ELAPSED" -lt "$TIMEOUT" ]; do
        sleep 0.3
        ELAPSED=$((ELAPSED + 1))
    done
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
    if ! PATH="$PATH" wezterm cli split-pane --bottom --percent "$WEZTERM_PCT" \
        --pane-id "$WEZTERM_PANE" --cwd "$WORK_DIR" -- sh -c "$RESUME_CMD; touch '$SENTINEL'" >/dev/null 2>&1; then
        echo "error: wezterm split-pane launch failed" >&2
        exit 1
    fi

    TIMEOUT=3600
    ELAPSED=0
    while [ ! -f "$SENTINEL" ] && [ "$ELAPSED" -lt "$TIMEOUT" ]; do
        sleep 0.3
        ELAPSED=$((ELAPSED + 1))
    done
    rm -f "$SENTINEL"
    exit 0
fi

# fallback: direct run
cd "$WORK_DIR"
exec sh -c "$RESUME_CMD"
