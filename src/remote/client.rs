//! One connection to one remote host: `ssh <host> ccs serve --stdio`, kept
//! open while the picker runs. The client owns the host's state; callers
//! `poll` it and get back what changed.

use super::config::HostConfig;
use super::ssh::{ssh_command, SshMode};
use super::wire::{
    RecentParams, SearchParams, ShowParams, TreeParams, WireCancel, WireHello, WireRecent,
    WireRequest, WireResponse, WireSearch, WireShow, WireTree, PROTOCOL_VERSION,
};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lines of ssh's stderr kept for the "why is this host down" detail.
const STDERR_TAIL_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub enum HostState {
    Connecting,
    Ready,
    Down {
        reason: String,
    },
    /// `version` is known only when a hello arrived with another protocol.
    Incompatible {
        version: Option<String>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ClientTimeouts {
    /// From spawning ssh to the server's hello. ccs keeps its own deadline
    /// because a live ControlMaster ignores ssh's `ConnectTimeout`.
    pub hello: Duration,
    /// One request; failing it leaves the host up.
    pub request: Duration,
    /// Wait before reconnecting a host that went down.
    pub retry: Duration,
}

impl Default for ClientTimeouts {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(3),
            request: Duration::from_secs(10),
            retry: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Search(SearchParams),
    Recent(RecentParams),
    Show(ShowParams),
    Tree(TreeParams),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Search(WireSearch),
    Recent(Vec<WireRecent>),
    Show(WireShow),
    Tree(WireTree),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    State(HostState),
    Hello {
        ccs_version: String,
    },
    Reply {
        id: u64,
        result: Result<Reply, String>,
    },
}

/// What the picker needs from a host connection; a fake stands in for it in tests.
pub trait HostLink: Send {
    fn name(&self) -> &str;
    fn state(&self) -> &HostState;
    /// Send a request; `None` unless the host is `Ready`.
    fn send(&mut self, req: Request) -> Option<u64>;
    /// Forget the request; a late reply for it is dropped.
    fn cancel(&mut self, id: u64);
    /// Advance deadlines and read what arrived.
    fn poll(&mut self, now: Instant) -> Vec<ClientEvent>;
    /// Reconnect a host that is down or incompatible, without waiting.
    fn reconnect_now(&mut self);
}

#[derive(Clone, Copy)]
enum Kind {
    Search,
    Recent,
    Show,
    Tree,
}

struct Pending {
    kind: Kind,
    sent: Instant,
}

enum Incoming {
    Hello(WireHello),
    Response(WireResponse),
    Garbage(String),
    Eof,
}

struct Connection {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Incoming>,
    started: Instant,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

pub struct HostClient {
    config: HostConfig,
    timeouts: ClientTimeouts,
    state: HostState,
    conn: Option<Connection>,
    down_since: Option<Instant>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// Events raised outside `poll` (a failed write in `send`/`cancel`, a
    /// failed spawn, a reconnect); `poll` hands them out first.
    queued: Vec<ClientEvent>,
}

impl HostClient {
    pub fn spawn(config: HostConfig, timeouts: ClientTimeouts) -> Self {
        let mut client = Self {
            config,
            timeouts,
            state: HostState::Connecting,
            conn: None,
            down_since: None,
            next_id: 0,
            pending: HashMap::new(),
            queued: Vec::new(),
        };
        let mut events = Vec::new();
        client.connect(Instant::now(), &mut events);
        client.queued = events;
        client
    }

    fn connect(&mut self, now: Instant, events: &mut Vec<ClientEvent>) {
        self.close();
        let mut command = ssh_command(&self.config, &SshMode::Serve);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        detach_from_terminal(&mut command);
        match command.spawn() {
            Ok(child) => {
                self.conn = Some(Connection::start(child, now));
                self.set_state(HostState::Connecting, now, events);
            }
            Err(e) => {
                let reason = format!("cannot start ssh: {e}");
                self.set_state(HostState::Down { reason }, now, events);
            }
        }
    }

    fn set_state(&mut self, state: HostState, now: Instant, events: &mut Vec<ClientEvent>) {
        if self.state == state {
            return;
        }
        self.down_since = matches!(state, HostState::Down { .. }).then_some(now);
        self.state = state.clone();
        events.push(ClientEvent::State(state));
    }

    /// Close the connection: stdin first so the server sees EOF, then kill
    /// and reap ssh so no zombie is left.
    fn close(&mut self) {
        if let Some(mut conn) = self.conn.take() {
            drop(conn.stdin);
            let _ = conn.child.kill();
            let _ = conn.child.wait();
        }
    }

    fn go_down(&mut self, reason: String, now: Instant, events: &mut Vec<ClientEvent>) {
        self.close();
        self.fail_pending(&reason, events);
        self.set_state(HostState::Down { reason }, now, events);
    }

    fn fail_pending(&mut self, reason: &str, events: &mut Vec<ClientEvent>) {
        let mut ids: Vec<u64> = self.pending.drain().map(|(id, _)| id).collect();
        ids.sort_unstable();
        events.extend(ids.into_iter().map(|id| ClientEvent::Reply {
            id,
            result: Err(reason.to_string()),
        }));
    }

    fn write(&mut self, line: String, now: Instant, events: &mut Vec<ClientEvent>) -> bool {
        let Some(conn) = self.conn.as_mut() else {
            return false;
        };
        let ok = writeln!(conn.stdin, "{line}").and_then(|_| conn.stdin.flush());
        if let Err(e) = ok {
            self.go_down(format!("write failed: {e}"), now, events);
            return false;
        }
        true
    }

    fn drain_incoming(&mut self, now: Instant, events: &mut Vec<ClientEvent>) {
        loop {
            let Some(conn) = self.conn.as_ref() else {
                return;
            };
            let Ok(incoming) = conn.rx.try_recv() else {
                return;
            };
            match incoming {
                Incoming::Hello(hello) => self.on_hello(hello, now, events),
                Incoming::Response(response) => self.on_response(response, events),
                Incoming::Garbage(line) => {
                    self.go_down(format!("protocol error: {line}"), now, events)
                }
                Incoming::Eof => self.on_eof(now, events),
            }
        }
    }

    fn on_hello(&mut self, hello: WireHello, now: Instant, events: &mut Vec<ClientEvent>) {
        if hello.protocol != PROTOCOL_VERSION {
            self.close();
            let version = Some(hello.ccs_version);
            self.set_state(HostState::Incompatible { version }, now, events);
            return;
        }
        self.set_state(HostState::Ready, now, events);
        events.push(ClientEvent::Hello {
            ccs_version: hello.ccs_version,
        });
    }

    fn on_response(&mut self, response: WireResponse, events: &mut Vec<ClientEvent>) {
        let Some(pending) = self.pending.remove(&response.id) else {
            return;
        };
        let result = match (response.result, response.error) {
            (_, Some(error)) => Err(error),
            (Some(value), None) => decode(pending.kind, value),
            (None, None) => Err("empty reply".to_string()),
        };
        events.push(ClientEvent::Reply {
            id: response.id,
            result,
        });
    }

    fn on_eof(&mut self, now: Instant, events: &mut Vec<ClientEvent>) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        let code = conn.child.wait().ok().and_then(|s| s.code());
        let tail = conn.tail();
        let state = if self.state == HostState::Connecting {
            classify_exit_before_hello(code, &tail)
        } else {
            HostState::Down {
                reason: if tail.is_empty() {
                    "connection closed".to_string()
                } else {
                    tail
                },
            }
        };
        let reason = match &state {
            HostState::Down { reason } => reason.clone(),
            _ => "incompatible ccs".to_string(),
        };
        self.fail_pending(&reason, events);
        self.set_state(state, now, events);
    }

    /// ssh exited but stdout has not reached EOF: on macOS a pipe end can
    /// leak into a process spawned at the same moment by another thread
    /// (there is no `pipe2`), which keeps the pipe open. The exit decides.
    fn check_exit(&mut self, now: Instant, events: &mut Vec<ClientEvent>) {
        let exited = self
            .conn
            .as_mut()
            .is_some_and(|c| matches!(c.child.try_wait(), Ok(Some(_))));
        if exited {
            // Lines written before the exit are already queued; read them first.
            std::thread::sleep(Duration::from_millis(20));
            self.drain_incoming(now, events);
            self.on_eof(now, events);
        }
    }

    fn check_deadlines(&mut self, now: Instant, events: &mut Vec<ClientEvent>) {
        let hello_late = self.state == HostState::Connecting
            && self
                .conn
                .as_ref()
                .is_some_and(|c| now.duration_since(c.started) >= self.timeouts.hello);
        if hello_late {
            let secs = self.timeouts.hello.as_secs_f32();
            let tail = self.conn.as_ref().map(Connection::tail).unwrap_or_default();
            let mut reason = format!("no answer from ccs serve in {secs:.1} s");
            if !tail.is_empty() {
                reason.push_str(&format!(": {tail}"));
            }
            self.go_down(reason, now, events);
        }
        let late: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| now.duration_since(p.sent) >= self.timeouts.request)
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            self.pending.remove(&id);
            events.push(ClientEvent::Reply {
                id,
                result: Err("timeout".to_string()),
            });
        }
        let retry_due = self
            .down_since
            .is_some_and(|since| now.duration_since(since) >= self.timeouts.retry);
        if retry_due {
            self.connect(now, events);
        }
    }
}

impl HostLink for HostClient {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn state(&self) -> &HostState {
        &self.state
    }

    fn send(&mut self, req: Request) -> Option<u64> {
        if self.state != HostState::Ready {
            return None;
        }
        self.next_id += 1;
        let id = self.next_id;
        let (kind, method, params) = encode(req);
        let line = serde_json::to_string(&WireRequest {
            id,
            method: method.to_string(),
            params,
        })
        .ok()?;
        let now = Instant::now();
        let mut events = Vec::new();
        let written = self.write(line, now, &mut events);
        self.queued.extend(events);
        if !written {
            return None;
        }
        self.pending.insert(id, Pending { kind, sent: now });
        Some(id)
    }

    fn cancel(&mut self, id: u64) {
        if self.pending.remove(&id).is_none() {
            return;
        }
        if let Ok(line) = serde_json::to_string(&WireCancel::new(id)) {
            let mut events = Vec::new();
            self.write(line, Instant::now(), &mut events);
            self.queued.extend(events);
        }
    }

    fn poll(&mut self, now: Instant) -> Vec<ClientEvent> {
        let mut events = std::mem::take(&mut self.queued);
        self.drain_incoming(now, &mut events);
        self.check_exit(now, &mut events);
        self.check_deadlines(now, &mut events);
        events
    }

    fn reconnect_now(&mut self) {
        if matches!(
            self.state,
            HostState::Down { .. } | HostState::Incompatible { .. }
        ) {
            let mut events = Vec::new();
            self.connect(Instant::now(), &mut events);
            self.queued.extend(events);
        }
    }
}

impl Drop for HostClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn encode(req: Request) -> (Kind, &'static str, serde_json::Value) {
    let to_value = |v: serde_json::Result<serde_json::Value>| v.unwrap_or_default();
    match req {
        Request::Search(p) => (Kind::Search, "search", to_value(serde_json::to_value(p))),
        Request::Recent(p) => (Kind::Recent, "recent", to_value(serde_json::to_value(p))),
        Request::Show(p) => (Kind::Show, "show", to_value(serde_json::to_value(p))),
        Request::Tree(p) => (Kind::Tree, "tree", to_value(serde_json::to_value(p))),
    }
}

fn decode(kind: Kind, value: serde_json::Value) -> Result<Reply, String> {
    let bad = |e: serde_json::Error| format!("bad reply: {e}");
    Ok(match kind {
        Kind::Search => Reply::Search(serde_json::from_value(value).map_err(bad)?),
        Kind::Recent => Reply::Recent(serde_json::from_value(value).map_err(bad)?),
        Kind::Show => Reply::Show(serde_json::from_value(value).map_err(bad)?),
        Kind::Tree => Reply::Tree(serde_json::from_value(value).map_err(bad)?),
    })
}

/// ssh exited before the server said hello: 255 is ssh itself (no route,
/// refused, bad key); 127 or "command not found" on stderr is a missing
/// `ccs`; another non-zero code is a `ccs` that does not know `serve`; exit 0
/// or death by a signal is a connection that closed.
pub fn classify_exit_before_hello(code: Option<i32>, stderr_tail: &str) -> HostState {
    let missing = code == Some(127) || stderr_tail.contains("command not found");
    match code {
        Some(255) => HostState::Down {
            reason: non_empty(stderr_tail, "ssh failed"),
        },
        _ if missing => HostState::Down {
            reason: "ccs не найден".to_string(),
        },
        Some(c) if c != 0 => HostState::Incompatible { version: None },
        _ => HostState::Down {
            reason: non_empty(stderr_tail, "connection closed"),
        },
    }
}

fn non_empty(text: &str, fallback: &str) -> String {
    if text.trim().is_empty() {
        fallback.to_string()
    } else {
        text.trim().to_string()
    }
}

impl Connection {
    fn start(mut child: Child, now: Instant) -> Self {
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || read_stdout(BufReader::new(stdout), tx));
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let tail = stderr_tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut tail = tail.lock().unwrap_or_else(|p| p.into_inner());
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
        Self {
            child,
            stdin,
            rx,
            started: now,
            stderr_tail,
        }
    }

    fn tail(&self) -> String {
        // Give the stderr reader a moment to catch the last lines.
        std::thread::sleep(Duration::from_millis(20));
        let tail = self.stderr_tail.lock().unwrap_or_else(|p| p.into_inner());
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Lines before the hello that are not a hello (shell rc noise) are skipped;
/// after it, a line that is not a reply means the channel is broken.
fn read_stdout(reader: impl BufRead, tx: Sender<Incoming>) {
    let mut greeted = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let incoming = if greeted {
            match serde_json::from_str::<WireResponse>(&line) {
                Ok(response) => Incoming::Response(response),
                Err(_) => Incoming::Garbage(line),
            }
        } else {
            match serde_json::from_str::<WireHello>(&line) {
                Ok(hello) if hello.r#type == "hello" => {
                    greeted = true;
                    Incoming::Hello(hello)
                }
                _ => continue,
            }
        };
        if tx.send(incoming).is_err() {
            return;
        }
    }
    let _ = tx.send(Incoming::Eof);
}

/// Put ssh in its own process group: Ctrl+C typed while a resumed session
/// starts goes to the terminal's foreground group and would kill it.
#[cfg(unix)]
fn detach_from_terminal(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach_from_terminal(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Ready client whose ssh already exited, so the next write fails.
    fn client_with_dead_pipe() -> HostClient {
        let child = Command::new("true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut conn = Connection::start(child, Instant::now());
        let _ = conn.child.wait();
        HostClient {
            config: HostConfig {
                name: "Z".into(),
                ccs_path: "ccs".into(),
            },
            timeouts: ClientTimeouts::default(),
            state: HostState::Ready,
            conn: Some(conn),
            down_since: None,
            next_id: 2,
            pending: HashMap::from([
                (
                    1,
                    Pending {
                        kind: Kind::Search,
                        sent: Instant::now(),
                    },
                ),
                (
                    2,
                    Pending {
                        kind: Kind::Show,
                        sent: Instant::now(),
                    },
                ),
            ]),
            queued: Vec::new(),
        }
    }

    #[test]
    fn cancel_when_pipe_closed_then_poll_reports_down_and_fails_pending() {
        let mut client = client_with_dead_pipe();
        client.cancel(1);
        let events = client.poll(Instant::now());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ClientEvent::State(HostState::Down { .. }))),
            "{events:?}"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            ClientEvent::Reply {
                id: 2,
                result: Err(_)
            }
        )));
    }

    #[test]
    fn classify_exit_before_hello_when_255_then_down() {
        assert!(matches!(
            classify_exit_before_hello(Some(255), "ssh: connect to host z port 22: No route"),
            HostState::Down { reason } if reason.contains("No route")
        ));
    }

    #[test]
    fn classify_exit_before_hello_when_127_then_down_ccs_not_found() {
        assert_eq!(
            classify_exit_before_hello(Some(127), "sh: ccs: command not found"),
            HostState::Down {
                reason: "ccs не найден".into()
            }
        );
    }

    #[test]
    fn classify_exit_before_hello_when_clap_exit_2_then_incompatible_without_version() {
        assert_eq!(
            classify_exit_before_hello(Some(2), "error: unrecognized subcommand 'serve'"),
            HostState::Incompatible { version: None }
        );
    }
}
