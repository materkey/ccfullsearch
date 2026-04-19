use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::dag::{DisplayFilter, SessionDag, TipStrategy};
use crate::search::extract_project_from_path;
use crate::session::record::{render_text_content, SessionRecord};
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
pub(crate) fn truncate_summary(s: &str, max_len: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_len {
        trimmed.to_string()
    } else {
        let truncated: String = trimmed.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

pub(crate) fn extract_non_meta_user_text(json: &serde_json::Value) -> Option<String> {
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
    render_text_content(content)
}

pub(crate) fn is_real_user_prompt(text: &str) -> bool {
    !text.starts_with("<system-reminder>")
}

/// Extract text from user or assistant messages for automation detection.
/// Ralphex markers appear in assistant responses, so we need to check both roles.
fn extract_text_for_automation(json: &serde_json::Value) -> Option<String> {
    let record_type = session::extract_record_type(json)?;
    if record_type != "user" && record_type != "assistant" {
        return None;
    }
    let message = json.get("message")?;
    let content = message.get("content")?;
    render_text_content(content)
}

#[derive(Default)]
struct ScanResult {
    session_id: Option<String>,
    first_user_message: Option<String>,
    last_summary: Option<String>,
    last_summary_sid: Option<String>,
    metadata_title: Option<String>,
    last_prompt: Option<String>,
    automation: Option<String>,
    lines_scanned: usize,
    saw_off_chain_summary: bool,
    last_timestamp: Option<DateTime<Utc>>,
}

struct ScanNeeds {
    summary: bool,
    user_message: bool,
    session_id: bool,
    automation: bool,
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

fn scan_head(
    path: &Path,
    max_lines: usize,
    latest_chain: Option<&HashSet<String>>,
) -> Option<ScanResult> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut scan = ScanResult::default();

    if let Some(tool) = session::detect_automation_by_path(path) {
        scan.automation = Some(tool.to_string());
    }

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

        if let Some(ts) = session::extract_timestamp(&json) {
            scan.last_timestamp = Some(scan.last_timestamp.map_or(ts, |prev| prev.max(ts)));
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

        // Check ALL messages (user + assistant) for automation markers
        if scan.automation.is_none() {
            if let Some(text) = extract_text_for_automation(&json) {
                scan.automation = session::detect_automation(&text).map(|s| s.to_string());
            }
        }

        if let Some(text) = extract_non_meta_user_text(&json) {
            if scan.first_user_message.is_none() && is_real_user_prompt(&text) {
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

#[derive(Default)]
struct TailSummaryScan {
    summary: Option<(Option<String>, String)>,
    saw_off_chain_summary: bool,
    /// Metadata records extracted from the tail of the JSONL file.
    /// These are written by Claude Code as standalone records without uuid.
    custom_title: Option<String>,
    ai_title: Option<String>,
    agent_name: Option<String>,
    last_prompt: Option<String>,
    last_timestamp: Option<DateTime<Utc>>,
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
    let mut custom_title: Option<String> = None;
    let mut ai_title: Option<String> = None;
    let mut agent_name: Option<String> = None;
    let mut last_prompt: Option<String> = None;
    let mut max_ts: Option<DateTime<Utc>> = None;
    for line in tail.lines() {
        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(ts) = session::extract_timestamp(&json) {
            max_ts = Some(max_ts.map_or(ts, |prev: DateTime<Utc>| prev.max(ts)));
        }
        if any_sid.is_none() {
            any_sid = session::extract_session_id(&json);
        }
        if let Some(record) = SessionRecord::from_value(&json) {
            match record {
                SessionRecord::Summary {
                    text, leaf_uuid, ..
                } => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let on_chain = match (latest_chain, leaf_uuid.as_deref()) {
                            (Some(chain), Some(leaf)) => chain.contains(leaf),
                            _ => true,
                        };
                        if on_chain {
                            let sid = session::extract_session_id(&json);
                            last_summary = Some((sid, truncate_summary(trimmed, 100)));
                        } else {
                            saw_off_chain_summary = true;
                        }
                    }
                }
                SessionRecord::CustomTitle(t) => {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        custom_title = Some(truncate_summary(trimmed, 100));
                    }
                }
                SessionRecord::AiTitle(t) => {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        ai_title = Some(truncate_summary(trimmed, 100));
                    }
                }
                SessionRecord::AgentName(name) => {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        agent_name = Some(truncate_summary(trimmed, 100));
                    }
                }
                SessionRecord::LastPrompt(prompt) => {
                    let trimmed = prompt.trim();
                    if !trimmed.is_empty() {
                        last_prompt = Some(truncate_summary(trimmed, 100));
                    }
                }
                _ => {}
            }
        }
    }

    Some(TailSummaryScan {
        summary: last_summary.map(|(sid, text)| (sid.or(any_sid), text)),
        saw_off_chain_summary,
        custom_title,
        ai_title,
        agent_name,
        last_prompt,
        last_timestamp: max_ts,
    })
}

#[cfg(test)]
fn find_summary_from_tail(path: &Path, max_bytes: u64) -> Option<(Option<String>, String)> {
    find_summary_from_tail_with_chain(path, max_bytes, None)?.summary
}

fn scan_tail(
    path: &Path,
    max_bytes: u64,
    latest_chain: Option<&HashSet<String>>,
) -> Option<ScanResult> {
    let tail = find_summary_from_tail_with_chain(path, max_bytes, latest_chain)?;

    let (last_summary, last_summary_sid) = match tail.summary {
        Some((sid, text)) => (Some(text), sid),
        None => (None, None),
    };

    let metadata_title = tail.agent_name.or(tail.custom_title).or(tail.ai_title);

    Some(ScanResult {
        last_summary,
        last_summary_sid,
        metadata_title,
        last_prompt: tail.last_prompt,
        saw_off_chain_summary: tail.saw_off_chain_summary,
        last_timestamp: tail.last_timestamp,
        ..Default::default()
    })
}

fn scan_middle(
    path: &Path,
    start_line: usize,
    end_byte: u64,
    needs: &ScanNeeds,
    latest_chain: Option<&HashSet<String>>,
) -> ScanResult {
    let mut result = ScanResult::default();

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return result,
    };
    let reader = BufReader::new(file);
    let mut bytes_read: u64 = 0;

    for (i, line) in reader.lines().enumerate() {
        if i < start_line {
            if let Ok(ref l) = line {
                bytes_read += l.len() as u64 + 1;
            }
            continue;
        }

        let in_tail_region = end_byte > 0 && bytes_read >= end_byte;
        let still_need_user_msg = needs.user_message && result.first_user_message.is_none();
        let still_need_sid = needs.session_id && result.session_id.is_none();
        let still_need_auto = needs.automation && result.automation.is_none();
        if in_tail_region && !(still_need_user_msg || still_need_sid || still_need_auto) {
            break;
        }

        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        bytes_read += line.len() as u64 + 1;

        let have_summary = !needs.summary || result.last_summary.is_some();
        let have_user_msg = !needs.user_message || result.first_user_message.is_some();
        let have_sid = !needs.session_id || result.session_id.is_some();
        let have_auto = !needs.automation || result.automation.is_some();
        let all_needs_met = have_summary && have_user_msg && have_sid && have_auto;

        // Timestamp extraction needs to happen for ALL parseable lines, not just
        // those matching the could_be_* predicates below. Try a cheap string check
        // first to avoid parsing lines that have neither timestamps nor needed fields.
        let could_have_timestamp =
            line.contains("\"timestamp\"") || line.contains("\"_audit_timestamp\"");

        // When all business fields are found, only continue scanning for timestamps
        if all_needs_met && !could_have_timestamp {
            continue;
        }

        let could_be_summary = needs.summary && !in_tail_region && line.contains("\"summary\"");
        let could_be_user = still_need_user_msg && line.contains("\"user\"");
        let could_be_msg =
            still_need_auto && (line.contains("\"user\"") || line.contains("\"assistant\""));
        let could_have_sid =
            still_need_sid && (line.contains("\"sessionId\"") || line.contains("\"session_id\""));

        if !could_have_timestamp
            && !could_be_summary
            && !could_be_user
            && !could_have_sid
            && !could_be_msg
        {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(ts) = session::extract_timestamp(&json) {
            result.last_timestamp = Some(result.last_timestamp.map_or(ts, |prev| prev.max(ts)));
        }

        if result.session_id.is_none() {
            result.session_id = session::extract_session_id(&json);
        }

        if could_be_summary && session::extract_record_type(&json) == Some("summary") {
            if let Some(summary_text) = json.get("summary").and_then(|v| v.as_str()) {
                let trimmed = summary_text.trim();
                if !trimmed.is_empty() {
                    if summary_is_on_latest_chain(&json, latest_chain) {
                        result.last_summary = Some(truncate_summary(trimmed, 100));
                        result.last_summary_sid = session::extract_session_id(&json);
                    } else {
                        result.saw_off_chain_summary = true;
                    }
                }
            }
        }

        if could_be_user {
            if let Some(text) = extract_non_meta_user_text(&json) {
                if result.first_user_message.is_none() && is_real_user_prompt(&text) {
                    result.first_user_message = Some(truncate_summary(&text, 100));
                }
            }
        }

        if still_need_auto {
            if let Some(text) = extract_text_for_automation(&json) {
                if let Some(tool) = session::detect_automation(&text) {
                    result.automation = Some(tool.to_string());
                }
            }
        }
    }

    result
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

        if session::extract_record_type(&json) != Some("user") {
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
        let Some(text) = render_text_content(content) else {
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
    // Cheap path-based shortcut for tools that land in a well-known directory
    // (e.g. claude-mem's observer sessions): skip content read entirely.
    if let Some(tool) = session::detect_automation_by_path(path) {
        return Some(tool.to_string());
    }

    // Automation markers appear early in the file (first user message),
    // so limit scan to avoid reading entire large JSONL files on the main thread.
    const MAX_LINES: usize = 50;

    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().take(MAX_LINES) {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };

        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Check both user and assistant messages for automation markers
        if let Some(text) = extract_text_for_automation(&json) {
            if let Some(tool) = session::detect_automation(&text) {
                return Some(tool.to_string());
            }
        }
    }

    None
}

/// Extract a `RecentSession` from a JSONL session file.
///
/// Priority for summary (highest to lowest):
/// 1. `agentName` from `type=agent-name` metadata record
/// 2. `customTitle` from `type=custom-title` metadata record
/// 3. `aiTitle` from `type=ai-title` metadata record
/// 4. `type=summary` record -> use `.summary` field (scans file tail, head, then middle)
/// 5. `lastPrompt` from `type=last-prompt` metadata record
/// 6. First `type=user` where `isMeta` is not true -> extract text content
///
/// Uses a three-pass approach via `scan_head`, `scan_tail`, and `scan_middle`:
/// 1. `scan_head`: first 30 lines for session_id, first user message, summary, automation
/// 2. `scan_tail`: last 256KB for summary, metadata titles, last-prompt
/// 3. `scan_middle`: remaining lines for anything head/tail missed
///
/// Uses last message timestamp (falls back to file mtime) for accurate recency sorting.
pub fn extract_summary(path: &Path) -> Option<RecentSession> {
    let path_str = path.to_str().unwrap_or("");
    let source = SessionSource::from_path(path_str);
    let project = extract_project_from_path(path_str);
    const TAIL_BYTES: u64 = 256 * 1024;
    let latest_chain = SessionDag::from_file(path, DisplayFilter::Standard)
        .ok()
        .and_then(|dag| {
            let tip = dag.tip(TipStrategy::LastAppended)?;
            Some(dag.chain_from(tip))
        });

    let mtime = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let mtime_timestamp: DateTime<Utc> = mtime.into();

    let head = scan_head(path, HEAD_SCAN_LINES, latest_chain.as_ref())?;

    let file_len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let tail_start = file_len.saturating_sub(TAIL_BYTES);
    let tail = scan_tail(path, TAIL_BYTES, latest_chain.as_ref()).unwrap_or_default();

    // Merge head + tail
    let mut session_id = head.session_id.clone();
    let mut first_user_message = head.first_user_message.clone();
    let mut last_summary = head.last_summary.clone();
    let mut last_summary_sid = head.last_summary_sid.clone();
    let mut automation = head.automation.clone();
    let mut saw_off_chain_summary = head.saw_off_chain_summary || tail.saw_off_chain_summary;

    if tail.last_summary.is_some() {
        let tail_sid = tail.last_summary_sid.clone();
        session_id = tail_sid.clone().or(session_id);
        last_summary = tail.last_summary.clone();
        last_summary_sid = tail_sid;
    }

    // Conditionally scan the middle region for anything head/tail missed
    let need_summary = last_summary.is_none();
    let need_user_msg = first_user_message.is_none();
    let need_sid = last_summary_sid.is_none() && session_id.is_none();
    let need_automation = automation.is_none();
    let should_scan_middle =
        (need_summary && tail_start > 0) || need_user_msg || need_sid || need_automation;

    let mut middle_timestamp = None;
    if should_scan_middle && head.lines_scanned >= HEAD_SCAN_LINES {
        let needs = ScanNeeds {
            summary: need_summary,
            user_message: need_user_msg,
            session_id: need_sid,
            automation: need_automation,
        };
        let middle = scan_middle(
            path,
            head.lines_scanned,
            tail_start,
            &needs,
            latest_chain.as_ref(),
        );

        if session_id.is_none() {
            session_id = middle.session_id;
        }
        if last_summary.is_none() {
            last_summary = middle.last_summary;
            if last_summary.is_some() {
                last_summary_sid = middle.last_summary_sid;
            }
        }
        if first_user_message.is_none() {
            first_user_message = middle.first_user_message;
        }
        if automation.is_none() {
            automation = middle.automation;
        }
        saw_off_chain_summary = saw_off_chain_summary || middle.saw_off_chain_summary;
        middle_timestamp = middle.last_timestamp;
    }

    let content_timestamp = [head.last_timestamp, tail.last_timestamp, middle_timestamp]
        .into_iter()
        .flatten()
        .max();

    // Apply title priority: metadata_title > summary > lastPrompt > firstUserMessage
    if let Some(title) = tail.metadata_title {
        let sid = last_summary_sid
            .or(session_id)
            .or_else(|| head.session_id.clone())?;
        return Some(RecentSession {
            session_id: sid,
            file_path: path_str.to_string(),
            project,
            source,
            timestamp: content_timestamp.unwrap_or(mtime_timestamp),
            summary: title,
            automation,
        });
    }

    if let Some(summary_text) = last_summary {
        let sid = last_summary_sid.or(session_id)?;
        return Some(RecentSession {
            session_id: sid,
            file_path: path_str.to_string(),
            project,
            source,
            timestamp: content_timestamp.unwrap_or(mtime_timestamp),
            summary: summary_text,
            automation,
        });
    }

    if let Some(prompt) = tail.last_prompt {
        let session_id = session_id?;
        return Some(RecentSession {
            session_id,
            file_path: path_str.to_string(),
            project,
            source,
            timestamp: content_timestamp.unwrap_or(mtime_timestamp),
            summary: prompt,
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
        timestamp: content_timestamp.unwrap_or(mtime_timestamp),
        summary,
        automation,
    })
}

/// Read the first few lines to extract session_id.
/// Scans up to 30 lines to handle files that start with non-session records
/// (e.g., file-history-snapshot, summary) before the first record with a session_id.
#[cfg(test)]
fn extract_session_id_from_head(path: &Path) -> Option<String> {
    scan_head(path, HEAD_SCAN_LINES, None).and_then(|scan| scan.session_id)
}

/// Walk directories and find `*.jsonl` session files, skipping agent files and
/// `subagents/` directories via the shared session-layer walker.
fn find_jsonl_files(search_paths: &[String]) -> Vec<PathBuf> {
    session::collect_session_jsonl_files(search_paths)
}

/// Collect recent sessions from search paths.
///
/// Walks directories, finds `*.jsonl` files (skipping `agent-*`),
/// pre-sorts by filesystem mtime descending, extracts summaries in parallel
/// with rayon (filtering out non-session files), and returns the top
/// `limit` results sorted by content timestamp descending.
///
/// When timestamps tie, file path is used as a deterministic fallback so
/// equal-timestamp sessions do not produce unstable ordering across platforms.
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

    // Partition by path-based automation so each class gets its own `limit` quota.
    // Otherwise a burst of claude-mem observer sessions can crowd the top-mtime
    // window and the TUI's AutomationFilter has nothing left to show as Manual.
    let (auto_files, manual_files): (Vec<_>, Vec<_>) = files_with_mtime
        .into_iter()
        .partition(|(p, _)| session::detect_automation_by_path(p).is_some());

    let mut sessions = collect_from_files(manual_files, limit);
    sessions.extend(collect_from_files(auto_files, limit));

    // Merge-level dedup across partitions (rare: same session_id appearing in both
    // manual and auto pools — possible only if the two pools somehow share a file,
    // but cheap to guard against).
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<RecentSession> = Vec::with_capacity(sessions.len());
    for session in sessions {
        if let Some(&idx) = seen.get(&session.session_id) {
            if session.timestamp > deduped[idx].timestamp {
                deduped[idx] = session;
            }
        } else {
            seen.insert(session.session_id.clone(), deduped.len());
            deduped.push(session);
        }
    }
    deduped.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.file_path.cmp(&a.file_path))
    });
    deduped
}

/// Apply the mtime-ordered batching + dedup + sort + truncate pipeline to a
/// single pre-sorted (descending by mtime) file list.
fn collect_from_files(
    files_with_mtime: Vec<(PathBuf, std::time::SystemTime)>,
    limit: usize,
) -> Vec<RecentSession> {
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
            files_with_mtime.len().saturating_sub(offset)
        };
        let end = (offset + batch_size).min(files_with_mtime.len());
        if offset >= end {
            break;
        }

        let batch_sessions: Vec<RecentSession> = files_with_mtime[offset..end]
            .par_iter()
            .filter_map(|(path, _)| extract_summary(path))
            .collect();
        sessions.extend(batch_sessions);
        offset = end;

        let unique_count = {
            let mut seen = HashSet::new();
            sessions
                .iter()
                .filter(|s| seen.insert(&s.session_id))
                .count()
        };
        if unique_count >= limit {
            if offset >= files_with_mtime.len() {
                break;
            }
            // Verify no remaining file can displace our current top `limit`.
            // Since content_timestamp <= mtime (post-session metadata writes
            // inflate mtime), the next unscanned file's mtime is an upper bound
            // on its content timestamp.  Use strict inequality: when
            // cutoff == next_mtime, an unscanned file could still tie on
            // timestamp and outrank on file_path (the deterministic tiebreaker).
            let mut best_ts: HashMap<&str, DateTime<Utc>> = HashMap::new();
            for s in &sessions {
                best_ts
                    .entry(&s.session_id)
                    .and_modify(|t| {
                        if s.timestamp > *t {
                            *t = s.timestamp;
                        }
                    })
                    .or_insert(s.timestamp);
            }
            let mut sorted_ts: Vec<DateTime<Utc>> = best_ts.into_values().collect();
            sorted_ts.sort_unstable_by(|a, b| b.cmp(a));
            if let Some(&cutoff) = sorted_ts.get(limit.saturating_sub(1)) {
                let next_mtime: DateTime<Utc> = files_with_mtime[offset].1.into();
                if cutoff > next_mtime {
                    break;
                }
            }
        }
    }

    // Deduplicate by session_id, keeping the newest-timestamp record.
    // This handles git worktrees where the same session appears in multiple dirs.
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut deduped: Vec<RecentSession> = Vec::with_capacity(sessions.len());
    for session in sessions {
        if let Some(&idx) = seen.get(&session.session_id) {
            if session.timestamp > deduped[idx].timestamp {
                deduped[idx] = session;
            }
        } else {
            seen.insert(session.session_id.clone(), deduped.len());
            deduped.push(session);
        }
    }

    deduped.sort_by(|a, b| {
        b.timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.file_path.cmp(&a.file_path))
    });
    deduped.truncate(limit);
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use filetime::{set_file_mtime, FileTime};
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
    fn test_extract_summary_truncates_long_messages() {
        let long_msg = "a".repeat(200);
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#, long_msg).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary.chars().count(), 100); // 97 chars + "..."
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
    fn test_extract_summary_prefers_content_timestamp_over_mtime() {
        let content_ts = Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
        let file_mtime = Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap();

        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello world"}}]}},"sessionId":"sess-ts-001","timestamp":"2025-01-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Hi there"}}]}},"sessionId":"sess-ts-001","timestamp":"2025-01-01T10:00:00Z"}}"#).unwrap();

        // Set file mtime to a much later date (simulates Claude appending metadata records)
        set_file_mtime(
            f.path(),
            FileTime::from_unix_time(file_mtime.timestamp(), 0),
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(
            result.timestamp, content_ts,
            "expected content timestamp {}, got {} (file mtime is {})",
            content_ts, result.timestamp, file_mtime
        );
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
        // The over-fetch (limit*4=12) ensures we reach real sessions.
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

        // Create files with INVERTED mtime vs content timestamps to prove
        // that ordering is based on content timestamp, not filesystem mtime.
        let older_content_path = proj.join("older.jsonl");
        let mut f = std::fs::File::create(&older_content_path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Old question"}}]}},"sessionId":"sess-old","timestamp":"2025-01-01T10:00:00Z"}}"#).unwrap();

        let newer_content_path = proj.join("newer.jsonl");
        let mut f = std::fs::File::create(&newer_content_path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"New question"}}]}},"sessionId":"sess-new","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        // Set mtimes INVERTED: older content gets newer mtime, newer content
        // gets older mtime. If sorting used mtime, sess-old would come first.
        set_file_mtime(
            &older_content_path,
            FileTime::from_unix_time(1750000000, 0), // ~2025-06-15
        )
        .unwrap();
        set_file_mtime(
            &newer_content_path,
            FileTime::from_unix_time(1700000000, 0), // ~2023-11-14
        )
        .unwrap();

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 50);
        assert_eq!(result.len(), 2);
        // Sort must follow content timestamp (sess-new=2025-06 first),
        // NOT mtime (which would put sess-old first).
        assert_eq!(result[0].session_id, "sess-new");
        assert_eq!(result[1].session_id, "sess-old");
    }

    #[test]
    fn test_collect_recent_sessions_mtime_invariant_prevents_missing_sessions() {
        // Regression: when files with high mtime have low content timestamps
        // (large metadata drift), the early-stop condition must not skip files
        // beyond the first batch that have higher content timestamps.
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("projects").join("-Users-user-proj");
        std::fs::create_dir_all(&proj).unwrap();

        // 4 drift files: high mtime but very old content timestamps.
        for i in 1..=4u32 {
            let path = proj.join(format!("drift{}.jsonl", i));
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"drift {}"}}]}},"sessionId":"sess-drift-{}","timestamp":"2025-01-0{}T10:00:00Z"}}"#, i, i, i).unwrap();
            // mtime ~2025-06-15 minus i days (all higher than the "recent" file)
            set_file_mtime(
                &path,
                FileTime::from_unix_time(1750000000 - (i as i64 - 1) * 86400, 0),
            )
            .unwrap();
        }

        // 1 recent file: lower mtime but much newer content timestamp.
        let recent_path = proj.join("recent.jsonl");
        let mut f = std::fs::File::create(&recent_path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"recent"}}]}},"sessionId":"sess-recent","timestamp":"2025-06-06T10:00:00Z"}}"#).unwrap();
        set_file_mtime(
            &recent_path,
            FileTime::from_unix_time(1749168000, 0), // ~2025-06-06
        )
        .unwrap();

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        // limit=1 with batch_multiplier=4: first batch processes the 4 drift files.
        // Without the mtime invariant check, sess-drift-4 (Jan 4) would win.
        // With the fix, the invariant detects that the next file's mtime exceeds
        // the cutoff, continues scanning, and sess-recent (Jun 6) correctly wins.
        let result = collect_recent_sessions(&paths, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].session_id, "sess-recent");
    }

    #[test]
    fn test_collect_recent_sessions_equal_mtime_tiebreak_on_path() {
        // Regression: when the cutoff content_timestamp equals the next file's
        // mtime, the early-stop must use strict `>` (not `>=`), otherwise the
        // unscanned file is skipped even though it may have a better
        // content_timestamp.
        //
        // Setup: path_a has mtime == sess-b's content_timestamp (the cutoff).
        // sess-a has a strictly better content_timestamp than sess-b.
        // With `>=` the early-stop fires and sess-a is never scanned (BUG).
        // With `>` scanning continues, sess-a is found and wins (CORRECT).
        let dir = tempfile::TempDir::new().unwrap();
        let proj = dir.path().join("projects").join("-Users-user-proj");
        std::fs::create_dir_all(&proj).unwrap();

        // 2025-06-15T10:00:00Z == unix 1749981600.
        // path_a's mtime is set to this value so cutoff == next_mtime exactly.
        let cutoff_mtime = 1749981600i64;

        // File "aaa.jsonl" — content timestamp 1 second BETTER than sess-b.
        // mtime equals the cutoff so the `>=` vs `>` distinction matters.
        let path_a = proj.join("aaa.jsonl");
        let mut f = std::fs::File::create(&path_a).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"session A"}}]}},"sessionId":"sess-a","timestamp":"2025-06-15T10:00:01Z"}}"#).unwrap();
        set_file_mtime(&path_a, FileTime::from_unix_time(cutoff_mtime, 0)).unwrap();

        // File "bbb.jsonl" — content timestamp == cutoff.  Slightly higher mtime
        // so it lands in the first batch (sorted by mtime DESC).
        let path_b = proj.join("bbb.jsonl");
        let mut f = std::fs::File::create(&path_b).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"session B"}}]}},"sessionId":"sess-b","timestamp":"2025-06-15T10:00:00Z"}}"#).unwrap();
        set_file_mtime(&path_b, FileTime::from_unix_time(cutoff_mtime + 1, 0)).unwrap();

        // 3 filler files to fill the first batch (batch_multiplier=4, limit=1 → batch=4).
        for i in 1..=3u32 {
            let path = proj.join(format!("filler{}.jsonl", i));
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"filler {}"}}]}},"sessionId":"sess-filler-{}","timestamp":"2025-01-0{}T10:00:00Z"}}"#, i, i, i).unwrap();
            set_file_mtime(
                &path,
                FileTime::from_unix_time(cutoff_mtime + 2 + i as i64, 0),
            )
            .unwrap();
        }

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        // limit=1: first batch = bbb + 3 fillers (4 files).
        // cutoff = sess-b content_timestamp = 2025-06-15T10:00:00Z
        // next_mtime = path_a mtime = 2025-06-15T10:00:00Z (== cutoff)
        // With `>=`: early-stop fires, sess-a never scanned → returns sess-b (BUG)
        // With `>`:  continues scanning, sess-a (10:00:01) beats sess-b → sess-a (CORRECT)
        let result = collect_recent_sessions(&paths, 1);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].session_id, "sess-a",
            "sess-a (better content_timestamp) must win; \
             if sess-b won, the `>=` early-stop bug has regressed"
        );
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
    fn test_collect_recent_sessions_per_class_cap() {
        // Regression: before partitioning the pipeline applied limit to the raw
        // mtime-sorted file list, so many auto sessions (claude-mem observer)
        // could crowd out all manual ones. Per-class cap keeps both populated.
        let dir = tempfile::TempDir::new().unwrap();
        let manual_dir = dir.path().join("projects").join("-Users-u-manual-proj");
        let auto_dir = dir
            .path()
            .join("projects")
            .join("-Users-u--claude-mem-observer-sessions");
        std::fs::create_dir_all(&manual_dir).unwrap();
        std::fs::create_dir_all(&auto_dir).unwrap();

        for i in 0..3 {
            write_test_session(
                &manual_dir,
                &format!("m{}.jsonl", i),
                &format!("manual-{}", i),
                &format!("manual question {}", i),
            );
        }
        // Auto sessions written second → newer mtime → would crowd top-K before fix.
        for i in 0..20 {
            write_test_session(
                &auto_dir,
                &format!("a{}.jsonl", i),
                &format!("auto-{}", i),
                &format!("auto content {}", i),
            );
        }

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 3);

        let manual_count = result.iter().filter(|s| s.automation.is_none()).count();
        let auto_count = result.iter().filter(|s| s.automation.is_some()).count();
        assert_eq!(
            manual_count, 3,
            "manual quota must survive even when auto dominates mtime"
        );
        assert_eq!(auto_count, 3, "auto class keeps its own cap");
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
    fn test_extract_summary_marker_in_assistant_detected() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Tell me about ralphex"}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"Ralphex uses <<<RALPHEX:ALL_TASKS_DONE>>> signals."}}]}},"sessionId":"sess-001","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.automation, Some("ralphex".to_string()));
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

    #[test]
    fn test_tail_extracts_custom_title() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello world"}}]}},"sessionId":"sess-ct","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"My Custom Session","sessionId":"sess-ct"}}"#
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "My Custom Session");
        assert_eq!(result.session_id, "sess-ct");
    }

    #[test]
    fn test_tail_extracts_ai_title() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello world"}}]}},"sessionId":"sess-ai","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"ai-title","aiTitle":"Debugging auth flow","sessionId":"sess-ai"}}"#
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Debugging auth flow");
        assert_eq!(result.session_id, "sess-ai");
    }

    #[test]
    fn test_title_priority_custom_over_ai() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello world"}}]}},"sessionId":"sess-both","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"ai-title","aiTitle":"AI generated title","sessionId":"sess-both"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"User custom title","sessionId":"sess-both"}}"#
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "User custom title");
    }

    #[test]
    fn test_title_priority_agent_name_highest() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello world"}}]}},"sessionId":"sess-agent","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"ai-title","aiTitle":"AI title","sessionId":"sess-agent"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"Custom title","sessionId":"sess-agent"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"agent-name","agentName":"researcher","sessionId":"sess-agent"}}"#
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "researcher");
    }

    #[test]
    fn test_recent_skips_subagent_dir() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let base = temp_dir.path();

        // Create a normal session file
        let normal_path = base.join("session.jsonl");
        std::fs::write(&normal_path, r#"{"type":"user","sessionId":"s1"}"#).unwrap();

        // Create a subagents directory with a JSONL file inside
        let subagents_dir = base.join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let subagent_path = subagents_dir.join("sub-session.jsonl");
        std::fs::write(&subagent_path, r#"{"type":"user","sessionId":"s2"}"#).unwrap();

        let files = find_jsonl_files(&[base.to_string_lossy().to_string()]);

        // Should contain session.jsonl but NOT subagents/sub-session.jsonl
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].file_name().unwrap().to_str().unwrap(),
            "session.jsonl"
        );
    }

    fn write_test_session_with_ts(
        dir: &std::path::Path,
        filename: &str,
        session_id: &str,
        msg: &str,
        ts: &str,
    ) {
        let path = dir.join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"sessionId":"{}","timestamp":"{}"}}"#, msg, session_id, ts).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"reply"}}]}},"sessionId":"{}","timestamp":"{}"}}"#, session_id, ts).unwrap();
    }

    #[test]
    fn test_dedup_sessions_keeps_newest() {
        let dir = tempfile::TempDir::new().unwrap();
        let proj1 = dir.path().join("projects").join("-Users-user-projA");
        let proj2 = dir.path().join("projects").join("-Users-user-projB");
        std::fs::create_dir_all(&proj1).unwrap();
        std::fs::create_dir_all(&proj2).unwrap();

        // Same session_id in two project dirs, different content timestamps
        write_test_session_with_ts(
            &proj1,
            "dup.jsonl",
            "sess-dup-1",
            "Old question",
            "2025-06-01T10:00:00Z",
        );
        write_test_session_with_ts(
            &proj2,
            "dup.jsonl",
            "sess-dup-1",
            "New question",
            "2025-06-02T10:00:00Z",
        );

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 50);

        // Should have exactly 1 session after dedup
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].session_id, "sess-dup-1");
        // Should keep the one with newer timestamp
        assert_eq!(result[0].summary, "New question");
    }

    #[test]
    fn test_dedup_sessions_different_ids_preserved() {
        let dir = tempfile::TempDir::new().unwrap();
        let proj1 = dir.path().join("projects").join("-Users-user-projA");
        let proj2 = dir.path().join("projects").join("-Users-user-projB");
        std::fs::create_dir_all(&proj1).unwrap();
        std::fs::create_dir_all(&proj2).unwrap();

        write_test_session_with_ts(
            &proj1,
            "a.jsonl",
            "sess-unique-1",
            "Question one",
            "2025-06-01T10:00:00Z",
        );
        write_test_session_with_ts(
            &proj2,
            "b.jsonl",
            "sess-unique-2",
            "Question two",
            "2025-06-02T10:00:00Z",
        );

        let paths = vec![dir.path().join("projects").to_str().unwrap().to_string()];
        let result = collect_recent_sessions(&paths, 50);

        // Different session IDs should both be preserved
        assert_eq!(result.len(), 2);
        let ids: HashSet<String> = result.iter().map(|s| s.session_id.clone()).collect();
        assert!(ids.contains("sess-unique-1"));
        assert!(ids.contains("sess-unique-2"));
    }

    // --- scan_head tests ---

    #[test]
    fn test_scan_head_extracts_user_message() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello from scan_head"}}]}},"sessionId":"sess-sh-1","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = scan_head(f.path(), 30, None).unwrap();
        assert_eq!(result.session_id, Some("sess-sh-1".to_string()));
        assert_eq!(
            result.first_user_message,
            Some("Hello from scan_head".to_string())
        );
        assert!(result.last_summary.is_none());
        assert!(result.metadata_title.is_none());
        assert!(result.last_prompt.is_none());
        assert_eq!(result.lines_scanned, 1);
    }

    #[test]
    fn test_scan_head_extracts_summary_and_session_id() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Head summary text","sessionId":"sess-sh-2","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"User prompt"}}]}},"sessionId":"sess-sh-2","timestamp":"2025-06-01T10:01:00Z"}}"#).unwrap();

        let result = scan_head(f.path(), 30, None).unwrap();
        assert_eq!(result.session_id, Some("sess-sh-2".to_string()));
        assert_eq!(result.last_summary, Some("Head summary text".to_string()));
        assert_eq!(result.first_user_message, Some("User prompt".to_string()));
    }

    #[test]
    fn test_scan_head_empty_file() {
        let f = NamedTempFile::new().unwrap();
        let result = scan_head(f.path(), 30, None).unwrap();
        assert!(result.session_id.is_none());
        assert!(result.first_user_message.is_none());
        assert!(result.last_summary.is_none());
        assert_eq!(result.lines_scanned, 0);
    }

    #[test]
    fn test_scan_head_metadata_only_no_user_message() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..5 {
            writeln!(
                f,
                r#"{{"type":"file-history-snapshot","files":["file{}.rs"],"sessionId":"sess-sh-3"}}"#,
                i
            )
            .unwrap();
        }

        let result = scan_head(f.path(), 30, None).unwrap();
        assert_eq!(result.session_id, Some("sess-sh-3".to_string()));
        assert!(result.first_user_message.is_none());
        assert!(result.last_summary.is_none());
        assert_eq!(result.lines_scanned, 5);
    }

    #[test]
    fn test_scan_head_detects_automation() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Emit <<<RALPHEX:ALL_TASKS_DONE>>> when done"}}]}},"sessionId":"sess-sh-auto","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = scan_head(f.path(), 30, None).unwrap();
        assert_eq!(result.automation, Some("ralphex".to_string()));
    }

    // --- scan_tail tests ---

    #[test]
    fn test_scan_tail_extracts_summary() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-st-1","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Tail summary found","sessionId":"sess-st-1","timestamp":"2025-06-01T11:00:00Z"}}"#).unwrap();

        let result = scan_tail(f.path(), 4096, None).unwrap();
        assert_eq!(result.last_summary, Some("Tail summary found".to_string()));
        assert!(result.metadata_title.is_none());
        assert!(result.last_prompt.is_none());
    }

    #[test]
    fn test_scan_tail_extracts_custom_title() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-st-2","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"My Custom Title","sessionId":"sess-st-2"}}"#
        )
        .unwrap();

        let result = scan_tail(f.path(), 4096, None).unwrap();
        assert_eq!(result.metadata_title, Some("My Custom Title".to_string()));
    }

    #[test]
    fn test_scan_tail_extracts_ai_title() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-st-3","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"ai-title","aiTitle":"AI Generated Title","sessionId":"sess-st-3"}}"#
        )
        .unwrap();

        let result = scan_tail(f.path(), 4096, None).unwrap();
        assert_eq!(
            result.metadata_title,
            Some("AI Generated Title".to_string())
        );
    }

    #[test]
    fn test_scan_tail_extracts_agent_name() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-st-4","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"agent-name","agentName":"researcher","sessionId":"sess-st-4"}}"#
        )
        .unwrap();

        let result = scan_tail(f.path(), 4096, None).unwrap();
        assert_eq!(result.metadata_title, Some("researcher".to_string()));
    }

    #[test]
    fn test_scan_tail_agent_name_beats_custom_title() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-st-5","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"ai-title","aiTitle":"AI title","sessionId":"sess-st-5"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"Custom title","sessionId":"sess-st-5"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"agent-name","agentName":"builder","sessionId":"sess-st-5"}}"#
        )
        .unwrap();

        let result = scan_tail(f.path(), 4096, None).unwrap();
        assert_eq!(result.metadata_title, Some("builder".to_string()));
    }

    #[test]
    fn test_scan_tail_extracts_last_prompt() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-st-6","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"last-prompt","lastPrompt":"Fix the build","sessionId":"sess-st-6"}}"#
        )
        .unwrap();

        let result = scan_tail(f.path(), 4096, None).unwrap();
        assert_eq!(result.last_prompt, Some("Fix the build".to_string()));
    }

    // --- merge logic tests ---

    #[test]
    fn test_merge_metadata_title_beats_summary() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-merge-1","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"A summary","sessionId":"sess-merge-1","timestamp":"2025-06-01T11:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"Preferred title","sessionId":"sess-merge-1"}}"#
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Preferred title");
    }

    #[test]
    fn test_merge_summary_beats_last_prompt() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Hello"}}]}},"sessionId":"sess-merge-2","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"summary","summary":"Summary wins","sessionId":"sess-merge-2","timestamp":"2025-06-01T11:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"last-prompt","lastPrompt":"Last prompt","sessionId":"sess-merge-2"}}"#
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Summary wins");
    }

    #[test]
    fn test_merge_last_prompt_beats_first_user_message() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"First user message"}}]}},"sessionId":"sess-merge-3","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"last-prompt","lastPrompt":"Last prompt wins","sessionId":"sess-merge-3"}}"#
        )
        .unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Last prompt wins");
    }

    #[test]
    fn test_merge_falls_back_to_first_user_message() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Only user message"}}]}},"sessionId":"sess-merge-4","timestamp":"2025-06-01T10:00:00Z"}}"#).unwrap();

        let result = extract_summary(f.path()).unwrap();
        assert_eq!(result.summary, "Only user message");
    }
}
