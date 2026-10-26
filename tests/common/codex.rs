//! Codex rollouts that share history through `session_meta.history_base`.
//! `end_byte_offset` depends on the bytes written, so fixtures are built here
//! instead of kept as static files.

use std::path::{Path, PathBuf};

pub const ROOT: &str = "01a00000-0000-7000-8000-000000000001";
pub const REVERT: &str = "01a00000-0000-7000-8000-000000000002";
pub const FORK1: &str = "01a00000-0000-7000-8000-000000000003";
pub const FORK2: &str = "01a00000-0000-7000-8000-000000000004";

/// `session_meta` line, optionally with a `history_base` of
/// `(rollout id, end_ordinal_exclusive, end_byte_offset)`.
pub fn meta(id: &str, base: Option<(&str, u64, u64)>) -> String {
    let mut payload = serde_json::json!({
        "id": id,
        "session_id": id,
        "timestamp": "2026-10-26T10:00:00.000Z",
        "cwd": "/tmp/project",
    });
    if let Some((rollout_id, end_ordinal, end_byte)) = base {
        payload["history_base"] = serde_json::json!({
            "thread_id": rollout_id,
            "end_ordinal_exclusive": end_ordinal,
            "end_byte_offset": end_byte,
        });
    }
    serde_json::json!({
        "timestamp": "2026-10-26T10:00:00.000Z",
        "type": "session_meta",
        "payload": payload,
    })
    .to_string()
}

/// A `response_item` chat message at `HH:MM:SS` on the fixture day.
pub fn msg_at(time: &str, role: &str, text: &str) -> String {
    let kind = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    serde_json::json!({
        "timestamp": format!("2026-10-26T{time}.000Z"),
        "type": "response_item",
        "payload": {"type": "message", "role": role, "content": [{"type": kind, "text": text}]},
    })
    .to_string()
}

pub fn msg(role: &str, text: &str) -> String {
    msg_at("10:00:01", role, text)
}

/// Write `rollout-<ts>-<thread>[_<rollout>].jsonl` under
/// `<root>/.codex/sessions/2026/10/26/`. Returns its path and the byte length
/// of its first `kept` lines (the `end_byte_offset` a child would use).
pub fn write_rollout(
    root: &Path,
    ts: &str,
    thread: &str,
    rollout: Option<&str>,
    lines: &[String],
    kept: usize,
) -> (PathBuf, u64) {
    let dir = root.join(".codex/sessions/2026/10/26");
    std::fs::create_dir_all(&dir).unwrap();
    let name = match rollout {
        Some(r) => format!("rollout-{ts}-{thread}_{r}.jsonl"),
        None => format!("rollout-{ts}-{thread}.jsonl"),
    };
    let path = dir.join(name);
    let kept_text: String = lines[..kept].iter().map(|l| format!("{l}\n")).collect();
    let rest: String = lines[kept..].iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&path, format!("{kept_text}{rest}")).unwrap();
    (path, kept_text.len() as u64)
}

/// Root thread with 2 kept messages and 1 discarded by a revert, plus the
/// revert continuation with 2 own messages: `(base, continuation)`.
pub fn revert_pair(root: &Path) -> (PathBuf, PathBuf) {
    let (base, end) = write_rollout(
        root,
        "2026-10-26T10-00-00",
        ROOT,
        None,
        &[
            meta(ROOT, None),
            msg_at("10:00:01", "user", "root question"),
            msg_at("10:00:02", "assistant", "root answer"),
            msg_at("10:00:03", "user", "discarded by revert"),
        ],
        3,
    );
    let (cont, _) = write_rollout(
        root,
        "2026-10-26T11-00-00",
        ROOT,
        Some(REVERT),
        &[
            meta(ROOT, Some((ROOT, 3, end))),
            msg_at("11:00:01", "user", "after revert question"),
            msg_at("11:00:02", "assistant", "after revert answer"),
        ],
        3,
    );
    (base, cont)
}

/// Root (1 message) <- FORK1 (1 own) <- FORK2 (1 own): `(root, fork1, fork2)`.
pub fn fork_chain(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let (root_path, root_end) = write_rollout(
        root,
        "2026-10-26T10-00-00",
        ROOT,
        None,
        &[meta(ROOT, None), msg_at("10:00:01", "user", "root prompt")],
        2,
    );
    let (fork1, fork1_end) = write_rollout(
        root,
        "2026-10-26T11-00-00",
        FORK1,
        None,
        &[
            meta(FORK1, Some((ROOT, 2, root_end))),
            msg_at("11:00:01", "user", "fork one prompt"),
        ],
        2,
    );
    let (fork2, _) = write_rollout(
        root,
        "2026-10-26T12-00-00",
        FORK2,
        None,
        &[
            meta(FORK2, Some((FORK1, 4, fork1_end))),
            msg_at("12:00:01", "user", "fork two prompt"),
        ],
        2,
    );
    (root_path, fork1, fork2)
}
