//! Ctrl+G agent search: runs `claude -p` restricted to `ccs search` /
//! `ccs show`, streams its `stream-json` output, reports the agent's search
//! queries as progress and returns up to [`MAX_HITS`] sessions.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Deserialize;

use crate::config::AgentSearchConfig;
use crate::recent::{extract_summary, RecentSession};

pub const AGENT_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_HITS: usize = 5;
pub const AGENT_NO_MATCH_MSG: &str = "nothing found; edit the query and press Enter";

/// One session as the agent reports it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentHit {
    pub session_id: String,
    pub file_path: String,
    pub why: String,
    /// `host` of the search row; absent or `local` for this machine.
    #[serde(default)]
    pub host: Option<String>,
    /// `project` and `timestamp` of the search row, for a remote hit whose
    /// file is not here to read.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// A hit resolved to a session on disk.
#[derive(Debug, Clone)]
pub struct FoundSession {
    pub session: RecentSession,
    /// The agent's reasons, one per hit it reported in this session.
    pub whys: Vec<String>,
}

pub enum AgentEvent {
    /// All search queries the agent has issued so far, de-duplicated, in
    /// first-seen order.
    Progress(Vec<String>),
    Done(Result<Vec<FoundSession>, String>),
}

/// A running agent. Dropping it kills the agent.
pub struct AgentSearchHandle {
    pub rx: Receiver<AgentEvent>,
    cancel: Arc<AtomicBool>,
    child: Option<Arc<Mutex<Child>>>,
}

impl AgentSearchHandle {
    /// Stop the agent: set the flag the threads check and kill the process
    /// right away, so a drop just before ccs exits does not orphan it.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(child) = &self.child {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }
    }
}

#[cfg(test)]
impl AgentSearchHandle {
    /// A handle with no process behind it, fed by the test through `rx`.
    pub(crate) fn for_test(rx: Receiver<AgentEvent>) -> (Self, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = Self {
            rx,
            cancel: Arc::clone(&cancel),
            child: None,
        };
        (handle, cancel)
    }
}

impl Drop for AgentSearchHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Path of the running ccs binary for the agent to call.
pub fn resolve_ccs_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate ccs binary: {e}"))?;
    Ok(ccs_path_or_proc_exe(exe, std::process::id()))
}

/// On Linux a binary replaced in place shows up as `<path> (deleted)`;
/// `/proc/<pid>/exe` still execs the running version.
fn ccs_path_or_proc_exe(exe: PathBuf, pid: u32) -> PathBuf {
    if exe.to_string_lossy().ends_with(" (deleted)") {
        PathBuf::from(format!("/proc/{pid}/exe"))
    } else {
        exe
    }
}

pub fn system_prompt(ccs: &Path) -> String {
    let ccs = ccs.display();
    format!(
        "You find sessions in the user's Claude Code / Codex history. Tools: Bash, restricted to \
`{ccs} search` and `{ccs} show`.
- `{ccs} search \"<query>\" --limit 10 [--regex]` prints JSONL matches (session_id, project, \
file_path, line_number, timestamp, content) and a summary line.
- `{ccs} show <file_path> --line N --context 3` prints the surrounding messages.
Work fast: in your FIRST turn issue 3-5 `{ccs} search` calls in parallel (Russian and English \
phrasings, synonyms, product/tool names, a --regex alternation). Open a session with \
`{ccs} show` only when snippets are ambiguous. Stop as soon as you are confident.
Skip matches whose file_path contains '#' (Opencode sessions cannot be opened here).
Each match has a `host`: `local`, or another machine. For a session on another machine pass \
`--host <host>` to `{ccs} show`.
Final answer: ONLY a JSON array of up to {MAX_HITS} objects {{\"session_id\": \"...\", \
\"file_path\": \"...\", \"why\": \"<10 words>\", \"host\": \"...\", \"project\": \"...\", \
\"timestamp\": \"...\"}} (host, project, timestamp copied from the match), best match first, \
[] if nothing fits. No prose, no code fences."
    )
}

pub fn build_agent_args(query: &str, ccs: &Path, cfg: &AgentSearchConfig) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        query.to_string(),
        "--model".to_string(),
        cfg.model.clone(),
    ];
    if let Some(effort) = cfg.effort {
        args.push("--effort".to_string());
        args.push(effort.as_str().to_string());
    }
    let ccs_str = ccs.display();
    args.extend([
        "--system-prompt".to_string(),
        system_prompt(ccs),
        "--tools".to_string(),
        "Bash".to_string(),
        "--allowedTools".to_string(),
        format!("Bash({ccs_str} search:*)"),
        format!("Bash({ccs_str} show:*)"),
        "--strict-mcp-config".to_string(),
        "--setting-sources".to_string(),
        String::new(),
        "--disable-slash-commands".to_string(),
        "--no-session-persistence".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ]);
    args
}

#[derive(Debug, PartialEq, Eq)]
enum StreamEvent {
    Searches(Vec<String>),
    Final(Result<String, String>),
}

/// `<ccs> search <query>`: the query is a double-quoted, single-quoted or
/// bare token.
static SEARCH_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|/)ccs\s+search\s+("(?:[^"\\]|\\.)*"|'[^']*'|\S+)"#).unwrap()
});

fn search_query_from_command(command: &str) -> Option<String> {
    let token = SEARCH_COMMAND.captures(command)?.get(1)?.as_str();
    let query = if let Some(inner) = token.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        inner.replace("\\\"", "\"")
    } else if let Some(inner) = token.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        inner.to_string()
    } else {
        token.to_string()
    };
    Some(query)
}

fn parse_stream_line(line: &str) -> Option<StreamEvent> {
    let json: serde_json::Value = serde_json::from_str(line).ok()?;
    match json.get("type")?.as_str()? {
        "assistant" => {
            let searches: Vec<String> = json
                .pointer("/message/content")?
                .as_array()?
                .iter()
                .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                .filter_map(|block| block.pointer("/input/command")?.as_str())
                .filter_map(search_query_from_command)
                .collect();
            (!searches.is_empty()).then_some(StreamEvent::Searches(searches))
        }
        "result" => Some(StreamEvent::Final(result_outcome(&json))),
        _ => None,
    }
}

/// `Ok(result)` for a successful run; otherwise the most specific error text
/// the line carries: `result`, then `errors`, then `subtype`.
fn result_outcome(json: &serde_json::Value) -> Result<String, String> {
    let is_error = json
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let result = json.get("result").and_then(|v| v.as_str());
    match (is_error, result) {
        (false, Some(text)) => return Ok(text.to_string()),
        (true, Some(text)) => return Err(text.to_string()),
        _ => {}
    }
    let errors: Vec<&str> = json
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(|e| e.as_str()).collect())
        .unwrap_or_default();
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Err(json
        .get("subtype")
        .and_then(|v| v.as_str())
        .unwrap_or("claude returned an error")
        .to_string())
}

#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
enum ParseError {
    NoJsonArray,
    UnterminatedArray,
    NotHitArray,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ParseError::NoJsonArray => "no JSON array",
            ParseError::UnterminatedArray => "unterminated array",
            ParseError::NotHitArray => "not an array of hits",
        };
        f.write_str(s)
    }
}

const EMPTY_OUTPUT_MARKER: &str = "<empty output>";
const SAMPLE_LIMIT: usize = 100;

/// Find the first `[` and its matching `]` and parse the span as hits.
fn parse_hits(text: &str) -> Result<Vec<AgentHit>, String> {
    parse_hit_array(text).map_err(|err| format_parse_error(err, text))
}

fn parse_hit_array(text: &str) -> Result<Vec<AgentHit>, ParseError> {
    let start = text.find('[').ok_or(ParseError::NoJsonArray)?;

    let mut depth = 0;
    let mut end = None;
    for (i, ch) in text[start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    let end = end.ok_or(ParseError::UnterminatedArray)?;
    let mut hits: Vec<AgentHit> =
        serde_json::from_str(&text[start..end]).map_err(|_| ParseError::NotHitArray)?;
    hits.truncate(MAX_HITS);
    Ok(hits)
}

/// Parse error with a normalized, truncated sample of the agent's answer, so
/// the status bar shows why it could not be read. Char-based truncation keeps
/// it UTF-8 safe.
fn format_parse_error(err: ParseError, text: &str) -> String {
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let sample = if normalized.is_empty() {
        EMPTY_OUTPUT_MARKER.to_string()
    } else if normalized.chars().count() > SAMPLE_LIMIT {
        let mut truncated: String = normalized.chars().take(SAMPLE_LIMIT).collect();
        truncated.push('…');
        truncated
    } else {
        normalized
    };

    format!("{}: {}", err, sample)
}

/// Start the agent in the background. Events arrive on the handle's `rx`.
pub fn spawn_agent_search(
    query: String,
    claude: PathBuf,
    ccs: PathBuf,
    cfg: AgentSearchConfig,
) -> AgentSearchHandle {
    spawn_with_timeout(query, claude, ccs, cfg, AGENT_TIMEOUT)
}

fn spawn_with_timeout(
    query: String,
    claude: PathBuf,
    ccs: PathBuf,
    cfg: AgentSearchConfig,
    timeout: Duration,
) -> AgentSearchHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));

    let args = build_agent_args(&query, &ccs, &cfg);
    let mut child = match spawn_claude(&claude, &args) {
        Ok(child) => child,
        Err(e) => {
            let _ = tx.send(AgentEvent::Done(Err(format!("failed to run claude: {e}"))));
            return AgentSearchHandle {
                rx,
                cancel,
                child: None,
            };
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));

    // Drain stderr so the pipe never fills; its text goes into error messages.
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let stderr_thread = {
        let stderr_text = Arc::clone(&stderr_text);
        thread::spawn(move || drain_stderr(stderr, &stderr_text))
    };

    // The agent is silent for many seconds between lines, so cancel and
    // timeout are enforced here rather than per line.
    let reader_done = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    {
        let child = Arc::clone(&child);
        let cancel = Arc::clone(&cancel);
        let reader_done = Arc::clone(&reader_done);
        let timed_out = Arc::clone(&timed_out);
        thread::spawn(move || watchdog(&child, &cancel, &reader_done, &timed_out, timeout));
    }

    {
        let cancel = Arc::clone(&cancel);
        let child = Arc::clone(&child);
        thread::spawn(move || {
            let _done = SetOnDrop(reader_done);
            let stream = read_stream(stdout, &cancel, &tx);
            let status = wait_for_exit(&child);
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let done = finish(stream.outcome, timed_out.load(Ordering::SeqCst), || {
                exit_report(status, &stderr_thread, &stderr_text, stream.last_lines)
            });
            let _ = tx.send(AgentEvent::Done(done));
        });
    }

    AgentSearchHandle {
        rx,
        cancel,
        child: Some(child),
    }
}

fn drain_stderr(stderr: Option<ChildStderr>, into: &Mutex<String>) {
    if let Some(mut stderr) = stderr {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        if let Ok(mut text) = into.lock() {
            *text = buf;
        }
    }
}

/// Kill the child on cancel or once `timeout` passes; stop when the reader
/// is done.
fn watchdog(
    child: &Mutex<Child>,
    cancel: &AtomicBool,
    reader_done: &AtomicBool,
    timed_out: &AtomicBool,
    timeout: Duration,
) {
    let started = Instant::now();
    while !reader_done.load(Ordering::SeqCst) {
        let expired = started.elapsed() > timeout;
        if expired {
            timed_out.store(true, Ordering::SeqCst);
        }
        if expired || cancel.load(Ordering::SeqCst) {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
            return;
        }
        thread::sleep(WATCHDOG_TICK);
    }
}

/// What the reader kept from the agent's stdout.
struct StreamRead {
    outcome: Option<Result<String, String>>,
    /// The last [`LAST_LINES`] lines, for an agent that ends without a result.
    last_lines: VecDeque<String>,
}

/// Read stream-json lines to EOF, reporting each new search query as progress.
fn read_stream(
    stdout: Option<ChildStdout>,
    cancel: &AtomicBool,
    tx: &Sender<AgentEvent>,
) -> StreamRead {
    let mut queries: Vec<String> = Vec::new();
    let mut read = StreamRead {
        outcome: None,
        last_lines: VecDeque::new(),
    };
    let Some(stdout) = stdout else {
        return read;
    };
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        match parse_stream_line(&line) {
            Some(StreamEvent::Searches(found)) => record_searches(&mut queries, found, cancel, tx),
            Some(StreamEvent::Final(result)) => read.outcome = Some(result),
            None => {}
        }
        if read.last_lines.len() == LAST_LINES {
            read.last_lines.pop_front();
        }
        read.last_lines.push_back(line);
    }
    read
}

/// Add the queries not seen yet; when any was new, send the whole list.
fn record_searches(
    queries: &mut Vec<String>,
    found: Vec<String>,
    cancel: &AtomicBool,
    tx: &Sender<AgentEvent>,
) {
    let before = queries.len();
    for query in found {
        if !queries.contains(&query) {
            queries.push(query);
        }
    }
    if queries.len() > before && !cancel.load(Ordering::SeqCst) {
        let _ = tx.send(AgentEvent::Progress(queries.clone()));
    }
}

/// Why the agent ended without a result: exit status, stderr, last stdout lines.
fn exit_report(
    status: Option<ExitStatus>,
    stderr_thread: &thread::JoinHandle<()>,
    stderr_text: &Mutex<String>,
    last_lines: VecDeque<String>,
) -> String {
    // Give the drain a moment: a grandchild may still hold stderr.
    let deadline = Instant::now() + STDERR_GRACE;
    while !stderr_thread.is_finished() && Instant::now() < deadline {
        thread::sleep(WAIT_POLL);
    }
    let stderr = stderr_text
        .lock()
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let status = status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown status".to_string());
    let last: Vec<String> = last_lines.into_iter().collect();
    format!(
        "claude exited with {status}: {stderr}; last output: {}",
        last.join(" | ")
    )
}

/// The agent's final answer. A received `result` wins over a timeout that
/// fired after it; `exit_report` describes an agent that ended without one.
fn finish(
    outcome: Option<Result<String, String>>,
    timed_out: bool,
    exit_report: impl FnOnce() -> String,
) -> Result<Vec<FoundSession>, String> {
    match outcome {
        Some(Ok(text)) => {
            let found = resolve_hits(parse_hits(&text)?);
            if found.is_empty() {
                Err(AGENT_NO_MATCH_MSG.to_string())
            } else {
                Ok(found)
            }
        }
        Some(Err(msg)) => Err(msg),
        None if timed_out => Err("agent timed out".to_string()),
        None => Err(exit_report()),
    }
}

const WATCHDOG_TICK: Duration = Duration::from_millis(100);
const WAIT_POLL: Duration = Duration::from_millis(50);
const STDERR_GRACE: Duration = Duration::from_millis(500);
const LAST_LINES: usize = 3;
const SPAWN_ATTEMPTS: usize = 5;

/// Sets the flag when dropped, so the watchdog stops even if the reader panics.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Spawn `claude` in its own scratch directory. "Text file busy" is retried:
/// it happens when another thread forks while the binary is still open for
/// writing (seen with freshly written stubs in parallel tests).
fn spawn_claude(claude: &Path, args: &[String]) -> std::io::Result<Child> {
    let cwd = std::env::temp_dir().join("ccs-agent-search");
    std::fs::create_dir_all(&cwd)?;
    let mut attempt = 1;
    loop {
        let spawned = Command::new(claude)
            .args(args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match spawned {
            Err(e) if e.kind() == ErrorKind::ExecutableFileBusy && attempt < SPAWN_ATTEMPTS => {
                attempt += 1;
                thread::sleep(WAIT_POLL);
            }
            other => return other,
        }
    }
}

/// Reap the child without holding its lock across a blocking wait, so
/// `cancel()` can always take the lock and kill it.
fn wait_for_exit(child: &Mutex<Child>) -> Option<ExitStatus> {
    loop {
        match child.lock().ok()?.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => thread::sleep(WAIT_POLL),
            Err(_) => return None,
        }
    }
}

/// Keep hits that point at a readable session file. Opencode rows
/// (`<db>#<id>`) cannot be opened from here and are dropped. Hits in the
/// same session (the agent reports each matching moment) become one row
/// with all their reasons, at the place of the first.
fn resolve_hits(hits: Vec<AgentHit>) -> Vec<FoundSession> {
    let mut found: Vec<FoundSession> = Vec::new();
    for hit in hits.into_iter().filter(|hit| !hit.file_path.contains('#')) {
        let session = match hit.host.as_deref() {
            None | Some("local") => match extract_summary(Path::new(&hit.file_path)) {
                Some(session) => session,
                None => continue,
            },
            Some(host) => remote_hit_session(host, &hit),
        };
        let same = found
            .iter_mut()
            .find(|f| f.session.host == session.host && f.session.session_id == session.session_id);
        match same {
            Some(f) => f.whys.push(hit.why),
            None => found.push(FoundSession {
                session,
                whys: vec![hit.why],
            }),
        }
    }
    found
}

/// A hit on another machine: its file is there, so the row is built from
/// what the agent read in the search output. Enter resumes it over ssh.
fn remote_hit_session(host: &str, hit: &AgentHit) -> RecentSession {
    RecentSession {
        session_id: hit.session_id.clone(),
        file_path: hit.file_path.clone(),
        project: hit.project.clone().unwrap_or_default(),
        source: crate::session::SessionSource::from_path(&hit.file_path),
        timestamp: hit.timestamp.unwrap_or_else(chrono::Utc::now),
        summary: hit.why.clone(),
        automation: None,
        branch: None,
        message_count: None,
        preview_role: crate::session::record::MessageRole::User,
        cwd: None,
        host: crate::remote::Host::Remote(host.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
    }

    #[test]
    fn default_config_build_agent_args_has_lean_flags_and_no_effort() {
        let args = build_agent_args("q", Path::new("/bin/ccs"), &AgentSearchConfig::default());
        assert_eq!(&args[..2], ["-p", "q"]);
        for flag in [
            "--no-session-persistence",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--verbose",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag}");
        }
        assert!(args
            .windows(2)
            .any(|w| w == ["--model", "claude-haiku-5-5"]));
        assert!(args.windows(2).any(|w| w == ["--setting-sources", ""]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"]));
        assert!(args.windows(2).any(|w| w == ["--tools", "Bash"]));
        assert!(args.windows(3).any(|w| w
            == [
                "--allowedTools",
                "Bash(/bin/ccs search:*)",
                "Bash(/bin/ccs show:*)"
            ]));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--system-prompt" && w[1] == system_prompt(Path::new("/bin/ccs"))));
        assert!(!args.iter().any(|a| a == "--effort" || a == "--bare"));
    }

    #[test]
    fn effort_configured_build_agent_args_passes_effort() {
        let cfg = AgentSearchConfig {
            effort: Some(Effort::High),
            ..AgentSearchConfig::default()
        };
        let args = build_agent_args("q", Path::new("/bin/ccs"), &cfg);
        assert!(args.windows(2).any(|w| w == ["--effort", "high"]));
    }

    #[test]
    fn each_config_effort_build_agent_args_passes_it_verbatim() {
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            let cfg: crate::config::Config =
                toml::from_str(&format!("[agent_search]\neffort = \"{effort}\"\n")).unwrap();
            let args = build_agent_args("q", Path::new("/bin/ccs"), &cfg.agent_search);
            assert!(
                args.windows(2).any(|w| w == ["--effort", effort]),
                "{effort}"
            );
        }
    }

    #[test]
    fn query_with_quotes_and_newline_build_agent_args_keeps_it_verbatim() {
        let q = "it's \"kotlin\" $HOME\nline2";
        assert_eq!(
            build_agent_args(q, Path::new("/bin/ccs"), &AgentSearchConfig::default())[1],
            q
        );
    }

    #[test]
    fn ccs_path_system_prompt_names_absolute_binary_and_limit_10() {
        let p = system_prompt(Path::new("/opt/ccs"));
        assert!(p.contains("/opt/ccs search"), "{p}");
        assert!(p.contains("--limit 10"), "{p}");
        assert!(p.contains("/opt/ccs show"), "{p}");
        assert!(p.contains("file_path contains '#'"), "{p}");
    }

    #[test]
    fn assistant_tool_use_line_parse_stream_line_extracts_search_queries() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "Bash",
                 "input": {"command": "/opt/ccs search \"raycast kitty\" --limit 10"}},
                {"type": "tool_use", "name": "Bash",
                 "input": {"command": "/opt/ccs search 'kotlin.*2\\.4' --regex --limit 10"}},
                {"type": "tool_use", "name": "Bash",
                 "input": {"command": "/opt/ccs show /a.jsonl --line 3"}}
            ]}
        })
        .to_string();
        assert_eq!(
            parse_stream_line(&line),
            Some(StreamEvent::Searches(vec![
                "raycast kitty".to_string(),
                "kotlin.*2\\.4".to_string()
            ]))
        );
    }

    #[test]
    fn success_result_line_parse_stream_line_returns_final_text() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"[]"}"#;
        assert_eq!(
            parse_stream_line(line),
            Some(StreamEvent::Final(Ok("[]".to_string())))
        );
    }

    #[test]
    fn error_result_line_parse_stream_line_returns_error_text() {
        let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}"#;
        assert_eq!(
            parse_stream_line(line),
            Some(StreamEvent::Final(Err(
                "Not logged in · Please run /login".to_string()
            )))
        );
    }

    #[test]
    fn error_subtype_result_line_parse_stream_line_returns_errors_joined() {
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["e1","e2"]}"#;
        assert_eq!(
            parse_stream_line(line),
            Some(StreamEvent::Final(Err("e1; e2".to_string())))
        );
    }

    #[test]
    fn error_subtype_without_errors_parse_stream_line_returns_subtype() {
        let line = r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#;
        assert_eq!(
            parse_stream_line(line),
            Some(StreamEvent::Final(Err("error_max_turns".to_string())))
        );
    }

    #[test]
    fn deleted_exe_path_ccs_path_or_proc_exe_uses_proc_pid_exe() {
        assert_eq!(
            ccs_path_or_proc_exe("/h/.cargo/bin/ccs (deleted)".into(), 42),
            PathBuf::from("/proc/42/exe")
        );
    }

    #[test]
    fn live_exe_path_ccs_path_or_proc_exe_keeps_it() {
        assert_eq!(
            ccs_path_or_proc_exe("/h/.cargo/bin/ccs".into(), 42),
            PathBuf::from("/h/.cargo/bin/ccs")
        );
    }

    #[test]
    fn running_test_binary_resolve_ccs_path_returns_existing_file() {
        assert!(resolve_ccs_path().unwrap().exists());
    }

    #[test]
    fn system_and_tool_result_lines_parse_stream_line_ignored() {
        for line in [
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#,
            r#"{"type":"rate_limit_event"}"#,
            "not json",
        ] {
            assert_eq!(parse_stream_line(line), None, "{line}");
        }
    }

    #[test]
    fn prose_around_array_parse_hits_extracts_hits() {
        let text = "Here:\n[{\"session_id\":\"a\",\"file_path\":\"/p\",\"why\":\"w\"}]\nDone";
        assert_eq!(
            parse_hits(text).unwrap(),
            vec![AgentHit {
                host: None,
                project: None,
                timestamp: None,
                session_id: "a".into(),
                file_path: "/p".into(),
                why: "w".into()
            }]
        );
    }

    #[test]
    fn more_than_five_parse_hits_truncates_to_max() {
        let items: Vec<String> = (0..7)
            .map(|i| format!(r#"{{"session_id":"s{i}","file_path":"/p{i}","why":"w"}}"#))
            .collect();
        let text = format!("[{}]", items.join(","));
        assert_eq!(parse_hits(&text).unwrap().len(), MAX_HITS);
    }

    #[test]
    fn prose_only_parse_hits_errors_with_sample() {
        let err = parse_hits("I could not find it").unwrap_err();
        assert!(err.contains("no JSON array"), "{err}");
        assert!(err.contains("I could not find it"), "{err}");
    }

    #[test]
    fn missing_and_opencode_paths_resolve_hits_drops_them() {
        let hits = vec![
            AgentHit {
                host: None,
                project: None,
                timestamp: None,
                session_id: "sess-linear-001".into(),
                file_path: fixture("linear_session.jsonl"),
                why: "sorting lists".into(),
            },
            AgentHit {
                host: None,
                project: None,
                timestamp: None,
                session_id: "gone".into(),
                file_path: "/nope.jsonl".into(),
                why: "w".into(),
            },
            AgentHit {
                host: None,
                project: None,
                timestamp: None,
                session_id: "ses_1".into(),
                file_path: "/db.sqlite#ses_1".into(),
                why: "w".into(),
            },
        ];
        let found = resolve_hits(hits);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session.session_id, "sess-linear-001");
        assert_eq!(found[0].whys, ["sorting lists"]);
    }

    #[test]
    fn remote_hit_resolve_hits_keeps_it_with_host_and_reported_project() {
        let hits: Vec<AgentHit> = serde_json::from_str(
            r#"[{"session_id": "r1", "file_path": "/home/m/.claude/projects/-p/r1.jsonl",
                 "why": "kotlin bump", "host": "Z270XX", "project": "infra",
                 "timestamp": "2026-10-22T11:00:00Z"}]"#,
        )
        .unwrap();
        let found = resolve_hits(hits);
        assert_eq!(found.len(), 1);
        let session = &found[0].session;
        assert_eq!(session.host, crate::remote::Host::Remote("Z270XX".into()));
        assert_eq!(session.project, "infra");
        assert_eq!(session.session_id, "r1");
    }

    #[test]
    fn two_hits_in_one_session_resolve_hits_merges_them_into_one_row() {
        let hit = |id: &str, file: String, why: &str| AgentHit {
            host: None,
            project: None,
            timestamp: None,
            session_id: id.into(),
            file_path: file,
            why: why.into(),
        };
        let hits = vec![
            hit(
                "sess-linear-001",
                fixture("linear_session.jsonl"),
                "sorting",
            ),
            hit(
                "sess-branched-001",
                fixture("branched_session.jsonl"),
                "branch",
            ),
            hit(
                "sess-linear-001",
                fixture("linear_session.jsonl"),
                "reverse",
            ),
        ];
        let found = resolve_hits(hits);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].session.session_id, "sess-linear-001");
        assert_eq!(found[0].whys, ["sorting", "reverse"]);
        assert_eq!(found[1].whys, ["branch"]);
    }

    #[test]
    fn local_host_field_resolve_hits_reads_the_file_here() {
        let hits = vec![AgentHit {
            session_id: "sess-linear-001".into(),
            file_path: fixture("linear_session.jsonl"),
            why: "sorting lists".into(),
            host: Some("local".into()),
            project: None,
            timestamp: None,
        }];
        let found = resolve_hits(hits);
        assert_eq!(found[0].session.host, crate::remote::Host::Local);
    }

    #[test]
    fn system_prompt_when_hosts_then_explains_host_field_and_show_host() {
        let p = system_prompt(Path::new("/opt/ccs"));
        assert!(p.contains("--host"), "{p}");
        assert!(p.contains("\"host\""), "{p}");
    }

    #[cfg(unix)]
    mod process {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::mpsc::RecvTimeoutError;
        use std::time::Instant;

        /// A fake `claude`: prints `stdout_lines` (via a file, so no shell
        /// quoting), then runs `tail` as its last command.
        struct Stub {
            dir: tempfile::TempDir,
            claude: PathBuf,
        }

        fn stub(stdout_lines: &[String], tail: &str) -> Stub {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out.jsonl");
            std::fs::write(&out, stdout_lines.join("\n") + "\n").unwrap();
            let claude = dir.path().join("claude");
            let script = format!(
                "#!/bin/sh\necho $$ > '{}'\ncat '{}'\n{}\n",
                dir.path().join("pid").display(),
                out.display(),
                tail
            );
            std::fs::write(&claude, script).unwrap();
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
            Stub { dir, claude }
        }

        fn search_line(query: &str) -> String {
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [{"type": "tool_use", "name": "Bash",
                    "input": {"command": format!("/x/ccs search \"{query}\" --limit 10")}}]}
            })
            .to_string()
        }

        fn result_line(result: &str) -> String {
            serde_json::json!({"type": "result", "subtype": "success", "is_error": false, "result": result})
                .to_string()
        }

        fn linear_hit_result() -> String {
            result_line(&linear_hit_json())
        }

        fn spawn(stub: &Stub, timeout: Duration) -> AgentSearchHandle {
            spawn_with_timeout(
                "q".into(),
                stub.claude.clone(),
                "/x/ccs".into(),
                AgentSearchConfig::default(),
                timeout,
            )
        }

        fn recv(handle: &AgentSearchHandle) -> AgentEvent {
            handle.rx.recv_timeout(Duration::from_secs(10)).unwrap()
        }

        fn expect_done(event: AgentEvent) -> Result<Vec<FoundSession>, String> {
            match event {
                AgentEvent::Done(result) => result,
                AgentEvent::Progress(p) => panic!("expected Done, got Progress({p:?})"),
            }
        }

        fn wait_for_pid(stub: &Stub) -> i32 {
            let path = stub.dir.path().join("pid");
            let started = Instant::now();
            loop {
                if let Some(pid) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                {
                    return pid;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "stub never started"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn alive(pid: i32) -> bool {
            unsafe { libc::kill(pid, 0) == 0 }
        }

        #[test]
        fn stub_prints_search_then_result_spawn_emits_progress_then_hits() {
            let stub = stub(&[search_line("kitty"), linear_hit_result()], "");
            let handle = spawn(&stub, AGENT_TIMEOUT);
            match recv(&handle) {
                AgentEvent::Progress(queries) => assert_eq!(queries, vec!["kitty".to_string()]),
                AgentEvent::Done(r) => panic!("expected Progress, got Done({:?})", r.err()),
            }
            let hits = expect_done(recv(&handle)).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].session.session_id, "sess-linear-001");
        }

        #[test]
        fn stub_returns_empty_array_spawn_reports_no_match() {
            let stub = stub(&[result_line("[]")], "");
            let handle = spawn(&stub, AGENT_TIMEOUT);
            assert_eq!(expect_done(recv(&handle)).unwrap_err(), AGENT_NO_MATCH_MSG);
        }

        #[test]
        fn stub_exits_without_result_spawn_reports_stderr() {
            let stub = stub(&["partial".to_string()], "echo boom >&2\nexit 3");
            let handle = spawn(&stub, AGENT_TIMEOUT);
            let err = expect_done(recv(&handle)).unwrap_err();
            assert!(err.contains("boom"), "{err}");
            assert!(err.contains("partial"), "{err}");
        }

        #[test]
        fn cancel_during_silent_stub_kills_child_and_sends_no_done() {
            let stub = stub(&[], "exec sleep 30");
            let handle = spawn(&stub, AGENT_TIMEOUT);
            let pid = wait_for_pid(&stub);
            std::thread::sleep(Duration::from_millis(200));
            handle.cancel();
            assert!(matches!(
                handle.rx.recv_timeout(Duration::from_secs(2)),
                Err(RecvTimeoutError::Disconnected)
            ));
            assert!(!alive(pid));
        }

        #[test]
        fn dropping_handle_kills_child() {
            let stub = stub(&[], "exec sleep 30");
            let handle = spawn(&stub, AGENT_TIMEOUT);
            let pid = wait_for_pid(&stub);
            drop(handle);
            let started = Instant::now();
            while alive(pid) {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "stub still alive after drop"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        #[test]
        fn silent_stub_past_timeout_spawn_reports_timed_out() {
            let stub = stub(&[], "exec sleep 30");
            let handle = spawn(&stub, Duration::from_millis(300));
            assert_eq!(expect_done(recv(&handle)).unwrap_err(), "agent timed out");
        }
    }

    fn linear_hit_json() -> String {
        serde_json::json!([{
            "session_id": "sess-linear-001",
            "file_path": fixture("linear_session.jsonl"),
            "why": "sorting lists"
        }])
        .to_string()
    }

    #[test]
    fn result_received_then_timed_out_finish_returns_hits() {
        let hits = finish(Some(Ok(linear_hit_json())), true, || unreachable!()).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn no_result_and_timed_out_finish_reports_timeout() {
        assert_eq!(
            finish(None, true, || unreachable!()).unwrap_err(),
            "agent timed out"
        );
    }

    #[test]
    fn no_result_not_timed_out_finish_uses_exit_report() {
        assert_eq!(
            finish(None, false, || "exit report".to_string()).unwrap_err(),
            "exit report"
        );
    }
}
