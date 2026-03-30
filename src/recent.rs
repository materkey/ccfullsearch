use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::search::extract_project_from_path;
use crate::session::{self, SessionSource};

const HEAD_SCAN_LINES: usize = 30;

/// A recently accessed Claude session with summary metadata.
#[derive(Debug, Clone)]
pub struct RecentSession {
    pub session_id: String,
    pub file_path: String,
    pub project: String,
    pub source: SessionSource,
    pub timestamp: DateTime<Utc>,
    pub summary: String,
    pub automation: Option<String>,
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate_summary(s: &str, max_len: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_len {
        trimmed.to_string()
    } else {
        let truncated: String = trimmed.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// Extract text content from a message's content field.
/// Handles both array format [{"type":"text","text":"..."}] and plain string.
fn extract_text_content(content: &serde_json::Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
        return None;
    }

    if let Some(arr) = content.as_array() {
        let mut parts: Vec<String> = Vec::new();
        for item in arr {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if item_type == "text" {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed.to_string());
                    }
                }
            }
        }
        if !parts.is_empty() {
            return Some(parts.join(" "));
        }
    }

    None
}

fn extract_non_meta_user_text(json: &serde_json::Value) -> Option<String> {
    if session::extract_record_type(json) != Some("user") {
        return None;
    }

    if json
        .get("isMeta")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return None;
    }

    let message = json.get("message")?;
    let content = message.get("content")?;
    extract_text_content(content)
}

fn is_real_user_prompt(text: &str) -> bool {
    !text.starts_with("<system-reminder>")
}

#[derive(Default)]
struct HeadScan {
    lines_scanned: usize,
    session_id: Option<String>,
    first_user_message: Option<String>,
    last_summary: Option<String>,
    last_summary_sid: Option<String>,
    automation: Option<String>,
    saw_off_chain_summary: bool,
}

fn build_latest_chain(path: &Path) -> Option<HashSet<String>> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut parents: HashMap<String, Option<String>> = HashMap::new();
    let mut last_uuid: Option<String> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };

        if !line.contains("\"uuid\"") {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session::is_synthetic_linear_record(&json) {
            continue;
        }

        let Some(uuid) = session::extract_uuid(&json) else {
            continue;
        };
        parents.insert(uuid.clone(), session::extract_parent_uuid(&json));
        last_uuid = Some(uuid);
    }

    let mut chain = HashSet::new();
    let mut current = last_uuid?;
    loop {
        if !chain.insert(current.clone()) {
            break;
        }
        let Some(parent_uuid) = parents.get(&current).cloned().flatten() else {
            break;
        };
        current = parent_uuid;
    }

    Some(chain)
}

fn summary_is_on_latest_chain(
    json: &serde_json::Value,
    latest_chain: Option<&HashSet<String>>,
) -> bool {
    let Some(latest_chain) = latest_chain else {
        return true;
    };
    let Some(leaf_uuid) = session::extract_leaf_uuid(json) else {
        return true;
    };
    latest_chain.contains(&leaf_uuid)
}

fn scan_head_with_chain(
    path: &Path,
    max_lines: usize,
    latest_chain: Option<&HashSet<String>>,
) -> Option<HeadScan> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut scan = HeadScan::default();

    for (i, line) in reader.lines().enumerate() {
        if i >= max_lines {
            break;
        }
        scan.lines_scanned = i + 1;

        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session::is_synthetic_linear_record(&json) {
            continue;
        }

        if scan.session_id.is_none() {
            scan.session_id = session::extract_session_id(&json);
        }

        if session::extract_record_type(&json) == Some("summary") {
            if let Some(summary_text) = json.get("summary").and_then(|v| v.as_str()) {
                let trimmed = summary_text.trim();
                if !trimmed.is_empty() {
                    if summary_is_on_latest_chain(&json, latest_chain) {
                        scan.last_summary = Some(truncate_summary(trimmed, 100));
                        scan.last_summary_sid = session::extract_session_id(&json);
                    } else {
                        scan.saw_off_chain_summary = true;
                    }
                }
            }
        }

        if let Some(text) = extract_non_meta_user_text(&json) {
            if scan.first_user_message.is_none() && is_real_user_prompt(&text) {
                scan.automation = session::detect_automation(&text).map(|s| s.to_string());
                scan.first_user_message = Some(truncate_summary(&text, 100));
            }
        }

        if scan.first_user_message.is_some()
            && scan.session_id.is_some()
            && scan.last_summary.is_some()
        {
            break;
        }
    }

    Some(scan)
}

#[cfg(test)]
fn scan_head(path: &Path, max_lines: usize) -> Option<HeadScan> {
    scan_head_with_chain(path, max_lines, None)
}

#[derive(Default)]
struct TailSummaryScan {
    summary: Option<(Option<String>, String)>,
    saw_off_chain_summary: bool,
}

/// Read the last `max_bytes` of a file and search for the last `type=summary` record.
/// Compaction summaries are appended during context compaction, so they appear near
/// the end of long session files. Returns (session_id, summary_text) if found.
///
/// Reads into a byte buffer and skips to the first newline after the seek offset
/// to avoid splitting multibyte UTF-8 characters or partial JSONL lines.
fn find_summary_from_tail_with_chain(
    path: &Path,
    max_bytes: u64,
    latest_chain: Option<&HashSet<String>>,
) -> Option<TailSummaryScan> {
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let start = file_len.saturating_sub(max_bytes);

    // When seeking into the middle, start one byte earlier so we can check
    // whether the seek position falls on a line boundary without reopening the file.
    let read_start = if start > 0 { start - 1 } else { 0 };
    if read_start > 0 {
        file.seek(SeekFrom::Start(read_start)).ok()?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;

    // If we seeked into the middle of the file, skip to the first complete line
    // to avoid partial lines and mid-UTF-8 character issues.
    // The buffer includes one extra byte before `start` (when start > 0) so we
    // can check for a line boundary without a second file open.
    let data = if start > 0 {
        // buf[0] is the byte at read_start (= start - 1). The actual tail starts at buf[1..].
        let at_line_boundary = buf[0] == b'\n';
        let tail_buf = &buf[1..];
        if at_line_boundary {
            tail_buf
        } else if tail_buf.first() == Some(&b'\n') {
            &tail_buf[1..]
        } else if let Some(pos) = tail_buf.iter().position(|&b| b == b'\n') {
            &tail_buf[pos + 1..]
        } else {
            return None;
        }
    } else {
        &buf
    };
    let tail = String::from_utf8_lossy(data);

    // Find the last summary record in the tail, and track any sessionId from any record
    // so we have a fallback if the summary record itself lacks a sessionId.
    let mut last_summary: Option<(Option<String>, String)> = None;
    let mut any_sid: Option<String> = None;
    let mut saw_off_chain_summary = false;
    for line in tail.lines() {
        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if session::is_synthetic_linear_record(&json) {
            continue;
        }
        if any_sid.is_none() {
            any_sid = session::extract_session_id(&json);
        }
        if session::extract_record_type(&json) == Some("summary") {
            if let Some(summary_text) = json.get("summary").and_then(|v| v.as_str()) {
                let trimmed = summary_text.trim();
                if !trimmed.is_empty() {
                    if summary_is_on_latest_chain(&json, latest_chain) {
                        let sid = session::extract_session_id(&json);
                        last_summary = Some((sid, truncate_summary(trimmed, 100)));
                    } else {
                        saw_off_chain_summary = true;
                    }
                }
            }
        }
    }

    Some(TailSummaryScan {
        summary: last_summary.map(|(sid, text)| (sid.or(any_sid), text)),
        saw_off_chain_summary,
    })
}

#[cfg(test)]
fn find_summary_from_tail(path: &Path, max_bytes: u64) -> Option<(Option<String>, String)> {
    find_summary_from_tail_with_chain(path, max_bytes, None)?.summary
}

fn extract_latest_user_message_on_chain(
    path: &Path,
    latest_chain: &HashSet<String>,
) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut latest_user_message: Option<String> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };

        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session::is_synthetic_linear_record(&json)
            || session::extract_record_type(&json) != Some("user")
        {
            continue;
        }

        let is_meta = json
            .get("isMeta")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_meta {
            continue;
        }

        let Some(uuid) = session::extract_uuid(&json) else {
            continue;
        };
        if !latest_chain.contains(&uuid) {
            continue;
        }

        let Some(message) = json.get("message") else {
            continue;
        };
        let Some(content) = message.get("content") else {
            continue;
        };
        let Some(text) = extract_text_content(content) else {
            continue;
        };
        if text.starts_with("<system-reminder>") {
            continue;
        }

        latest_user_message = Some(truncate_summary(&text, 100));
    }

    latest_user_message
}

pub(crate) fn detect_session_automation(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };

        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session::is_synthetic_linear_record(&json) {
            continue;
        }

        let Some(text) = extract_non_meta_user_text(&json) else {
            continue;
        };

        if is_real_user_prompt(&text) {
            return session::detect_automation(&text).map(|s| s.to_string());
        }
    }

    None
}

/// Extract a `RecentSession` from a JSONL session file.
///
/// Priority for summary:
/// 1. `type=summary` record -> use `.summary` field (scans file tail, head, then middle)
/// 2. First `type=user` where `isMeta` is not true -> extract text content
///
/// Uses a three-pass approach:
/// 1. Check the last 256KB for summary records (compaction summaries at file end)
/// 2. Scan first 30 lines for session_id, first user message, and summary records
/// 3. Scan the remaining lines for any missing summary/session-id/first-user metadata
///
/// Uses file mtime as timestamp for accurate recency sorting.
pub fn extract_summary(path: &Path) -> Option<RecentSession> {
    let path_str = path.to_str().unwrap_or("");
    let source = SessionSource::from_path(path_str);
    let project = extract_project_from_path(path_str);
    const TAIL_BYTES: u64 = 256 * 1024;
    let latest_chain = build_latest_chain(path);

    // Use file mtime as timestamp — more accurate for recency than first JSONL record
    let mtime = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let mtime_timestamp: DateTime<Utc> = mtime.into();
    let head_scan = scan_head_with_chain(path, HEAD_SCAN_LINES, latest_chain.as_ref())?;

    // Calculate where the tail scan started (in bytes) so pass 3 knows when to stop
    let file_len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let tail_start = file_len.saturating_sub(TAIL_BYTES);
    let tail_scan = find_summary_from_tail_with_chain(path, TAIL_BYTES, latest_chain.as_ref())
        .unwrap_or_default();
    let tail_summary = tail_scan.summary;

    // Pass 2: reuse the head scan, then let the tail summary override it when present.
    let mut session_id = head_scan.session_id.clone();
    let mut first_user_message = head_scan.first_user_message.clone();
    let mut last_summary = head_scan.last_summary.clone();
    let mut last_summary_sid = head_scan.last_summary_sid.clone();
    let mut automation = head_scan.automation.clone();
    let mut saw_off_chain_summary =
        head_scan.saw_off_chain_summary || tail_scan.saw_off_chain_summary;
    let lines_scanned = head_scan.lines_scanned;

    if let Some((tail_sid, summary_text)) = tail_summary {
        session_id = tail_sid.clone().or(session_id);
        last_summary = Some(summary_text);
        last_summary_sid = tail_sid;
    }

    // Pass 3: scan the middle region for any metadata the head/tail passes still
    // could not find. This handles three cases:
    // - Compaction wrote a summary early, then many messages pushed it out of both windows
    // - The file starts with >30 non-message records (e.g., file-history-snapshot) so the
    //   first user message was beyond the head scan
    // - Automation markers or session IDs only appear in the unscanned middle of a large file
    //
    let need_summary = last_summary.is_none();
    let need_user_msg = first_user_message.is_none();
    let need_sid = last_summary_sid.is_none() && session_id.is_none();
    let need_automation = automation.is_none() && first_user_message.is_none();
    // For summaries, only scan the middle if tail_start > 0 (otherwise tail covered the whole
    // file). For the first user prompt, session_id, and automation, always scan beyond the head
    // since the tail pass never looks for them.
    let should_scan_middle =
        (need_summary && tail_start > 0) || need_user_msg || need_sid || need_automation;
    if should_scan_middle && lines_scanned >= HEAD_SCAN_LINES {
        let file = File::open(path).ok()?;
        let reader = BufReader::new(file);
        let mut bytes_read: u64 = 0;

        for (i, line) in reader.lines().enumerate() {
            // Skip lines already covered by pass 2
            if i < lines_scanned {
                if let Ok(ref l) = line {
                    bytes_read += l.len() as u64 + 1; // +1 for newline
                }
                continue;
            }

            // For summary scanning, stop before the tail region (already covered by pass 1).
            // For the first real user prompt, continue through the tail since pass 1 never
            // looks for it.
            let in_tail_region = tail_start > 0 && bytes_read >= tail_start;
            let still_need_user_msg = need_user_msg && first_user_message.is_none();
            let still_need_sid = need_sid && session_id.is_none();
            if in_tail_region && !(still_need_user_msg || still_need_sid) {
                break;
            }

            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            bytes_read += line.len() as u64 + 1;

            // Stop once we have everything we need from the middle scan
            let have_summary = !need_summary || last_summary.is_some();
            let have_user_msg = !need_user_msg || first_user_message.is_some();
            let have_sid = !need_sid || session_id.is_some();
            let have_auto = !need_automation || first_user_message.is_some();
            if have_summary && have_user_msg && have_sid && have_auto {
                break;
            }

            let could_be_summary = need_summary && !in_tail_region && line.contains("\"summary\"");
            let could_be_user = still_need_user_msg && line.contains("\"user\"");

            let could_have_sid = still_need_sid
                && (line.contains("\"sessionId\"") || line.contains("\"session_id\""));

            if !could_be_summary && !could_be_user && !could_have_sid {
                continue;
            }

            let json: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if session::is_synthetic_linear_record(&json) {
                continue;
            }

            // Extract session_id from any parsed record if still missing
            if session_id.is_none() {
                session_id = session::extract_session_id(&json);
            }

            if could_be_summary && session::extract_record_type(&json) == Some("summary") {
                if let Some(summary_text) = json.get("summary").and_then(|v| v.as_str()) {
                    let trimmed = summary_text.trim();
                    if !trimmed.is_empty() {
                        if summary_is_on_latest_chain(&json, latest_chain.as_ref()) {
                            last_summary = Some(truncate_summary(trimmed, 100));
                            last_summary_sid = session::extract_session_id(&json);
                        } else {
                            saw_off_chain_summary = true;
                        }
                    }
                }
            }

            if could_be_user {
                if let Some(text) = extract_non_meta_user_text(&json) {
                    if first_user_message.is_none() && is_real_user_prompt(&text) {
                        automation = session::detect_automation(&text).map(|s| s.to_string());
                        first_user_message = Some(truncate_summary(&text, 100));
                    }
                }
            }
        }
    }

    // After pass 3, check if we found a summary in the middle region
    if let Some(summary_text) = last_summary {
        let sid = last_summary_sid.or(session_id)?;
        return Some(RecentSession {
            session_id: sid,
            file_path: path_str.to_string(),
            project,
            source,
            timestamp: mtime_timestamp,
            summary: summary_text,
            automation,
        });
    }

    let session_id = session_id?;
    let summary = if saw_off_chain_summary {
        latest_chain
            .as_ref()
            .and_then(|chain| extract_latest_user_message_on_chain(path, chain))
            .or(first_user_message)
            .unwrap_or_default()
    } else {
        first_user_message.unwrap_or_default()
    };

    if summary.is_empty() {
        return None;
    }

    Some(RecentSession {
        session_id,
        file_path: path_str.to_string(),
        project,
        source,
        timestamp: mtime_timestamp,
        summary,
        automation,
    })
}

/// Read the first few lines to extract session_id.
/// Scans up to 30 lines to handle files that start with non-session records
/// (e.g., file-history-snapshot, summary) before the first record with a session_id.
#[cfg(test)]
fn extract_session_id_from_head(path: &Path) -> Option<String> {
    scan_head(path, HEAD_SCAN_LINES).and_then(|scan| scan.session_id)
}

/// Walk directories and find `*.jsonl` files, skipping `agent-*` files.
fn find_jsonl_files(search_paths: &[String]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for base in search_paths {
        let base_path = Path::new(base);
        if !base_path.is_dir() {
            continue;
        }
        collect_jsonl_recursive(base_path, &mut files);
    }
    files
}

fn collect_jsonl_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip symlinks to avoid infinite loops from cyclic directory structures
            if path.is_symlink() {
                continue;
            }
            collect_jsonl_recursive(&path, files);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".jsonl") && !name.starts_with("agent-") {
                files.push(path);
            }
        }
    }
}

/// Collect recent sessions from search paths.
///
/// Walks directories, finds `*.jsonl` files (skipping `agent-*`),
/// sorts by filesystem mtime descending, extracts summaries in parallel
/// with rayon (filtering out non-session files), and returns the top
/// `limit` results sorted by session timestamp descending.
///
/// When timestamps tie, file path is used as a deterministic fallback so
/// equal-mtime files do not produce unstable ordering across platforms.
pub fn collect_recent_sessions(search_paths: &[String], limit: usize) -> Vec<RecentSession> {
    let files = find_jsonl_files(search_paths);

    // Collect mtime once per file, then sort — avoids O(n log n) repeated stat() calls
    let mut files_with_mtime: Vec<(PathBuf, std::time::SystemTime)> = files
        .into_iter()
        .filter_map(|p| {
            fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| (p, t))
        })
        .collect();
    files_with_mtime.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));

    let files: Vec<PathBuf> = files_with_mtime.into_iter().map(|(p, _)| p).collect();

    // Process files in batches to avoid reading the entire corpus when only `limit`
    // sessions are needed. Non-session JSONL files (metadata, auxiliary) return None
    // from extract_summary(), so we process extra files per batch to compensate.
    // Start with 4x the limit and expand if needed.
    let mut sessions: Vec<RecentSession> = Vec::new();
    let batch_multiplier = 4;
    let mut offset = 0;

    loop {
        let batch_size = if offset == 0 {
            (limit * batch_multiplier).max(limit)
        } else {
            // Subsequent batches: process remaining files
            files.len().saturating_sub(offset)
        };
        let end = (offset + batch_size).min(files.len());
        if offset >= end {
            break;
        }

        let batch = &files[offset..end];
        let batch_sessions: Vec<RecentSession> = batch
            .par_iter()
            .filter_map(|path| extract_summary(path))
            .collect();
        sessions.extend(batch_sessions);
        offset = end;

        // If we have enough sessions, stop processing more files
        if sessions.len() >= limit {
            break;
        }
    }

    // Sort by timestamp descending and apply limit
    sessions.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.file_path.cmp(&a.file_path))
    });
    sessions.truncate(limit);

    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_recent_session_creation() {
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap();
        let session = RecentSession {
            session_id: "sess-linear-001".to_string(),
            file_path: "/Users/user/.claude/projects/-Users-user-myproject/abc.jsonl".to_string(),
            project: "myproject".to_string(),
            source: SessionSource::ClaudeCodeCLI,
            timestamp: ts,
            summary: "How do I sort a list in Python?".to_string(),
            automation: None,
        };
        assert_eq!(session.session_id, "sess-linear-001");
        assert_eq!(session.project, "myproject");
        assert_eq!(session.source, SessionSource::ClaudeCodeCLI);
        assert_eq!(session.timestamp, ts);
        assert_eq!(session.summary, "How do I sort a list in Python?");
    }

    #[test]
    fn test_recent_session_desktop_source() {
        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap();
        let session = RecentSession {
            session_id: "desktop-uuid".to_string(),
            file_path: "/Users/user/Library/Application Support/Claude/local-agent-mode-sessions/uuid1/uuid2/local_session/audit.jsonl".to_string(),
            project: "uuid1".to_string(),
            source: SessionSource::ClaudeDesktop,
            timestamp: ts,
            summary: "Desktop session summary".to_string(),
            automation: None,
        };
        assert_eq!(session.source, SessionSource::ClaudeDesktop);
        assert_eq!(session.project, "uuid1");
    }

    #[test]
    fn test_extract_summary_returns_first_user_message() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"How do I sort a list in Python?"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Use sorted()"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "How do I sort a list in Python?");
        assert_eq!(result.session_id, "sess-001");
    }

    #[test]
    fn test_extract_summary_prefers_summary_record() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"This session discussed Python sorting","sessionId":"sess-001","timestamp":"2025-06-01T09:59:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"How do I sort a list?"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "This session discussed Python sorting");
    }

    #[test]
    fn test_extract_summary_returns_none_for_empty_file() {
        let f = NamedTempFile::new().unwrap();
        let result = extract_summary(f.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_summary_returns_none_for_assistant_only() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = extract_summary(f.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_summary_returns_none_for_synthetic_linear_bootstrap_only() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Synthetic linear resume"}}]}},"sessionId":"sess-synth","timestamp":"2025-06-01T10:00:00Z","ccsSyntheticLinear":true}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"reply"}}]}},"sessionId":"sess-synth","timestamp":"2025-06-01T10:01:00Z","ccsSyntheticLinear":true}}"#).unwrap();

        let result = extract_summary(f.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_summary_keeps_resumed_synthetic_branch_visible() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Synthetic linear resume"}}]}},"sessionId":"sess-synth","timestamp":"2025-06-01T10:00:00Z","ccsSyntheticLinear":true}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"bootstrap reply"}}]}},"sessionId":"sess-synth","timestamp":"2025-06-01T10:01:00Z","ccsSyntheticLinear":true}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Continue working on this branch"}}]}},"sessionId":"sess-synth","timestamp":"2025-06-01T10:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Continuing"}}]}},"sessionId":"sess-synth","timestamp":"2025-06-01T10:03:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.session_id, "sess-synth");
        assert_eq!(result.summary, "Continue working on this branch");
    }

    #[test]
    fn test_extract_summary_truncates_long_messages() {
        let long_msg = "a".repeat(200);
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#, long_msg).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary.len(), 100); // 97 chars + "..."
        assert!(result.summary.ends_with("..."));
    }

    #[test]
    fn test_extract_summary_handles_desktop_format() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"Explain Docker networking"}},"session_id":"desktop-001","_audit_timestamp":"2026-01-13T10:00:00.000Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Explain Docker networking");
        assert_eq!(result.session_id, "desktop-001");
    }

    #[test]
    fn test_extract_summary_skips_meta_messages() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","isMeta":true,"message":{{"role":"user","content":[{{"type":"text","text":"init message"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Real question here"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Real question here");
    }

    #[test]
    fn test_extract_summary_skips_system_reminder_messages() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"<system-reminder>Some system context</system-reminder>"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Real user question"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Real user question");
    }

    #[test]
    fn test_extract_summary_none_when_only_system_reminders() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"<system-reminder>System context only"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = extract_summary(f.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_summary_prefers_summary_after_user_message() {
        // Compaction layout: user message first, then summary record later.
        // The summary record should be preferred over the first user message.
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Start a long conversation"}}]}},"uuid":"c1","sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Sure, ready."}}]}},"uuid":"c2","parentUuid":"c1","sessionId":"sess-001","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"The conversation covered initial setup and greetings.","uuid":"c3","parentUuid":"c2","sessionId":"sess-001","timestamp":"2025-06-01T10:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Continue after compaction"}}]}},"uuid":"c4","parentUuid":"c3","sessionId":"sess-001","timestamp":"2025-06-01T10:03:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(
            result.summary,
            "The conversation covered initial setup and greetings."
        );
    }

    #[test]
    fn test_extract_summary_from_fixture_compaction() {
        let path = Path::new("tests/fixtures/compaction_session.jsonl");
        let result = extract_summary(path).unwrap();
        assert_eq!(
            result.summary,
            "The conversation covered initial setup and greetings."
        );
        assert_eq!(result.session_id, "sess-compact-001");
    }

    #[test]
    fn test_extract_summary_ignores_off_chain_summary_and_uses_live_branch_prompt() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Original prompt"}}]}},"uuid":"u1","sessionId":"sess-branch","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Initial reply"}}]}},"uuid":"a1","parentUuid":"u1","sessionId":"sess-branch","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Abandoned branch prompt"}}]}},"uuid":"u2","parentUuid":"a1","sessionId":"sess-branch","timestamp":"2025-06-01T10:02:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Abandoned branch reply"}}]}},"uuid":"a2","parentUuid":"u2","sessionId":"sess-branch","timestamp":"2025-06-01T10:03:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Stale abandoned branch summary","leafUuid":"a2","uuid":"s1","parentUuid":"a2","sessionId":"sess-branch","timestamp":"2025-06-01T10:04:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Live branch prompt"}}]}},"uuid":"u3","parentUuid":"a1","sessionId":"sess-branch","timestamp":"2025-06-01T10:05:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Live branch reply"}}]}},"uuid":"a3","parentUuid":"u3","sessionId":"sess-branch","timestamp":"2025-06-01T10:06:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.session_id, "sess-branch");
        assert_eq!(result.summary, "Live branch prompt");
    }

    #[test]
    fn test_collect_recent_sessions_not_crowded_by_nonsession_files() {
        // Auxiliary JSONL files (no user messages, no session_id) should not
        // crowd out real sessions from the result when limit is applied.
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("projects").join("-Users-user-proj");
        std::fs::create_dir_all(&proj).unwrap();

        // Create 3 real sessions first (older mtime)
        for i in 0..3 {
            write_test_session(
                &proj,
                &format!("real{}.jsonl", i),
                &format!("sess-{}", i),
                &format!("Real question {}", i),
            );
        }

        // Brief pause so auxiliary files get newer mtime
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Create 5 auxiliary JSONL files (no valid session data) — newer mtime
        for i in 0..5 {
            let aux_path = proj.join(format!("aux{}.jsonl", i));
            let mut f = std::fs::File::create(&aux_path).unwrap();
            writeln!(f, r#"{{"some_metadata":"value{}"}}"#, i).unwrap();
        }

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        // With limit=3, if we only took top 3 by mtime (all aux files), we'd get 0 sessions.
        // The over-fetch (limit*2=6) ensures we reach real sessions.
        let result = collect_recent_sessions(&paths, 3);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_extract_summary_from_fixture_linear() {
        let path = Path::new("tests/fixtures/linear_session.jsonl");
        let result = extract_summary(path).unwrap();
        assert_eq!(result.summary, "How do I sort a list in Python?");
        assert_eq!(result.session_id, "sess-linear-001");
    }

    #[test]
    fn test_extract_summary_from_fixture_desktop() {
        let path = Path::new("tests/fixtures/desktop_audit_session.jsonl");
        let result = extract_summary(path).unwrap();
        assert_eq!(result.summary, "Explain Docker networking");
    }

    #[test]
    fn test_truncate_summary_short() {
        assert_eq!(truncate_summary("short text", 100), "short text");
    }

    #[test]
    fn test_truncate_summary_exact() {
        let s = "a".repeat(100);
        assert_eq!(truncate_summary(&s, 100), s);
    }

    #[test]
    fn test_truncate_summary_long() {
        let s = "a".repeat(150);
        let result = truncate_summary(&s, 100);
        assert_eq!(result.chars().count(), 100); // 97 chars + "..."
        assert!(result.ends_with("..."));
    }

    // --- collect_recent_sessions tests ---

    fn write_test_session(dir: &std::path::Path, filename: &str, session_id: &str, msg: &str) {
        let path = dir.join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"{}","timestamp":"2025-06-01T10:00:00Z"}}"#, msg, session_id).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"reply"}}]}},"sessionId":"{}","timestamp":"2025-06-01T10:01:00Z"}}"#, session_id).unwrap();
    }

    #[test]
    fn test_collect_recent_sessions_finds_across_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let proj1 = dir.path().join("projects").join("-Users-user-proj1");
        let proj2 = dir.path().join("projects").join("-Users-user-proj2");
        std::fs::create_dir_all(&proj1).unwrap();
        std::fs::create_dir_all(&proj2).unwrap();

        write_test_session(&proj1, "sess1.jsonl", "sess-1", "Question one");
        write_test_session(&proj2, "sess2.jsonl", "sess-2", "Question two");

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 50);
        assert_eq!(result.len(), 2);

        let ids: Vec<&str> = result.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&"sess-1"));
        assert!(ids.contains(&"sess-2"));
    }

    #[test]
    fn test_collect_recent_sessions_skips_agent_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("projects").join("-Users-user-proj");
        std::fs::create_dir_all(&proj).unwrap();

        write_test_session(&proj, "sess1.jsonl", "sess-1", "Normal session");
        write_test_session(&proj, "agent-abc123.jsonl", "sess-1", "Agent session");

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 50);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].session_id, "sess-1");
    }

    #[test]
    fn test_collect_recent_sessions_sorts_by_timestamp_desc() {
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("projects").join("-Users-user-proj");
        std::fs::create_dir_all(&proj).unwrap();

        // Create files with different JSONL timestamps. If the filesystem mtime
        // lands on the same tick for both files, the path fallback should still
        // keep `newer.jsonl` ahead of `older.jsonl`.
        let older_path = proj.join("older.jsonl");
        let mut f = std::fs::File::create(&older_path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Old question"}}]}},"sessionId":"sess-old","timestamp":"2025-01-01T10:00:00Z"}}"#).unwrap();

        let newer_path = proj.join("newer.jsonl");
        let mut f = std::fs::File::create(&newer_path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"New question"}}]}},"sessionId":"sess-new","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 50);
        assert_eq!(result.len(), 2);
        // Final sort is by extracted recency descending, with a deterministic
        // file-path fallback when timestamps tie.
        assert_eq!(result[0].session_id, "sess-new");
        assert_eq!(result[1].session_id, "sess-old");
    }

    #[test]
    fn test_extract_summary_finds_late_summary_record() {
        // Simulates a long compacted session where the summary record appears
        // far from the beginning (beyond any fixed forward-scan limit).
        let mut f = NamedTempFile::new().unwrap();
        // First user message at line 1
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Initial question"}}]}},"sessionId":"sess-long","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        // 50 assistant/user exchanges (100 lines) pushing summary far away
        for i in 0..50 {
            writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Response {}"}}]}},"sessionId":"sess-long","timestamp":"2025-06-01T10:01:00Z"}}"#, i).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Follow-up {}"}}]}},"sessionId":"sess-long","timestamp":"2025-06-01T10:02:00Z"}}"#, i).unwrap();
        }
        // Summary record at line ~102 — well beyond any forward-scan limit
        writeln!(f, r#"{{"type":"summary","summary":"Session about Rust performance optimization","sessionId":"sess-long","timestamp":"2025-06-01T11:00:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(
            result.summary,
            "Session about Rust performance optimization"
        );
        assert_eq!(result.session_id, "sess-long");
    }

    #[test]
    fn test_extract_summary_finds_summary_in_middle_of_large_file() {
        // Regression test: summary record sits between the head scan (30 lines)
        // and the tail scan (last 256KB). Without pass 3, this summary is invisible.
        let mut f = NamedTempFile::new().unwrap();
        // First user message at line 1
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Initial question"}}]}},"sessionId":"sess-mid","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        // 40 lines of messages to push past the 30-line head scan
        for i in 0..40 {
            writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Response {}"}}]}},"sessionId":"sess-mid","timestamp":"2025-06-01T10:01:00Z"}}"#, i).unwrap();
        }
        // Summary record at line ~42 — beyond head scan
        writeln!(f, r#"{{"type":"summary","summary":"Mid-file compaction summary","sessionId":"sess-mid","timestamp":"2025-06-01T11:00:00Z"}}"#).unwrap();
        // Add >256KB of content after the summary to push it out of the tail scan
        let padding_line = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"sess-mid","timestamp":"2025-06-01T12:00:00Z"}}"#,
            "x".repeat(500)
        );
        // Each line is ~600 bytes; need ~430 lines to exceed 256KB
        for _ in 0..500 {
            writeln!(f, "{}", padding_line).unwrap();
        }

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Mid-file compaction summary");
        assert_eq!(result.session_id, "sess-mid");
    }

    #[test]
    fn test_find_summary_from_tail_handles_multibyte_utf8_boundary() {
        // If the tail-scan byte offset lands in the middle of a multibyte UTF-8
        // character, the function should still find the summary (not silently fail).
        let mut f = NamedTempFile::new().unwrap();
        // Write a line with multibyte characters (Cyrillic = 2 bytes each, emoji = 4 bytes each)
        // This line is ~200 bytes total
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Привет мир 🌍🌍🌍🌍🌍"}}]}},"sessionId":"sess-utf8","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        // Summary line is ~120 bytes
        writeln!(f, r#"{{"type":"summary","summary":"UTF-8 session summary","sessionId":"sess-utf8","timestamp":"2025-06-01T11:00:00Z"}}"#).unwrap();

        // Use a tail size that includes the summary line but starts mid-way through
        // the first line's multibyte characters. The file is ~320 bytes total;
        // reading the last 200 bytes should land inside the Cyrillic/emoji text.
        let result = find_summary_from_tail(f.path(), 200);
        assert!(result.is_some());
        let (sid, text) = result.unwrap();
        assert_eq!(text, "UTF-8 session summary");
        assert_eq!(sid, Some("sess-utf8".to_string()));
    }

    #[test]
    fn test_extract_summary_finds_summary_in_head_when_tail_misses() {
        // Simulates a session where the summary record is near the beginning
        // (within the first 30 lines) and the tail window doesn't contain it.
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Initial question"}}]}},"sessionId":"sess-head","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Summary found in head scan","sessionId":"sess-head","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();
        // Add a few more user messages after the summary
        for i in 0..5 {
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Follow-up {}"}}]}},"sessionId":"sess-head","timestamp":"2025-06-01T10:02:00Z"}}"#, i).unwrap();
        }

        let result = extract_summary(f.path()).unwrap();
        // Should prefer the summary record over the first user message
        assert_eq!(result.summary, "Summary found in head scan");
        assert_eq!(result.session_id, "sess-head");
    }

    #[test]
    fn test_extract_session_id_from_head_skips_non_session_records() {
        // Simulates a file where the first several lines are non-session records
        // (e.g., file-history-snapshot) without sessionId, followed by a user record
        // with a sessionId beyond the old 5-line limit.
        let mut f = NamedTempFile::new().unwrap();
        // 8 lines of metadata without sessionId
        for i in 0..8 {
            writeln!(
                f,
                r#"{{"type":"file-history-snapshot","files":["file{}.rs"]}}"#,
                i
            )
            .unwrap();
        }
        // User record with sessionId on line 9
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Real question"}}]}},"sessionId":"sess-late-id","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = extract_session_id_from_head(f.path());
        assert_eq!(result, Some("sess-late-id".to_string()));
    }

    #[test]
    fn test_extract_summary_finds_summary_after_user_in_large_file() {
        // Simulates a file larger than TAIL_BYTES where a summary record appears
        // AFTER the first user message in the head. The forward scan must not
        // break early before finding the summary.
        let mut f = NamedTempFile::new().unwrap();
        // User message on line 1
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Initial question"}}]}},"sessionId":"sess-big","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        // Some assistant/user exchanges
        for i in 0..5 {
            writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Response {}"}}]}},"sessionId":"sess-big","timestamp":"2025-06-01T10:01:00Z"}}"#, i).unwrap();
        }
        // Summary record on line 7 — after the first user message
        writeln!(f, r#"{{"type":"summary","summary":"Summary after user message","sessionId":"sess-big","timestamp":"2025-06-01T10:02:00Z"}}"#).unwrap();
        // Pad the file to exceed TAIL_BYTES (256KB) so the tail scan doesn't cover the head
        let padding_line = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"sess-big","timestamp":"2025-06-01T10:03:00Z"}}"#,
            "x".repeat(1024)
        );
        for _ in 0..300 {
            writeln!(f, "{}", padding_line).unwrap();
        }

        let result = extract_summary(f.path()).unwrap();
        // Must find the summary record, not fall back to the first user message
        assert_eq!(result.summary, "Summary after user message");
        assert_eq!(result.session_id, "sess-big");
    }

    #[test]
    fn test_extract_summary_user_message_beyond_head_scan() {
        // Regression test: session starts with >30 non-message records (e.g.,
        // file-history-snapshot) so the first user message is beyond the 30-line
        // head scan. Without scanning the middle for user messages, the session
        // would be silently dropped.
        let mut f = NamedTempFile::new().unwrap();
        // 35 lines of metadata records (with sessionId on the first one)
        writeln!(f, r#"{{"type":"file-history-snapshot","files":["a.rs"],"sessionId":"sess-deep-user","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        for i in 1..35 {
            writeln!(
                f,
                r#"{{"type":"file-history-snapshot","files":["file{}.rs"]}}"#,
                i
            )
            .unwrap();
        }
        // First user message at line 36 — beyond the 30-line head scan
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Deep user question"}}]}},"sessionId":"sess-deep-user","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();
        // A few more messages
        for i in 0..3 {
            writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Response {}"}}]}},"sessionId":"sess-deep-user","timestamp":"2025-06-01T10:02:00Z"}}"#, i).unwrap();
        }

        let result = extract_summary(f.path());
        assert!(
            result.is_some(),
            "Session with user message beyond 30-line head scan should not be dropped"
        );
        let session = result.unwrap();
        assert_eq!(session.summary, "Deep user question");
        assert_eq!(session.session_id, "sess-deep-user");
    }

    #[test]
    fn test_extract_summary_session_id_only_beyond_head() {
        // Regression test: session_id is only present on records beyond the 30-line
        // head scan. Pass 3 must extract session_id from those records rather than
        // relying solely on the head scan.
        let mut f = NamedTempFile::new().unwrap();
        // 35 lines of metadata records WITHOUT sessionId
        for i in 0..35 {
            writeln!(
                f,
                r#"{{"type":"file-history-snapshot","files":["file{}.rs"]}}"#,
                i
            )
            .unwrap();
        }
        // First user message at line 36 — has sessionId, beyond the head scan
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Late session ID question"}}]}},"sessionId":"sess-late-id","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let result = extract_summary(f.path());
        assert!(
            result.is_some(),
            "Session with session_id only beyond 30-line head scan should not be dropped"
        );
        let session = result.unwrap();
        assert_eq!(session.summary, "Late session ID question");
        assert_eq!(session.session_id, "sess-late-id");
    }

    #[test]
    fn test_find_summary_from_tail_exact_line_boundary() {
        // Regression test: if the tail-scan offset lands exactly on the first byte
        // of a JSONL record (i.e., the preceding byte is '\n'), the function must
        // NOT skip that line. Without checking the preceding byte, the code would
        // see '{' as the first byte, skip to the next newline, and drop the record.
        use std::io::Write;

        let mut f = NamedTempFile::new().unwrap();
        // Write a "prefix" line that we'll use to calculate exact offset
        let prefix_line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"padding"}]},"sessionId":"sess-boundary","timestamp":"2025-06-01T10:00:00Z"}"#;
        writeln!(f, "{}", prefix_line).unwrap();
        // The summary line starts right after the newline of the prefix line
        let summary_line = r#"{"type":"summary","summary":"Boundary summary found","sessionId":"sess-boundary","timestamp":"2025-06-01T11:00:00Z"}"#;
        writeln!(f, "{}", summary_line).unwrap();

        let file_len = f.as_file().metadata().unwrap().len();
        // Set max_bytes so that start = file_len - max_bytes lands exactly on the
        // first byte of the summary line (right after the prefix line's newline).
        let summary_offset = prefix_line.len() as u64 + 1; // +1 for the newline
        let max_bytes = file_len - summary_offset;

        let result = find_summary_from_tail(f.path(), max_bytes);
        assert!(
            result.is_some(),
            "Summary at exact tail boundary should not be skipped"
        );
        let (sid, text) = result.unwrap();
        assert_eq!(text, "Boundary summary found");
        assert_eq!(sid, Some("sess-boundary".to_string()));
    }

    #[test]
    fn test_collect_recent_sessions_respects_limit() {
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("projects").join("-Users-user-proj");
        std::fs::create_dir_all(&proj).unwrap();

        for i in 0..5 {
            write_test_session(
                &proj,
                &format!("sess{}.jsonl", i),
                &format!("sess-{}", i),
                &format!("Question {}", i),
            );
        }

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 3);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_extract_summary_detects_ralphex_automation() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Read the plan file. When done output <<<RALPHEX:ALL_TASKS_DONE>>>"}}]}},"sessionId":"sess-rx-001","timestamp":"2026-03-28T08:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Working on it."}}]}},"sessionId":"sess-rx-001","timestamp":"2026-03-28T08:01:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.automation, Some("ralphex".to_string()));
    }

    #[test]
    fn test_extract_summary_manual_session_no_automation() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"How do I sort a list?"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.automation, None);
    }

    #[test]
    fn test_extract_summary_ralphex_fixture_file() {
        let path = std::path::Path::new("tests/fixtures/ralphex_session.jsonl");
        let result = extract_summary(path).unwrap();
        assert_eq!(result.session_id, "sess-ralphex-001");
        assert_eq!(result.automation, Some("ralphex".to_string()));
    }

    #[test]
    fn test_extract_summary_linear_fixture_no_automation() {
        let path = std::path::Path::new("tests/fixtures/linear_session.jsonl");
        let result = extract_summary(path).unwrap();
        assert_eq!(result.session_id, "sess-linear-001");
        assert_eq!(result.automation, None);
    }

    #[test]
    fn test_extract_summary_marker_in_assistant_not_detected() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Tell me about ralphex"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Ralphex uses <<<RALPHEX:ALL_TASKS_DONE>>> signals."}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.automation, None);
    }

    #[test]
    fn test_extract_summary_tail_summary_uses_head_automation() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"When done output <<<RALPHEX:ALL_TASKS_DONE>>>"}}]}},"sessionId":"sess-tail-auto","timestamp":"2026-03-28T08:00:00Z"}}"#).unwrap();

        let padding_line = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"sess-tail-auto","timestamp":"2026-03-28T08:01:00Z"}}"#,
            "x".repeat(1024)
        );
        for _ in 0..300 {
            writeln!(f, "{}", padding_line).unwrap();
        }

        writeln!(f, r#"{{"type":"summary","summary":"Tail summary with automation marker only in head","sessionId":"sess-tail-auto","timestamp":"2026-03-28T08:02:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(
            result.summary,
            "Tail summary with automation marker only in head"
        );
        assert_eq!(result.automation, Some("ralphex".to_string()));
    }

    #[test]
    fn test_extract_summary_ignores_later_quoted_scheduled_task_marker() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"How can I distinguish ralphex transcripts from manual sessions?"}}]}},"sessionId":"sess-manual","timestamp":"2026-03-28T08:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Let's inspect the markers."}}]}},"sessionId":"sess-manual","timestamp":"2026-03-28T08:01:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"такие тоже надо детектить <scheduled-task name=\"chezmoi-sync\">"}}]}},"sessionId":"sess-manual","timestamp":"2026-03-28T08:02:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(
            result.summary,
            "How can I distinguish ralphex transcripts from manual sessions?"
        );
        assert_eq!(result.automation, None);
    }

    #[test]
    fn test_extract_summary_tail_summary_scans_middle_for_automation() {
        let mut f = NamedTempFile::new().unwrap();

        for i in 0..35 {
            writeln!(
                f,
                r#"{{"type":"file-history-snapshot","files":["file{}.rs"]}}"#,
                i
            )
            .unwrap();
        }

        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Follow the plan and emit <<<RALPHEX:ALL_TASKS_DONE>>> when complete"}}]}},"sessionId":"sess-mid-auto","timestamp":"2026-03-28T08:00:00Z"}}"#).unwrap();

        let padding_line = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"sess-mid-auto","timestamp":"2026-03-28T08:01:00Z"}}"#,
            "x".repeat(1024)
        );
        for _ in 0..300 {
            writeln!(f, "{}", padding_line).unwrap();
        }

        writeln!(f, r#"{{"type":"summary","summary":"Tail summary with automation marker only in middle","sessionId":"sess-mid-auto","timestamp":"2026-03-28T08:02:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(
            result.summary,
            "Tail summary with automation marker only in middle"
        );
        assert_eq!(result.automation, Some("ralphex".to_string()));
    }
}
