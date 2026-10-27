use ccs::remote::client::{
    ClientEvent, ClientTimeouts, HostClient, HostLink, HostState, Reply, Request,
};
use ccs::remote::config::HostConfig;
use ccs::remote::wire::{SearchParams, ShowParams, TreeParams};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

/// Set once: `set_var` while other test threads spawn processes races
/// their environment reads, and a child could start the real ssh.
fn setup_env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(set_env);
}

fn set_env() {
    std::env::set_var(
        "CCS_SSH",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-ssh.sh"),
    );
    std::env::set_var(
        "CCFS_SEARCH_PATH",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"),
    );
}

/// A stand-in for `ccs` on the remote side: a shell script run by fake ssh.
fn script(dir: &tempfile::TempDir, body: &str) -> String {
    let path = dir.path().join("remote-ccs");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().to_string()
}

fn client(ccs_path: &str, timeouts: ClientTimeouts) -> HostClient {
    setup_env();
    HostClient::spawn(
        HostConfig {
            name: "fake".into(),
            ccs_path: ccs_path.into(),
        },
        timeouts,
    )
}

fn fast() -> ClientTimeouts {
    ClientTimeouts {
        // Generous: a debug ccs starting next to seven other test processes.
        hello: Duration::from_secs(5),
        request: Duration::from_secs(5),
        retry: Duration::from_secs(60),
    }
}

/// Poll until `done` says so or `limit` passes; returns every event seen.
fn poll_until(
    c: &mut HostClient,
    limit: Duration,
    mut done: impl FnMut(&HostClient, &[ClientEvent]) -> bool,
) -> Vec<ClientEvent> {
    let deadline = Instant::now() + limit;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        events.extend(c.poll(Instant::now()));
        if done(c, &events) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    events
}

fn ready(c: &mut HostClient) {
    poll_until(c, Duration::from_secs(5), |c, _| {
        c.state() == &HostState::Ready
    });
    assert_eq!(c.state(), &HostState::Ready);
}

fn search_req(q: &str) -> Request {
    Request::Search(SearchParams {
        query: q.into(),
        regex: false,
        project: None,
        full_content: false,
        limit: None,
    })
}

fn reply_for(events: &[ClientEvent], want: u64) -> Option<&Result<Reply, String>> {
    events.iter().find_map(|e| match e {
        ClientEvent::Reply { id, result } if *id == want => Some(result),
        _ => None,
    })
}

#[test]
fn client_when_serve_starts_then_ready_and_search_replies() {
    let mut c = client(env!("CARGO_BIN_EXE_ccs"), fast());
    ready(&mut c);
    let id = c.send(search_req("sort a list")).unwrap();
    let events = poll_until(&mut c, Duration::from_secs(5), |_, ev| {
        reply_for(ev, id).is_some()
    });
    match reply_for(&events, id) {
        Some(Ok(Reply::Search(found))) => assert!(!found.groups.is_empty()),
        other => panic!("unexpected reply {other:?}"),
    }
}

#[test]
fn client_when_noise_precedes_hello_then_ready() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(
        &dir,
        &format!("echo noise\nexec {} \"$@\"", env!("CARGO_BIN_EXE_ccs")),
    );
    let mut c = client(&path, fast());
    ready(&mut c);
}

#[test]
fn client_when_program_silent_then_down_after_hello_deadline() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(&dir, "sleep 30");
    let mut timeouts = fast();
    timeouts.hello = Duration::from_millis(300);
    let mut c = client(&path, timeouts);
    poll_until(&mut c, Duration::from_secs(2), |c, _| {
        matches!(c.state(), HostState::Down { .. })
    });
    assert!(
        matches!(c.state(), HostState::Down { .. }),
        "{:?}",
        c.state()
    );
}

#[test]
fn client_when_protocol_differs_then_incompatible_with_version() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(
        &dir,
        r#"echo '{"type":"hello","protocol":99,"ccs_version":"9.9.9"}'; sleep 5"#,
    );
    let mut c = client(&path, fast());
    poll_until(&mut c, Duration::from_secs(3), |c, _| {
        matches!(c.state(), HostState::Incompatible { .. })
    });
    assert_eq!(
        c.state(),
        &HostState::Incompatible {
            version: Some("9.9.9".into())
        }
    );
}

#[test]
fn client_when_old_ccs_then_incompatible_without_version() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(
        &dir,
        "echo \"error: unrecognized subcommand 'serve'\" >&2; exit 2",
    );
    let mut c = client(&path, fast());
    poll_until(&mut c, Duration::from_secs(3), |c, _| {
        c.state() != &HostState::Connecting
    });
    assert_eq!(c.state(), &HostState::Incompatible { version: None });
}

#[test]
fn client_when_eof_mid_line_then_down_and_pending_failed() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(
        &dir,
        r#"echo '{"type":"hello","protocol":1,"ccs_version":"x"}'; read l; printf '{"id":1,'; exit 0"#,
    );
    let mut c = client(&path, fast());
    ready(&mut c);
    let id = c.send(search_req("x")).unwrap();
    let events = poll_until(&mut c, Duration::from_secs(3), |c, _| {
        matches!(c.state(), HostState::Down { .. })
    });
    assert!(matches!(c.state(), HostState::Down { .. }));
    assert!(matches!(reply_for(&events, id), Some(Err(_))));
}

#[test]
fn client_when_request_times_out_then_only_that_request_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(
        &dir,
        r#"echo '{"type":"hello","protocol":1,"ccs_version":"x"}'; sleep 30"#,
    );
    let mut timeouts = fast();
    timeouts.request = Duration::from_millis(200);
    let mut c = client(&path, timeouts);
    ready(&mut c);
    let id = c.send(search_req("x")).unwrap();
    let events = poll_until(&mut c, Duration::from_secs(2), |_, ev| {
        reply_for(ev, id).is_some()
    });
    assert_eq!(reply_for(&events, id), Some(&Err("timeout".to_string())));
    assert_eq!(c.state(), &HostState::Ready);
}

#[test]
fn client_when_cancelled_then_late_reply_dropped() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(
        &dir,
        r#"echo '{"type":"hello","protocol":1,"ccs_version":"x"}'; read a; sleep 0.3; echo '{"id":1,"result":{"content":"c","text_content":"t"}}'; sleep 2"#,
    );
    let mut c = client(&path, fast());
    ready(&mut c);
    let id = c
        .send(Request::Show(ShowParams {
            file_path: "/f".into(),
            line: Some(1),
            uuid: None,
        }))
        .unwrap();
    // The server answers on its own 0.3 s after the request, cancel or not.
    c.cancel(id);
    let events = poll_until(&mut c, Duration::from_millis(800), |_, _| false);
    assert!(reply_for(&events, id).is_none());
}

fn linear_session() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/linear_session.jsonl"
    )
    .to_string()
}

#[test]
fn client_when_show_and_tree_sent_then_each_reply_has_its_kind() {
    let mut c = client(env!("CARGO_BIN_EXE_ccs"), fast());
    ready(&mut c);
    let show = c
        .send(Request::Show(ShowParams {
            file_path: linear_session(),
            line: Some(1),
            uuid: None,
        }))
        .unwrap();
    let tree = c
        .send(Request::Tree(TreeParams {
            file_path: linear_session(),
        }))
        .unwrap();
    let events = poll_until(&mut c, Duration::from_secs(5), |_, ev| {
        reply_for(ev, show).is_some() && reply_for(ev, tree).is_some()
    });
    match reply_for(&events, show) {
        Some(Ok(Reply::Show(shown))) => assert!(shown.text_content.contains("sort a list")),
        other => panic!("unexpected show reply {other:?}"),
    }
    match reply_for(&events, tree) {
        Some(Ok(Reply::Tree(found))) => assert_eq!(found.session_id, "sess-linear-001"),
        other => panic!("unexpected tree reply {other:?}"),
    }
}

#[test]
fn client_when_reply_has_wrong_shape_then_bad_reply_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = script(
        &dir,
        r#"echo '{"type":"hello","protocol":1,"ccs_version":"x"}'; read a; echo '{"id":1,"result":{"rows":7}}'; sleep 2"#,
    );
    let mut c = client(&path, fast());
    ready(&mut c);
    let id = c
        .send(Request::Tree(TreeParams {
            file_path: "/f".into(),
        }))
        .unwrap();
    let events = poll_until(&mut c, Duration::from_secs(3), |_, ev| {
        reply_for(ev, id).is_some()
    });
    match reply_for(&events, id) {
        Some(Err(e)) => assert!(e.starts_with("bad reply: "), "{e}"),
        other => panic!("unexpected reply {other:?}"),
    }
}
