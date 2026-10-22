//! Remote hosts as the picker sees them: one link per host, the requests in
//! flight, the latest search results per host and what the status bar says.

use crate::remote::client::{ClientEvent, HostLink, HostState, Request};
use crate::remote::Host;
use crate::search::SessionGroup;
use std::collections::HashMap;
use std::time::Instant;

/// Which hosts the picker shows and asks (Ctrl+L).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HostFilter {
    #[default]
    All,
    Local,
    Remote,
    Only(String),
}

impl HostFilter {
    pub fn allows(&self, host: &Host) -> bool {
        match (self, host) {
            (HostFilter::All, _) => true,
            (HostFilter::Local, h) => h.is_local(),
            (HostFilter::Remote, h) => !h.is_local(),
            (HostFilter::Only(name), Host::Remote(h)) => h == name,
            (HostFilter::Only(_), Host::Local) => false,
        }
    }

    fn allows_remote(&self, name: &str) -> bool {
        self.allows(&Host::Remote(name.to_string()))
    }
}

/// What the status bar shows for one host.
#[derive(Debug, Clone, PartialEq)]
pub struct HostStatus {
    pub state: HostState,
    /// Remote `ccs` version from its hello, when it differs from ours.
    pub other_version: Option<String>,
    /// Groups the host sent for the shown search.
    pub results: Option<usize>,
    /// A search was sent and has not been answered.
    pub waiting: bool,
    /// Last failed request (timeout, server error).
    pub last_error: Option<String>,
}

impl HostStatus {
    fn new(state: HostState) -> Self {
        Self {
            state,
            other_version: None,
            results: None,
            waiting: false,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PendingKind {
    Search { seq: u64 },
    Recent,
}

/// One host's answer to the search with this `seq`.
pub(crate) struct RemoteSearch {
    pub seq: u64,
    pub groups: Vec<SessionGroup>,
}

#[derive(Default)]
pub struct HostsState {
    pub(crate) links: Vec<Box<dyn HostLink>>,
    pub filter: HostFilter,
    pub(crate) pending: HashMap<(String, u64), PendingKind>,
    pub(crate) searches: HashMap<String, RemoteSearch>,
    pub status: HashMap<String, HostStatus>,
}

impl HostsState {
    pub(crate) fn new(links: Vec<Box<dyn HostLink>>) -> Self {
        let status = links
            .iter()
            .map(|l| (l.name().to_string(), HostStatus::new(l.state().clone())))
            .collect();
        Self {
            links,
            status,
            ..Self::default()
        }
    }

    pub fn has_hosts(&self) -> bool {
        !self.links.is_empty()
    }

    /// Host names in config order.
    pub fn names(&self) -> Vec<String> {
        self.links.iter().map(|l| l.name().to_string()).collect()
    }

    /// Send `request` to one host if it is up and the filter allows it.
    pub(crate) fn send_to(&mut self, name: &str, request: Request, kind: PendingKind) -> bool {
        if !self.filter.allows_remote(name) {
            return false;
        }
        let Some(link) = self.links.iter_mut().find(|l| l.name() == name) else {
            return false;
        };
        let Some(id) = link.send(request) else {
            return false;
        };
        self.pending.insert((name.to_string(), id), kind);
        true
    }

    /// Send the search `seq` to every host that is up and allowed.
    pub(crate) fn send_search(&mut self, seq: u64, request: &Request) {
        self.cancel_searches();
        for name in self.names() {
            let sent = self.send_to(&name, request.clone(), PendingKind::Search { seq });
            if let Some(status) = self.status.get_mut(&name) {
                status.waiting = sent;
                status.results = None;
            }
        }
    }

    /// Cancel every search in flight; their late replies are dropped.
    pub(crate) fn cancel_searches(&mut self) {
        self.cancel_where(|kind| matches!(kind, PendingKind::Search { .. }));
    }

    /// Cancel every `recent` in flight: a reply for an older project scope
    /// must not land after the one the user asked for last.
    pub(crate) fn cancel_recents(&mut self) {
        self.cancel_where(|kind| matches!(kind, PendingKind::Recent));
    }

    fn cancel_where(&mut self, which: impl Fn(&PendingKind) -> bool) {
        let keys: Vec<(String, u64)> = self
            .pending
            .iter()
            .filter(|(_, kind)| which(kind))
            .map(|(key, _)| key.clone())
            .collect();
        for (name, id) in keys {
            let kind = self.pending.remove(&(name.clone(), id));
            if let Some(link) = self.links.iter_mut().find(|l| l.name() == name) {
                link.cancel(id);
            }
            if matches!(kind, Some(PendingKind::Search { .. })) {
                if let Some(status) = self.status.get_mut(&name) {
                    status.waiting = false;
                }
            }
        }
    }

    /// Advance every link and collect what happened, tagged with the host.
    pub(crate) fn poll(&mut self, now: Instant) -> Vec<(String, ClientEvent)> {
        let mut events = Vec::new();
        for link in &mut self.links {
            let name = link.name().to_string();
            events.extend(link.poll(now).into_iter().map(|e| (name.clone(), e)));
        }
        events
    }

    /// Groups from every host whose answer belongs to search `seq`.
    pub(crate) fn groups_for(&self, seq: u64) -> impl Iterator<Item = &SessionGroup> {
        self.searches
            .values()
            .filter(move |s| s.seq == seq)
            .flat_map(|s| s.groups.iter())
    }

    /// Status text per host for the status bar, in config order.
    pub fn status_texts(&self) -> Vec<String> {
        self.names()
            .into_iter()
            .filter_map(|name| {
                let status = self.status.get(&name)?;
                Some(format!("{name}: {}", status_word(status)))
            })
            .collect()
    }

    /// Why hosts are down, for the detail line under the status bar.
    pub fn down_reasons(&self) -> Vec<String> {
        self.names()
            .into_iter()
            .filter_map(|name| match &self.status.get(&name)?.state {
                HostState::Down { reason } => Some(format!("{name}: {reason}")),
                _ => None,
            })
            .collect()
    }
}

fn status_word(status: &HostStatus) -> String {
    let word = match &status.state {
        HostState::Connecting => "…".to_string(),
        HostState::Down { reason } if reason == "ccs не найден" => reason.clone(),
        HostState::Down { .. } => "недоступен".to_string(),
        HostState::Incompatible { .. } => "обнови ccs".to_string(),
        HostState::Ready if status.waiting => "…".to_string(),
        // A failed request leaves the host up, but its answer is missing.
        HostState::Ready if status.last_error.is_some() => {
            let error: String = status
                .last_error
                .as_deref()
                .unwrap_or_default()
                .chars()
                .take(40)
                .collect();
            format!("ошибка ({error})")
        }
        HostState::Ready => status
            .results
            .map(|n| n.to_string())
            .unwrap_or_else(|| "ok".to_string()),
    };
    match &status.other_version {
        Some(v) if status.state == HostState::Ready => format!("{word} (ccs {v})"),
        _ => word,
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use crate::remote::wire::{WireGroup, WireMatch, WireSearch};
    use chrono::{TimeZone, Utc};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// A host link that records what was sent and plays back scripted events.
    #[derive(Clone)]
    pub(crate) struct FakeLink {
        pub name: String,
        pub state: HostState,
        pub sent: Arc<Mutex<Vec<(u64, Request)>>>,
        pub cancelled: Arc<Mutex<Vec<u64>>>,
        pub events: Arc<Mutex<VecDeque<ClientEvent>>>,
        next_id: u64,
    }

    impl FakeLink {
        pub(crate) fn new(name: &str, state: HostState) -> Self {
            Self {
                name: name.to_string(),
                state,
                sent: Arc::default(),
                cancelled: Arc::default(),
                events: Arc::default(),
                next_id: 0,
            }
        }

        pub(crate) fn push(&self, event: ClientEvent) {
            self.events.lock().unwrap().push_back(event);
        }

        pub(crate) fn last_sent(&self) -> Option<(u64, Request)> {
            self.sent.lock().unwrap().last().cloned()
        }
    }

    /// A one-group search answer for `session_id` at `hour`:00.
    pub(crate) fn wire_search(session_id: &str, hour: u32) -> WireSearch {
        let ts = Utc.with_ymd_and_hms(2026, 10, 22, hour, 0, 0).unwrap();
        let file_path = format!("/home/m/.claude/projects/-p/{session_id}.jsonl");
        WireSearch {
            groups: vec![WireGroup {
                session_id: session_id.into(),
                file_path: file_path.clone(),
                project: "remote-proj".into(),
                automation: None,
                message_count: Some(7),
                message_count_compacted: false,
                matches: vec![WireMatch {
                    session_id: session_id.into(),
                    role: "user".into(),
                    content: "needle".into(),
                    text_content: "needle".into(),
                    content_truncated: false,
                    timestamp: ts,
                    branch: None,
                    file_path: Some(file_path),
                    line_number: 1,
                    uuid: Some("u1".into()),
                    parent_uuid: None,
                    on_latest_chain: true,
                }],
                matches_total: 1,
            }],
            truncated: false,
        }
    }

    impl HostLink for FakeLink {
        fn name(&self) -> &str {
            &self.name
        }
        fn state(&self) -> &HostState {
            &self.state
        }
        fn send(&mut self, req: Request) -> Option<u64> {
            if self.state != HostState::Ready {
                return None;
            }
            self.next_id += 1;
            self.sent.lock().unwrap().push((self.next_id, req));
            Some(self.next_id)
        }
        fn cancel(&mut self, id: u64) {
            self.cancelled.lock().unwrap().push(id);
        }
        fn poll(&mut self, _now: Instant) -> Vec<ClientEvent> {
            let events: Vec<ClientEvent> = self.events.lock().unwrap().drain(..).collect();
            for event in &events {
                if let ClientEvent::State(state) = event {
                    self.state = state.clone();
                }
            }
            events
        }
        fn reconnect_now(&mut self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{wire_search, FakeLink};
    use super::*;
    use crate::remote::client::Reply;
    use crate::tui::App;

    fn app_with(link: &FakeLink) -> App {
        App::new(vec!["/nonexistent-ccs-test".to_string()])
            .with_host_links(vec![Box::new(link.clone())])
    }

    fn type_query(app: &mut App, query: &str) {
        app.input.set_text(query);
        app.start_search();
    }

    fn reply_search(link: &FakeLink, app: &mut App, session_id: &str, hour: u32) {
        let (id, _) = link.last_sent().expect("a search was sent");
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(wire_search(session_id, hour))),
        });
        app.poll_hosts();
    }

    fn local_result(app: &mut App, groups: Vec<SessionGroup>) {
        app.deliver_local_groups_for_test(groups);
    }

    #[test]
    fn remote_reply_when_same_seq_after_local_then_merged_and_cursor_kept() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        local_result(
            &mut app,
            vec![
                crate::tui::state::test_group("l1", "/l/l1.jsonl", 12),
                crate::tui::state::test_group("l2", "/l/l2.jsonl", 10),
            ],
        );
        app.search.group_cursor = 1;
        reply_search(&link, &mut app, "r1", 11);
        assert_eq!(app.search.groups.len(), 3);
        let selected = &app.search.groups[app.search.group_cursor];
        assert_eq!(
            (selected.host.clone(), selected.session_id.as_str()),
            (Host::Local, "l2")
        );
        assert!(app
            .search
            .groups
            .iter()
            .any(|g| g.host == Host::Remote("Z".into()) && g.session_id == "r1"));
    }

    #[test]
    fn remote_first_then_local_when_cursor_untouched_then_cursor_stays_on_top() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        reply_search(&link, &mut app, "r1", 9);
        local_result(
            &mut app,
            vec![crate::tui::state::test_group("l1", "/l/l1.jsonl", 12)],
        );
        assert_eq!(app.search.group_cursor, 0);
        assert_eq!(app.search.groups[0].session_id, "l1");
    }

    #[test]
    fn local_answer_when_remote_was_truncated_then_flag_kept() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        let (id, _) = link.last_sent().unwrap();
        let mut found = wire_search("r1", 9);
        found.truncated = true;
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(found)),
        });
        app.poll_hosts();
        local_result(
            &mut app,
            vec![crate::tui::state::test_group("l1", "/l/l1.jsonl", 12)],
        );
        assert!(app.search.search_truncated);
    }

    #[test]
    fn remote_group_when_capped_then_results_count_uses_host_total() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        let (id, _) = link.last_sent().unwrap();
        let mut found = wire_search("r1", 9);
        found.groups[0].matches_total = 25;
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(found)),
        });
        app.poll_hosts();
        assert_eq!(app.search.results_count, 25);
        assert!(app.search.search_truncated);
    }

    #[test]
    fn remote_reply_when_stale_seq_then_dropped() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "first");
        let (old_id, _) = link.last_sent().unwrap();
        type_query(&mut app, "second");
        link.push(ClientEvent::Reply {
            id: old_id,
            result: Ok(Reply::Search(wire_search("old", 9))),
        });
        app.poll_hosts();
        assert!(app.search.groups.iter().all(|g| g.session_id != "old"));
        assert!(link.cancelled.lock().unwrap().contains(&old_id));
    }

    #[test]
    fn remote_reply_when_input_cleared_then_dropped() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        let (id, _) = link.last_sent().unwrap();
        app.clear_input();
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(wire_search("late", 9))),
        });
        app.poll_hosts();
        assert!(app.in_recent_sessions_mode());
        assert!(link.cancelled.lock().unwrap().contains(&id));
    }

    #[test]
    fn remote_reply_when_suspended_for_resume_then_dropped() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        local_result(
            &mut app,
            vec![crate::tui::state::test_group("l1", "/l/l1.jsonl", 12)],
        );
        let (id, _) = link.last_sent().unwrap();
        app.suspend_for_resume_for_test();
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(wire_search("late", 9))),
        });
        app.poll_hosts();
        assert_eq!(app.search.groups.len(), 1);
    }

    #[test]
    fn host_ready_when_query_pending_then_search_sent_for_current_seq() {
        let link = FakeLink::new("Z", HostState::Connecting);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        assert!(link.last_sent().is_none());
        link.push(ClientEvent::State(HostState::Ready));
        app.poll_hosts();
        match link.last_sent() {
            Some((_, Request::Search(p))) => assert_eq!(p.query, "needle"),
            other => panic!("expected a search, got {other:?}"),
        }
        reply_search(&link, &mut app, "r1", 11);
        assert!(app.search.groups.iter().any(|g| g.session_id == "r1"));
    }

    #[test]
    fn remote_group_when_project_filter_on_then_kept_without_disk_read() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        app.project_filter = true;
        app.current_cwd = Some("/Users/me/projects/x".into());
        type_query(&mut app, "needle");
        reply_search(&link, &mut app, "r1", 11);
        assert!(app.search.groups.iter().any(|g| g.session_id == "r1"));
    }

    #[test]
    fn remote_group_when_message_counts_run_then_count_from_server_kept() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        reply_search(&link, &mut app, "r1", 11);
        local_result(
            &mut app,
            vec![crate::tui::state::test_group("l1", "/l/l1.jsonl", 12)],
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        app.tick();
        let remote = app
            .search
            .groups
            .iter()
            .find(|g| g.session_id == "r1")
            .unwrap();
        assert_eq!(remote.message_count, Some(7));
    }

    fn wire_recent(session_id: &str, hour: u32) -> crate::remote::wire::WireRecent {
        use chrono::TimeZone;
        crate::remote::wire::WireRecent {
            session_id: session_id.into(),
            file_path: format!("/home/m/.claude/projects/-p/{session_id}.jsonl"),
            project: "remote-proj".into(),
            timestamp: chrono::Utc
                .with_ymd_and_hms(2026, 10, 22, hour, 0, 0)
                .unwrap(),
            summary: "remote summary".into(),
            automation: None,
            branch: None,
            message_count: Some(2),
            preview_role: "user".into(),
            cwd: None,
        }
    }

    fn reply_recent(link: &FakeLink, app: &mut App, rows: Vec<crate::remote::wire::WireRecent>) {
        let (id, req) = link.last_sent().expect("a recent request was sent");
        assert!(matches!(req, Request::Recent(_)), "{req:?}");
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Recent(rows)),
        });
        app.poll_hosts();
    }

    fn connected(link: &FakeLink, app: &mut App) {
        link.push(ClientEvent::State(HostState::Ready));
        app.poll_hosts();
    }

    #[test]
    fn recent_when_host_ready_then_remote_rows_listed() {
        let link = FakeLink::new("Z", HostState::Connecting);
        let mut app = app_with(&link);
        connected(&link, &mut app);
        reply_recent(&link, &mut app, vec![wire_recent("r1", 11)]);
        assert!(app
            .recent
            .filtered
            .iter()
            .any(|s| s.host == Host::Remote("Z".into()) && s.session_id == "r1"));
    }

    #[test]
    fn recent_when_remote_buffer_and_project_filter_then_remote_rows_kept() {
        let link = FakeLink::new("Z", HostState::Connecting);
        let mut app = app_with(&link);
        app.project_filter = true;
        app.current_cwd = Some("/Users/me/projects/x".into());
        connected(&link, &mut app);
        reply_recent(&link, &mut app, vec![wire_recent("r1", 11)]);
        assert!(app.recent.filtered.iter().any(|s| s.session_id == "r1"));
    }

    #[test]
    fn recent_total_count_when_remote_rows_then_included() {
        let link = FakeLink::new("Z", HostState::Connecting);
        let mut app = app_with(&link);
        connected(&link, &mut app);
        reply_recent(
            &link,
            &mut app,
            vec![wire_recent("r1", 11), wire_recent("r2", 10)],
        );
        assert_eq!(app.recent.total_count(false), app.recent.all.len() + 2);
    }

    #[test]
    fn recent_when_same_session_id_on_two_hosts_then_two_rows() {
        let link = FakeLink::new("Z", HostState::Connecting);
        let mut app = app_with(&link);
        let mut local = crate::tui::state::test_recent("s1", 9);
        local.host = Host::Local;
        app.recent.all = vec![local];
        connected(&link, &mut app);
        reply_recent(&link, &mut app, vec![wire_recent("s1", 11)]);
        let rows: Vec<_> = app
            .recent
            .filtered
            .iter()
            .filter(|s| s.session_id == "s1")
            .collect();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn project_toggle_when_hosts_ready_then_recent_resent_with_home_relative_project() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        app.current_project_paths = vec!["/claude/projects/-x".into()];
        app.current_cwd = Some(format!("{home}/projects/x"));
        app.toggle_project_filter();
        match link.last_sent() {
            Some((_, Request::Recent(p))) => assert_eq!(p.project.as_deref(), Some("projects/x")),
            other => panic!("expected recent, got {other:?}"),
        }
    }

    #[test]
    fn recent_reply_when_from_superseded_ctrl_a_scope_then_dropped() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        app.current_project_paths = vec!["/claude/projects/-x".into()];
        app.current_cwd = Some(format!("{home}/projects/x"));
        app.toggle_project_filter();
        let (first, _) = link.last_sent().unwrap();
        app.toggle_project_filter();
        let (second, _) = link.last_sent().unwrap();
        assert_ne!(first, second);
        link.push(ClientEvent::Reply {
            id: second,
            result: Ok(Reply::Recent(vec![wire_recent("current", 11)])),
        });
        link.push(ClientEvent::Reply {
            id: first,
            result: Ok(Reply::Recent(vec![wire_recent("stale", 12)])),
        });
        app.poll_hosts();
        let remote: Vec<_> = app.recent.remote["Z"]
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(remote, ["current"]);
        assert!(link.cancelled.lock().unwrap().contains(&first));
    }

    #[test]
    fn recent_pick_when_remote_row_then_host_and_server_project() {
        let link = FakeLink::new("Z", HostState::Connecting);
        let mut app = app_with(&link);
        app.picker_mode = true;
        connected(&link, &mut app);
        reply_recent(&link, &mut app, vec![wire_recent("r1", 23)]);
        app.recent.cursor = app
            .recent
            .filtered
            .iter()
            .position(|s| s.session_id == "r1")
            .unwrap();
        app.on_enter();
        match app.take_outcome() {
            crate::tui::TuiOutcome::Pick(p) => {
                assert_eq!(p.host, Host::Remote("Z".into()));
                assert_eq!(p.project, "remote-proj");
            }
            other => panic!("expected pick, got {other:?}"),
        }
    }

    #[test]
    fn status_texts_when_states_vary_then_words_match_spec() {
        let mut hosts = HostsState::new(vec![Box::new(FakeLink::new("Z", HostState::Ready))]);
        let status = hosts.status.get_mut("Z").unwrap();
        status.state = HostState::Ready;
        status.results = Some(37);
        assert_eq!(hosts.status_texts(), vec!["Z: 37".to_string()]);
        hosts.status.get_mut("Z").unwrap().state = HostState::Down {
            reason: "no route".into(),
        };
        assert_eq!(hosts.status_texts(), vec!["Z: недоступен".to_string()]);
        assert_eq!(hosts.down_reasons(), vec!["Z: no route".to_string()]);
        hosts.status.get_mut("Z").unwrap().state = HostState::Incompatible { version: None };
        assert_eq!(hosts.status_texts(), vec!["Z: обнови ccs".to_string()]);
        let status = hosts.status.get_mut("Z").unwrap();
        status.state = HostState::Ready;
        status.last_error = Some("timeout".into());
        assert_eq!(
            hosts.status_texts(),
            vec!["Z: ошибка (timeout)".to_string()]
        );
    }
}
