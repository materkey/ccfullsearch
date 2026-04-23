#!/usr/bin/env python3
"""
Generate fake Claude Code session JSONL files for ccs demo recording.

Creates nine synthetic sessions across six realistic project names under
/tmp/ccfs-demo/projects/. The directory layout intentionally mirrors the real
~/.claude/projects/ layout so that src/search/ripgrep.rs::extract_project_from_path
extracts the project names correctly.

Usage:
    python3 gen-fake-sessions.py [--out /tmp/ccfs-demo/projects]
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path


def msg(type_, role, content, session_id, ts, branch=None, uid=None, parent=None):
    m = {
        "type": type_,
        "message": {"role": role, "content": [{"type": "text", "text": content}]},
        "sessionId": session_id,
        "timestamp": ts.strftime("%Y-%m-%dT%H:%M:%S.000Z"),
        "cwd": "/Users/user/projects",
    }
    if branch:
        m["branch"] = branch
        m["gitBranch"] = branch
    if uid:
        m["uuid"] = uid
    if parent:
        m["parentUuid"] = parent
    return json.dumps(m)


def write_session(out_dir: Path, project, session_id, branch, messages, base_time):
    dir_path = out_dir / f"-Users-user-projects-{project}"
    dir_path.mkdir(parents=True, exist_ok=True)
    path = dir_path / f"{session_id}.jsonl"
    prev_uuid = None
    with path.open("w") as f:
        for i, (role, content) in enumerate(messages):
            uid = str(uuid.uuid4())
            ts = base_time + timedelta(minutes=i * 2)
            t = "user" if role == "user" else "assistant"
            f.write(msg(t, role, content, session_id, ts, branch, uid, prev_uuid) + "\n")
            prev_uuid = uid


def write_branched_session(out_dir: Path, project, session_id, branch, base_time):
    """Write a session with a fork — lets the tree view show [fork] markers."""
    dir_path = out_dir / f"-Users-user-projects-{project}"
    dir_path.mkdir(parents=True, exist_ok=True)
    path = dir_path / f"{session_id}.jsonl"

    u1 = str(uuid.uuid4()); u2 = str(uuid.uuid4())
    u3 = str(uuid.uuid4()); u4 = str(uuid.uuid4())
    u5a = str(uuid.uuid4()); u6a = str(uuid.uuid4())
    u5b = str(uuid.uuid4()); u6b = str(uuid.uuid4())

    with path.open("w") as f:
        f.write(msg("user", "user",
                    "Write a function to parse markdown headings into a table of contents",
                    session_id, base_time, branch, u1, None) + "\n")
        f.write(msg("assistant", "assistant",
                    "Here's a function that parses markdown headings and generates a nested TOC structure.",
                    session_id, base_time + timedelta(minutes=2), branch, u2, u1) + "\n")
        f.write(msg("user", "user",
                    "Add support for GitHub-flavored markdown extensions",
                    session_id, base_time + timedelta(minutes=4), branch, u3, u2) + "\n")
        f.write(msg("assistant", "assistant",
                    "Added support for task lists, tables, strikethrough, and autolinks following the GFM spec.",
                    session_id, base_time + timedelta(minutes=6), branch, u4, u3) + "\n")
        # Branch A (from u4)
        f.write(msg("user", "user",
                    "Actually, let's also add emoji shortcodes",
                    session_id, base_time + timedelta(minutes=8), branch, u5a, u4) + "\n")
        f.write(msg("assistant", "assistant",
                    "Added emoji support using gh-emoji crate. Shortcodes like :smile: now render properly.",
                    session_id, base_time + timedelta(minutes=10), branch, u6a, u5a) + "\n")
        # Branch B (alternate continuation from u4)
        f.write(msg("user", "user",
                    "Add math equation rendering with KaTeX",
                    session_id, base_time + timedelta(minutes=12), branch, u5b, u4) + "\n")
        f.write(msg("assistant", "assistant",
                    "Integrated KaTeX for math rendering. Both inline $...$ and block $$...$$ work.",
                    session_id, base_time + timedelta(minutes=14), branch, u6b, u5b) + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--out", default="/tmp/ccfs-demo/projects",
        help="output directory (default: /tmp/ccfs-demo/projects)",
    )
    args = parser.parse_args()

    out_dir = Path(args.out)
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)

    now = datetime.now(timezone.utc).replace(microsecond=0)

    # Recent, rich session
    write_session(out_dir, "weather-app", "a1b2c3d4e5f6", "main", [
        ("user", "Add a function to fetch weather data from the OpenWeatherMap API"),
        ("assistant", "I'll create a function to fetch weather data. Let me first check the existing code structure."),
        ("user", "Use reqwest for HTTP requests and serde for JSON parsing"),
        ("assistant", "Here's the implementation using reqwest and serde with proper error handling via thiserror."),
        ("user", "Add caching so we don't hit the API too often"),
        ("assistant", "Added an in-memory cache with TTL using a HashMap and Instant for expiration tracking."),
        ("user", "Now add unit tests for the caching logic"),
        ("assistant", "Added comprehensive tests covering cache hits, cache misses, and TTL expiration scenarios."),
        ("user", "Can you also add a CLI flag to set the cache duration?"),
        ("assistant", "Done! Added --cache-ttl flag using clap. Defaults to 300 seconds."),
    ], now - timedelta(minutes=45))

    write_session(out_dir, "rust-cli-tool", "b2c3d4e5f6a7", "feature/add-search", [
        ("user", "Implement a fuzzy search function for the file finder"),
        ("assistant", "I'll implement fuzzy search using a scoring algorithm similar to fzf."),
        ("user", "Make it case-insensitive by default with a flag for case-sensitive mode"),
        ("assistant", "Updated the search function with case-insensitive matching by default. Added --case-sensitive flag."),
        ("user", "The search is too slow on large directories, can you optimize it?"),
        ("assistant", "I've parallelized the search using rayon and added early termination for better performance."),
    ], now - timedelta(hours=2))

    write_session(out_dir, "blog-engine", "c3d4e5f6a7b8", "develop", [
        ("user", "Add markdown rendering with syntax highlighting for code blocks"),
        ("assistant", "I'll use pulldown-cmark for markdown parsing and syntect for syntax highlighting."),
        ("user", "Add support for front matter parsing in YAML format"),
        ("assistant", "Added YAML front matter parsing using serde_yaml. Extracts title, date, tags, description."),
        ("user", "Create a function to generate RSS feed from blog posts"),
        ("assistant", "Implemented RSS 2.0 feed generation. Takes a list of posts and produces valid XML output."),
        ("user", "Fix the date formatting in RSS - it should use RFC 2822"),
        ("assistant", "Fixed! Changed from ISO 8601 to RFC 2822 format using chrono's to_rfc2822() method."),
    ], now - timedelta(hours=5))

    write_session(out_dir, "todo-api", "d4e5f6a7b8c9", "main", [
        ("user", "Set up a REST API with axum for the todo application"),
        ("assistant", "I've set up the axum server with routes for CRUD operations on todos. Using tower for middleware."),
        ("user", "Add SQLite persistence with sqlx"),
        ("assistant", "Added SQLite database with sqlx. Created migrations and implemented the repository pattern."),
        ("user", "Add authentication with JWT tokens"),
        ("assistant", "Implemented JWT authentication middleware using jsonwebtoken crate."),
    ], now - timedelta(hours=8))

    # Branched session — makes the tree view interesting
    write_branched_session(out_dir, "markdown-parser", "e5f6a7b8c9d0", "main",
                           now - timedelta(days=1))

    write_session(out_dir, "http-server", "f6a7b8c9d0e1", "feature/middleware", [
        ("user", "Implement a logging middleware that records request duration and status code"),
        ("assistant", "Created a Tower-compatible logging middleware that captures method, path, status, and duration."),
        ("user", "Add rate limiting middleware with configurable limits per IP"),
        ("assistant", "Implemented token bucket rate limiting per IP address."),
        ("user", "The rate limiter leaks memory for disconnected clients, add cleanup"),
        ("assistant", "Added a background task that periodically cleans up stale entries using TTL-based eviction."),
    ], now - timedelta(days=1, hours=3))

    # Older sessions for variety
    write_session(out_dir, "weather-app", "1122334455aa", "feature/ui", [
        ("user", "Add a terminal UI to display weather forecast using ratatui"),
        ("assistant", "I've created a TUI dashboard showing current weather, 5-day forecast, and a temperature graph."),
        ("user", "Add color coding for temperature ranges"),
        ("assistant", "Added color gradients: blue for cold, cyan for cool, green for mild, yellow warm, red hot."),
    ], now - timedelta(days=2))

    write_session(out_dir, "rust-cli-tool", "2233445566bb", "main", [
        ("user", "Add a function to recursively walk directories and filter by file extension"),
        ("assistant", "Implemented directory walker using walkdir crate with configurable extension filters."),
    ], now - timedelta(days=3))

    write_session(out_dir, "blog-engine", "3344556677cc", "main", [
        ("user", "Set up the project structure with proper module organization"),
        ("assistant", "Created the project with modules for content parsing, rendering, templates, and static serving."),
        ("user", "Add hot-reload for development mode using notify crate"),
        ("assistant", "Added file watcher with notify crate that triggers rebuild on content or template changes."),
    ], now - timedelta(days=4))

    total_files = 0
    total_lines = 0
    for root, _, files in os.walk(out_dir):
        for fname in files:
            p = os.path.join(root, fname)
            total_files += 1
            total_lines += sum(1 for _ in open(p))
    print(f"Generated {total_files} sessions, {total_lines} messages total under {out_dir}")


if __name__ == "__main__":
    main()
