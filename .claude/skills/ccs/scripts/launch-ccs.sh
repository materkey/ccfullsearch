#!/usr/bin/env bash
# launch ccs picker in a terminal overlay (tmux/kitty/wezterm) and capture selection.
# usage: launch-ccs.sh [query]
# output: key-value pairs from ccs pick (session_id, file_path, source, project)
#         empty if cancelled (exit 1)

set -euo pipefail

# resolve ccs to absolute path so overlay shells (sh -c) can find it
# even when /opt/homebrew/bin or similar dirs are not in sh's default PATH
CCS_BIN=$(command -v ccs 2>/dev/null || true)
if [ -z "$CCS_BIN" ]; then
    echo "error: ccs not found in PATH" >&2
    echo "install: cargo install ccfullsearch" >&2
    exit 1
fi

OUTPUT_FILE=$(mktemp /tmp/ccs-output-XXXXXX)
SENTINEL=""
trap 'rm -f "$OUTPUT_FILE" ${SENTINEL:+"$SENTINEL"}' EXIT

# Build command string with absolute paths baked in (safe from shell expansion).
# Query is passed via env var to avoid injection via backticks/$().
QUERY="${1:-}"
export CCS_QUERY="$QUERY"
if [ -n "$QUERY" ]; then
    CCS_CMD="'$CCS_BIN' pick --output='$OUTPUT_FILE' \"\$CCS_QUERY\""
else
    CCS_CMD="'$CCS_BIN' pick --output='$OUTPUT_FILE'"
fi
CWD="$(pwd)"

# validate_output: check that output file exists, is non-empty, and contains
# all required keys (guards against partial/truncated writes)
validate_output() {
    [ -s "$1" ] && grep -q '^session_id: ' "$1" && grep -q '^file_path: ' "$1" && grep -q '^source: ' "$1" && grep -q '^project: ' "$1"
}

OVERLAY_TITLE="ccs: pick session"

# popup size: override via CCS_POPUP_WIDTH / CCS_POPUP_HEIGHT env vars
POPUP_W="${CCS_POPUP_WIDTH:-90%}"
POPUP_H="${CCS_POPUP_HEIGHT:-90%}"

# tmux: display-popup -E blocks until command exits
if [ -n "${TMUX:-}" ] && command -v tmux >/dev/null 2>&1; then
    tmux display-popup -E -e "CCS_QUERY=$QUERY" -e "PATH=$PATH" -w "$POPUP_W" -h "$POPUP_H" -T " $OVERLAY_TITLE " -d "$CWD" -- sh -c "$CCS_CMD || true"
    if validate_output "$OUTPUT_FILE"; then
        cat "$OUTPUT_FILE"
        exit 0
    else
        exit 1
    fi
fi

# kitty: overlay with sentinel file for blocking
KITTY_SOCK="${KITTY_LISTEN_ON:-}"
if [ -n "$KITTY_SOCK" ] && command -v kitty >/dev/null 2>&1; then
    SENTINEL=$(mktemp /tmp/ccs-done-XXXXXX)
    rm -f "$SENTINEL"

    KITTY_ARGS=(kitty @ --to "$KITTY_SOCK" launch --type=overlay --title="$OVERLAY_TITLE" --cwd="$CWD" --env "CCS_QUERY=$QUERY" --env "PATH=$PATH")
    if [ -n "${KITTY_WINDOW_ID:-}" ]; then
        KITTY_ARGS+=(--match "id:${KITTY_WINDOW_ID}")
    fi
    KITTY_ARGS+=(sh -c "$CCS_CMD || true; touch '$SENTINEL'")

    "${KITTY_ARGS[@]}" >/dev/null 2>&1

    WAIT=0
    while [ ! -f "$SENTINEL" ]; do
        sleep 0.3
        WAIT=$((WAIT + 1))
        if [ "$WAIT" -ge 1000 ]; then
            echo "error: ccs picker timed out after 300s" >&2
            exit 1
        fi
    done
    rm -f "$SENTINEL"
    if validate_output "$OUTPUT_FILE"; then
        cat "$OUTPUT_FILE"
        exit 0
    else
        exit 1
    fi
fi

# wezterm: split-pane with sentinel file for blocking
if [ -n "${WEZTERM_PANE:-}" ] && command -v wezterm >/dev/null 2>&1; then
    SENTINEL=$(mktemp /tmp/ccs-done-XXXXXX)
    rm -f "$SENTINEL"

    WEZTERM_PCT="${CCS_POPUP_HEIGHT:-90%}"
    WEZTERM_PCT="${WEZTERM_PCT%%%}"
    CCS_QUERY="$QUERY" PATH="$PATH" wezterm cli split-pane --bottom --percent "$WEZTERM_PCT" \
        --pane-id "$WEZTERM_PANE" --cwd "$CWD" -- sh -c "$CCS_CMD || true; touch '$SENTINEL'" >/dev/null 2>&1

    WAIT=0
    while [ ! -f "$SENTINEL" ]; do
        sleep 0.3
        WAIT=$((WAIT + 1))
        if [ "$WAIT" -ge 1000 ]; then
            echo "error: ccs picker timed out after 300s" >&2
            exit 1
        fi
    done
    rm -f "$SENTINEL"
    if validate_output "$OUTPUT_FILE"; then
        cat "$OUTPUT_FILE"
        exit 0
    else
        exit 1
    fi
fi

# fallback: direct run (works from Bash tool in Claude Code)
if [ -n "$QUERY" ]; then
    "$CCS_BIN" pick --output="$OUTPUT_FILE" "$QUERY" || true
else
    "$CCS_BIN" pick --output="$OUTPUT_FILE" || true
fi
if validate_output "$OUTPUT_FILE"; then
    cat "$OUTPUT_FILE"
    exit 0
else
    exit 1
fi
