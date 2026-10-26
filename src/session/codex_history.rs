//! Codex rollout history spread over several files.
//!
//! A Codex rollout may start with `session_meta.payload.history_base`: instead
//! of copying earlier records it points at a prefix of another rollout file
//! (`thread/revert` keeps the thread id and starts `<thread>_<rollout>.jsonl`;
//! a fork starts a new thread). This module is the only place that knows about
//! rollout file names, `history_base`, `codex_home` and which file of a thread
//! is current. Everything else reads a session through [`history_lines`]: for a
//! non-Codex file, or a Codex file without `history_base`, that is just the
//! file's own lines.

use chrono::NaiveDateTime;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Longest `history_base` chain followed before giving up.
const MAX_HISTORY_DEPTH: usize = 64;

/// A rollout file name: `rollout-<YYYY-MM-DDThh-mm-ss>-<thread>[_<rollout>].jsonl`.
/// Without the `_<rollout>` suffix the rollout id is the thread id, as in Codex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutName {
    pub timestamp: NaiveDateTime,
    pub thread_id: String,
    pub rollout_id: String,
}

impl RolloutName {
    pub fn parse(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        let rest = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
        if rest.len() <= 20 || rest.as_bytes().get(19) != Some(&b'-') {
            return None;
        }
        let timestamp = NaiveDateTime::parse_from_str(&rest[..19], "%Y-%m-%dT%H-%M-%S").ok()?;
        let id = &rest[20..];
        let (thread_id, rollout_id) = match id.split_once('_') {
            Some((thread, rollout)) if !thread.is_empty() && !rollout.is_empty() => {
                (thread, rollout)
            }
            Some(_) => return None,
            None if id.is_empty() => return None,
            None => (id, id),
        };
        Some(RolloutName {
            timestamp,
            thread_id: thread_id.to_string(),
            rollout_id: rollout_id.to_string(),
        })
    }

    /// Newer rollouts sort higher: name timestamp first, then the rollout id
    /// (UUIDv7) for files created within the same second, as Codex does.
    pub fn order_key(&self) -> (NaiveDateTime, &str) {
        (self.timestamp, &self.rollout_id)
    }
}

/// The Codex home of a rollout: the path up to its `sessions` or
/// `archived_sessions` component.
pub fn codex_home(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| super::CODEX_SESSION_SUBDIRS.contains(&n))
        })?
        .parent()
        .map(Path::to_path_buf)
}

/// A piece of history: a whole file (`end_byte: None`) or its prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub path: PathBuf,
    pub end_byte: Option<u64>,
}

/// One line of history with its physical address (`line_number` is 1-based in
/// its own file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLine {
    pub path: Arc<Path>,
    pub line_number: usize,
    pub raw: String,
}

/// Rollout file names of one Codex home (`sessions` + `archived_sessions`).
/// Built by one walk per operation and never kept between operations: a cached
/// index would keep returning the old current file after a new revert.
#[derive(Debug, Default)]
pub struct RolloutIndex {
    by_rollout: HashMap<String, PathBuf>,
    /// Each file of a thread with its size: an empty file is a rollout Codex
    /// has created but not yet written its `session_meta` to.
    by_thread: HashMap<String, Vec<(RolloutName, PathBuf, u64)>>,
}

impl RolloutIndex {
    pub fn scan(codex_home: &Path) -> Self {
        fn walk(dir: &Path, index: &mut RolloutIndex) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if file_type.is_dir() {
                    walk(&path, index);
                } else if file_type.is_file() {
                    let Some(name) = RolloutName::parse(&path) else {
                        continue;
                    };
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    // `sessions` is walked first, so like Codex it wins over the archive.
                    index
                        .by_rollout
                        .entry(name.rollout_id.clone())
                        .or_insert_with(|| path.clone());
                    index
                        .by_thread
                        .entry(name.thread_id.clone())
                        .or_default()
                        .push((name, path, len));
                }
            }
        }

        let mut index = RolloutIndex::default();
        for subdir in super::CODEX_SESSION_SUBDIRS {
            walk(&codex_home.join(subdir), &mut index);
        }
        index
    }

    /// The file whose name carries `rollout_id`.
    pub fn rollout_path(&self, rollout_id: &str) -> Option<&Path> {
        self.by_rollout.get(rollout_id).map(PathBuf::as_path)
    }

    /// The current file of `thread_id` (see [`pick_current`]).
    pub fn current_rollout_for_thread(&self, thread_id: &str) -> Option<PathBuf> {
        pick_current(
            self.by_thread
                .get(thread_id)?
                .iter()
                .map(|(name, path, len)| (name, path.as_path(), *len)),
        )
    }
}

/// The rule for which rollout of a thread is current: the non-empty file with
/// the highest [`RolloutName::order_key`]. Every lookup goes through it.
fn pick_current<'a>(
    candidates: impl Iterator<Item = (&'a RolloutName, &'a Path, u64)>,
) -> Option<PathBuf> {
    candidates
        .filter(|(_, _, len)| *len > 0)
        .max_by(|(a, _, _), (b, _, _)| a.order_key().cmp(&b.order_key()))
        .map(|(_, path, _)| path.to_path_buf())
}

/// The current rollout of `thread_id` among the given files, for lookups that
/// already hold a list of files instead of a Codex home to index.
pub fn current_rollout_among(
    paths: impl IntoIterator<Item = PathBuf>,
    thread_id: &str,
) -> Option<PathBuf> {
    let candidates: Vec<(RolloutName, PathBuf, u64)> = paths
        .into_iter()
        .filter_map(|path| {
            let name = RolloutName::parse(&path).filter(|n| n.thread_id == thread_id)?;
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            Some((name, path, len))
        })
        .collect();
    pick_current(
        candidates
            .iter()
            .map(|(name, path, len)| (name, path.as_path(), *len)),
    )
}

/// Rollout indexes for one operation, one per Codex home, built on first use.
#[derive(Debug, Default)]
pub struct Rollouts {
    indexes: HashMap<PathBuf, RolloutIndex>,
}

impl Rollouts {
    pub fn index(&mut self, codex_home: &Path) -> &RolloutIndex {
        self.indexes
            .entry(codex_home.to_path_buf())
            .or_insert_with(|| RolloutIndex::scan(codex_home))
    }

    /// The current rollout of the thread `path` belongs to; `None` when `path`
    /// is not a rollout under a Codex home.
    pub fn current_rollout(&mut self, path: &Path) -> Option<PathBuf> {
        let name = RolloutName::parse(path)?;
        let home = codex_home(path)?;
        self.index(&home)
            .current_rollout_for_thread(&name.thread_id)
    }

    /// History segments from the root to `path` itself. The last segment is
    /// `path` as passed. A missing or short base file or a cycle ends the chain
    /// at what was collected.
    pub fn segments(&mut self, path: &Path) -> Vec<Segment> {
        let mut segments = vec![Segment {
            path: path.to_path_buf(),
            end_byte: None,
        }];
        let (Some(name), Some(home)) = (RolloutName::parse(path), codex_home(path)) else {
            return segments;
        };
        let mut visited = HashSet::from([name.rollout_id]);
        let mut current = path.to_path_buf();
        for _ in 0..MAX_HISTORY_DEPTH {
            let Some((base_id, end_byte)) = read_history_base(&current) else {
                break;
            };
            if !visited.insert(base_id.clone()) {
                break;
            }
            let Some(base_path) = self
                .index(&home)
                .rollout_path(&base_id)
                .map(Path::to_path_buf)
            else {
                break;
            };
            if !std::fs::metadata(&base_path).is_ok_and(|m| m.len() >= end_byte) {
                break;
            }
            segments.push(Segment {
                path: base_path.clone(),
                end_byte: Some(end_byte),
            });
            current = base_path;
        }
        segments.reverse();
        segments
    }
}

/// [`Rollouts::segments`] with indexes built only if `path` has a `history_base`.
pub fn resolve_segments(path: &Path) -> Vec<Segment> {
    Rollouts::default().segments(path)
}

/// All lines of the history of `path`, root first.
pub fn history_lines(path: &Path) -> impl Iterator<Item = HistoryLine> {
    lines_of_segments(resolve_segments(path))
}

/// Lines of the given segments, in order.
pub fn lines_of_segments(segments: Vec<Segment>) -> impl Iterator<Item = HistoryLine> {
    segments.into_iter().flat_map(|segment| {
        let path: Arc<Path> = Arc::from(segment.path.as_path());
        let reader: Option<Box<dyn BufRead>> =
            File::open(&segment.path)
                .ok()
                .map(|file| match segment.end_byte {
                    Some(end) => Box::new(BufReader::new(file.take(end))) as Box<dyn BufRead>,
                    None => Box::new(BufReader::new(file)),
                });
        reader.into_iter().flat_map(move |reader| {
            let path = path.clone();
            reader
                .lines()
                .map_while(Result::ok)
                .enumerate()
                .map(move |(i, raw)| HistoryLine {
                    path: path.clone(),
                    line_number: i + 1,
                    raw,
                })
        })
    })
}

/// `(rollout id, end_byte_offset)` of the `history_base` in the first record
/// of a rollout, when it has one.
fn read_history_base(path: &Path) -> Option<(String, u64)> {
    let mut first = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut first)
        .ok()?;
    let json: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    if json.get("type").and_then(|v| v.as_str()) != Some("session_meta") {
        return None;
    }
    let base = json.get("payload")?.get("history_base")?;
    Some((
        base.get("thread_id")?.as_str()?.to_string(),
        base.get("end_byte_offset")?.as_u64()?,
    ))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    pub const DAY: &str = "2026/10/26";

    /// `session_meta` line of a rollout, optionally with a `history_base`.
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

    /// A `response_item` chat message line.
    pub fn msg(role: &str, text: &str) -> String {
        let kind = if role == "user" {
            "input_text"
        } else {
            "output_text"
        };
        serde_json::json!({
            "timestamp": "2026-10-26T10:00:01.000Z",
            "type": "response_item",
            "payload": {"type": "message", "role": role, "content": [{"type": kind, "text": text}]},
        })
        .to_string()
    }

    /// Write `rollout-<ts>-<thread>[_<rollout>].jsonl` under
    /// `<root>/.codex/<subdir>/<DAY>/`. Returns its path and the byte length
    /// after the first `kept` lines (the `end_byte_offset` a child would use).
    pub fn write_rollout(
        root: &Path,
        subdir: &str,
        ts: &str,
        thread: &str,
        rollout: Option<&str>,
        lines: &[String],
        kept: usize,
    ) -> (PathBuf, u64) {
        let dir = root.join(".codex").join(subdir).join(DAY);
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
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    const ROOT: &str = "01a00000-0000-7000-8000-000000000001";
    const REVERT: &str = "01a00000-0000-7000-8000-000000000002";
    const FORK1: &str = "01a00000-0000-7000-8000-000000000003";
    const FORK2: &str = "01a00000-0000-7000-8000-000000000004";

    fn texts(path: &Path) -> Vec<String> {
        history_lines(path)
            .filter_map(|l| {
                let v: serde_json::Value = serde_json::from_str(&l.raw).ok()?;
                v["payload"]["content"][0]["text"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect()
    }

    /// Root with 2 kept + 1 discarded message, and a revert continuation of it.
    fn revert_pair(root: &Path) -> (PathBuf, PathBuf) {
        let lines = vec![
            meta(ROOT, None),
            msg("user", "root q1"),
            msg("assistant", "root a1"),
            msg("user", "discarded"),
        ];
        let (base, end) = write_rollout(
            root,
            "sessions",
            "2026-10-26T10-00-00",
            ROOT,
            None,
            &lines,
            3,
        );
        let (cont, _) = write_rollout(
            root,
            "sessions",
            "2026-10-26T11-00-00",
            ROOT,
            Some(REVERT),
            &[
                meta(ROOT, Some((ROOT, 3, end))),
                msg("user", "after revert"),
            ],
            2,
        );
        (base, cont)
    }

    #[test]
    fn rollout_name_when_plain_then_rollout_id_is_thread_id() {
        let name = RolloutName::parse(Path::new(&format!(
            "/x/.codex/sessions/2026/10/26/rollout-2026-10-26T18-35-27-{ROOT}.jsonl"
        )))
        .unwrap();
        assert_eq!(name.thread_id, ROOT);
        assert_eq!(name.rollout_id, ROOT);
        assert_eq!(
            name.timestamp.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "2026-10-26T18:35:27"
        );
    }

    #[test]
    fn rollout_name_when_revert_suffix_then_splits_thread_and_rollout() {
        let name = RolloutName::parse(Path::new(&format!(
            "rollout-2026-10-26T19-38-03-{ROOT}_{REVERT}.jsonl"
        )))
        .unwrap();
        assert_eq!(name.thread_id, ROOT);
        assert_eq!(name.rollout_id, REVERT);
    }

    #[test]
    fn rollout_name_when_not_a_rollout_then_none() {
        assert!(RolloutName::parse(Path::new("/x/.claude/projects/p/abc.jsonl")).is_none());
        assert!(RolloutName::parse(Path::new("rollout-garbage.jsonl")).is_none());
        assert!(RolloutName::parse(Path::new(&format!(
            "rollout-2026-10-26T18-35-27-{ROOT}.txt"
        )))
        .is_none());
        assert!(RolloutName::parse(Path::new("rollout-2026-10-26T18-35-27-.jsonl")).is_none());
    }

    #[test]
    fn codex_home_when_under_sessions_or_archive_then_common_root() {
        assert_eq!(
            codex_home(Path::new("/h/.codex/sessions/2026/10/26/r.jsonl")),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(
            codex_home(Path::new("/h/.codex/archived_sessions/r.jsonl")),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(codex_home(Path::new("/h/projects/r.jsonl")), None);
    }

    #[test]
    fn segments_when_no_history_base_then_single_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = revert_pair(dir.path());
        assert_eq!(
            resolve_segments(&base),
            vec![Segment {
                path: base.clone(),
                end_byte: None
            }]
        );
    }

    #[test]
    fn segments_when_revert_then_base_prefix_and_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let (base, cont) = revert_pair(dir.path());
        let segs = resolve_segments(&cont);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].path, base);
        assert!(segs[0].end_byte.is_some());
        assert_eq!(
            segs[1],
            Segment {
                path: cont,
                end_byte: None
            }
        );
    }

    #[test]
    fn history_lines_when_revert_then_discarded_tail_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let (_, cont) = revert_pair(dir.path());
        assert_eq!(texts(&cont), vec!["root q1", "root a1", "after revert"]);
    }

    #[test]
    fn history_lines_when_stitched_then_line_numbers_are_physical() {
        let dir = tempfile::tempdir().unwrap();
        let (base, cont) = revert_pair(dir.path());
        let lines: Vec<HistoryLine> = history_lines(&cont).collect();
        let addr: Vec<(PathBuf, usize)> = lines
            .iter()
            .map(|l| (l.path.to_path_buf(), l.line_number))
            .collect();
        assert_eq!(
            addr,
            vec![
                (base.clone(), 1),
                (base.clone(), 2),
                (base, 3),
                (cont.clone(), 1),
                (cont, 2)
            ]
        );
    }

    #[test]
    fn history_lines_when_fork_chain_then_all_ancestors_from_root() {
        let dir = tempfile::tempdir().unwrap();
        let root_lines = vec![meta(ROOT, None), msg("user", "r1"), msg("user", "r2")];
        let (_, root_end) = write_rollout(
            dir.path(),
            "archived_sessions",
            "2026-10-26T10-00-00",
            ROOT,
            None,
            &root_lines,
            2,
        );
        let f1_lines = vec![meta(FORK1, Some((ROOT, 2, root_end))), msg("user", "f1")];
        let (_, f1_end) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T11-00-00",
            FORK1,
            None,
            &f1_lines,
            2,
        );
        let (f2, _) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T12-00-00",
            FORK2,
            None,
            &[meta(FORK2, Some((FORK1, 4, f1_end))), msg("user", "f2")],
            2,
        );
        assert_eq!(resolve_segments(&f2).len(), 3);
        assert_eq!(texts(&f2), vec!["r1", "f1", "f2"]);
    }

    #[test]
    fn segments_when_base_missing_then_own_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let (cont, _) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T11-00-00",
            FORK1,
            None,
            &[meta(FORK1, Some((ROOT, 3, 100))), msg("user", "own")],
            2,
        );
        assert_eq!(resolve_segments(&cont).len(), 1);
        assert_eq!(texts(&cont), vec!["own"]);
    }

    #[test]
    fn segments_when_base_shorter_than_offset_then_own_file_only() {
        let dir = tempfile::tempdir().unwrap();
        write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T10-00-00",
            ROOT,
            None,
            &[meta(ROOT, None)],
            1,
        );
        let (cont, _) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T11-00-00",
            FORK1,
            None,
            &[meta(FORK1, Some((ROOT, 3, 1_000_000))), msg("user", "own")],
            2,
        );
        assert_eq!(resolve_segments(&cont).len(), 1);
    }

    #[test]
    fn segments_when_cycle_then_each_file_once() {
        let dir = tempfile::tempdir().unwrap();
        // FORK1 -> FORK2 -> FORK1: offsets cover the meta line only.
        let m1 = meta(FORK1, Some((FORK2, 1, 0)));
        let (f1, _) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T11-00-00",
            FORK1,
            None,
            &[m1, msg("user", "f1")],
            2,
        );
        let m2 = meta(FORK2, Some((FORK1, 1, 0)));
        write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T12-00-00",
            FORK2,
            None,
            &[m2, msg("user", "f2")],
            2,
        );
        let segs = resolve_segments(&f1);
        let paths: HashSet<&PathBuf> = segs.iter().map(|s| &s.path).collect();
        assert_eq!(paths.len(), segs.len());
        assert!(segs.len() <= 2);
    }

    #[test]
    fn current_rollout_when_revert_then_newer_name_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (base, cont) = revert_pair(dir.path());
        let index = RolloutIndex::scan(&dir.path().join(".codex"));
        assert_eq!(index.current_rollout_for_thread(ROOT), Some(cont.clone()));
        // Fresh mtime on the old file does not matter: only the name counts.
        filetime::set_file_mtime(&base, filetime::FileTime::now()).unwrap();
        let index = RolloutIndex::scan(&dir.path().join(".codex"));
        assert_eq!(index.current_rollout_for_thread(ROOT), Some(cont));
    }

    #[test]
    fn current_rollout_when_same_second_then_larger_rollout_id_wins() {
        let dir = tempfile::tempdir().unwrap();
        let ts = "2026-10-26T11-00-00";
        let lines = [meta(ROOT, None)];
        write_rollout(dir.path(), "sessions", ts, ROOT, Some(REVERT), &lines, 1);
        let (later, _) = write_rollout(dir.path(), "sessions", ts, ROOT, Some(FORK2), &lines, 1);
        let index = RolloutIndex::scan(&dir.path().join(".codex"));
        assert_eq!(index.current_rollout_for_thread(ROOT), Some(later));
    }

    #[test]
    fn current_rollout_when_newest_file_empty_then_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = revert_pair(dir.path());
        let (cont, _) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T12-00-00",
            ROOT,
            Some(FORK1),
            &[],
            0,
        );
        assert_eq!(std::fs::metadata(&cont).unwrap().len(), 0);
        let index = RolloutIndex::scan(&dir.path().join(".codex"));
        let current = index.current_rollout_for_thread(ROOT).unwrap();
        assert_ne!(current, cont);
        assert_ne!(current, base);
    }

    #[test]
    fn current_rollout_when_thread_unknown_then_none() {
        let dir = tempfile::tempdir().unwrap();
        revert_pair(dir.path());
        let index = RolloutIndex::scan(&dir.path().join(".codex"));
        assert_eq!(index.current_rollout_for_thread(FORK1), None);
    }

    #[test]
    fn rollouts_when_new_revert_appears_then_next_operation_sees_it() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T10-00-00",
            ROOT,
            None,
            &[meta(ROOT, None)],
            1,
        );
        assert_eq!(
            Rollouts::default().current_rollout(&base),
            Some(base.clone())
        );
        let (cont, _) = write_rollout(
            dir.path(),
            "sessions",
            "2026-10-26T11-00-00",
            ROOT,
            Some(REVERT),
            &[meta(ROOT, None)],
            1,
        );
        assert_eq!(Rollouts::default().current_rollout(&base), Some(cont));
    }

    #[test]
    fn current_rollout_when_outside_codex_home_then_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(format!("rollout-2026-10-26T10-00-00-{ROOT}.jsonl"));
        std::fs::write(&path, meta(ROOT, None)).unwrap();
        assert_eq!(Rollouts::default().current_rollout(&path), None);
    }
}
