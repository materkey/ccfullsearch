//! Remote hosts as the picker sees them: one link per host, the requests in
//! flight, the latest search results per host and what the status bar says.

use crate::remote::client::{ClientEvent, HostLink, HostState, Request};
use crate::remote::{Host, SessionRef};
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

    /// `All → Local → Remote`, then each host by name when there are
    /// several, then back to `All`.
    pub fn next(&self, hosts: &[String]) -> HostFilter {
        let mut cycle = vec![HostFilter::All, HostFilter::Local, HostFilter::Remote];
        if hosts.len() > 1 {
            cycle.extend(hosts.iter().cloned().map(HostFilter::Only));
        }
        let at = cycle.iter().position(|f| f == self).unwrap_or(0);
        cycle[(at + 1) % cycle.len()].clone()
    }

    /// Title tag for the search box; nothing for `All`.
    pub fn label(&self) -> Option<String> {
        match self {
            HostFilter::All => None,
            HostFilter::Local => Some("Local".to_string()),
            HostFilter::Remote => Some("Remote".to_string()),
            HostFilter::Only(name) => Some(name.clone()),
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
    Search {
        seq: u64,
    },
    Recent,
    /// Full text for a preview; `tree` says where it goes.
    Show {
        target: ShowTarget,
        tree: bool,
    },
    Tree(SessionRef),
}

/// Which message's full text: by uuid when the message has one, else by
/// JSONL line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ShowKey {
    Line(usize),
    Uuid(String),
}

pub(crate) type ShowTarget = (SessionRef, ShowKey);

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

    /// Whether this message's text was already asked for.
    pub(crate) fn show_pending(&self, target: &ShowTarget) -> bool {
        self.pending
            .values()
            .any(|k| matches!(k, PendingKind::Show { target: t, .. } if t == target))
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

    /// Cancel every tree request in flight (the user left or opened another tree).
    pub(crate) fn cancel_trees(&mut self) {
        self.cancel_where(|kind| matches!(kind, PendingKind::Tree(_)));
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

    /// Reconnect every host that is down or incompatible, now.
    pub(crate) fn reconnect_down(&mut self) {
        for link in &mut self.links {
            link.reconnect_now();
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

    fn remote_group_selected(link: &FakeLink, app: &mut App, truncated: bool) {
        type_query(app, "needle");
        let (id, _) = link.last_sent().unwrap();
        let mut found = wire_search("r1", 11);
        found.groups[0].matches[0].content_truncated = truncated;
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(found)),
        });
        app.poll_hosts();
        app.search.group_cursor = 0;
    }

    fn sent_count(link: &FakeLink, pred: impl Fn(&Request) -> bool) -> usize {
        link.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| pred(r))
            .count()
    }

    #[test]
    fn remote_tree_when_reply_arrives_then_tree_installed_with_branch_count() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, false);
        app.enter_tree_mode();
        assert!(app.tree_mode && app.tree.tree_loading);
        let (id, req) = link.last_sent().unwrap();
        assert!(matches!(req, Request::Tree(_)), "{req:?}");
        let tree: crate::remote::wire::WireTree = serde_json::from_value(serde_json::json!({
            "session_id": "r1", "file_path": "/home/m/.claude/projects/-p/r1.jsonl",
            "tip_uuid": "u1", "branch_count": 2,
            "rows": [{"uuid": "u1", "role": "user", "timestamp": "2026-10-22T11:00:00Z",
                      "content_preview": "hi", "graph_symbols": "* ", "is_on_latest_chain": true,
                      "is_branch_point": false, "is_compaction": false}]
        }))
        .unwrap();
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Tree(tree)),
        });
        app.poll_hosts();
        let installed = app.tree.session_tree.as_ref().expect("tree installed");
        assert!(!app.tree.tree_loading);
        assert_eq!(installed.branch_count(), 2);
        assert_eq!(installed.host, Host::Remote("Z".into()));
    }

    fn wire_tree(session_id: &str) -> crate::remote::wire::WireTree {
        serde_json::from_value(serde_json::json!({
            "session_id": session_id,
            "file_path": format!("/home/m/.claude/projects/-p/{session_id}.jsonl"),
            "tip_uuid": "u1", "branch_count": 0,
            "rows": [{"uuid": "u1", "role": "user", "timestamp": "2026-10-22T11:00:00Z",
                      "content_preview": session_id, "graph_symbols": "* ", "is_on_latest_chain": true,
                      "is_branch_point": false, "is_compaction": false}]
        }))
        .unwrap()
    }

    #[test]
    fn tree_reply_when_for_a_tree_left_earlier_then_ignored() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        let (id, _) = link.last_sent().unwrap();
        let mut found = wire_search("a", 11);
        found.groups.extend(wire_search("b", 10).groups);
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(found)),
        });
        app.poll_hosts();
        app.search.group_cursor = 0;
        app.enter_tree_mode();
        let (tree_a, _) = link.last_sent().unwrap();
        app.exit_tree_mode();
        app.search.group_cursor = 1;
        app.enter_tree_mode();
        let (tree_b, _) = link.last_sent().unwrap();
        link.push(ClientEvent::Reply {
            id: tree_a,
            result: Ok(Reply::Tree(wire_tree("a"))),
        });
        app.poll_hosts();
        assert!(
            app.tree.tree_loading,
            "A's late reply must not end B's load"
        );
        assert!(app.tree.session_tree.is_none());
        link.push(ClientEvent::Reply {
            id: tree_b,
            result: Ok(Reply::Tree(wire_tree("b"))),
        });
        app.poll_hosts();
        assert_eq!(app.tree.session_tree.as_ref().unwrap().session_id, "b");
    }

    #[test]
    fn preview_when_cursor_moves_twice_on_same_match_then_one_show_sent() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, true);
        app.preview_mode = true;
        app.request_preview_if_needed();
        app.request_preview_if_needed();
        assert_eq!(sent_count(&link, |r| matches!(r, Request::Show(_))), 1);
    }

    fn opencode_two_matches(link: &FakeLink, app: &mut App) {
        type_query(app, "needle");
        let (id, _) = link.last_sent().unwrap();
        let mut found = wire_search("ses_1", 11);
        let group = &mut found.groups[0];
        group.file_path = "/home/m/.local/share/opencode/opencode.db#ses_1".into();
        let mut second = group.matches[0].clone();
        let first = &mut group.matches[0];
        first.uuid = Some("m1".into());
        first.file_path = Some(group.file_path.clone());
        first.content_truncated = true;
        second.uuid = Some("m2".into());
        second.file_path = Some(group.file_path.clone());
        second.content_truncated = true;
        group.matches.push(second);
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Search(found)),
        });
        app.poll_hosts();
        app.search.group_cursor = 0;
        app.search.expanded = true;
        app.preview_mode = true;
    }

    #[test]
    fn preview_when_two_opencode_matches_share_line_1_then_each_gets_its_own_text() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        opencode_two_matches(&link, &mut app);
        app.search.sub_cursor = 0;
        app.request_preview_if_needed();
        let (id, _) = link.last_sent().unwrap();
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Show(crate::remote::wire::WireShow {
                content: "first message".into(),
                text_content: "first message".into(),
            })),
        });
        app.poll_hosts();
        app.search.sub_cursor = 1;
        app.request_preview_if_needed();
        assert_eq!(sent_count(&link, |r| matches!(r, Request::Show(_))), 2);
        let group = app.search.groups[0].clone();
        let msg = app.preview_message(&group, &group.matches[1]).unwrap();
        assert_ne!(msg.content, "first message");
    }

    #[test]
    fn preview_when_show_fails_then_not_resent_on_every_tick() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, true);
        app.preview_mode = true;
        app.request_preview_if_needed();
        let (id, _) = link.last_sent().unwrap();
        link.push(ClientEvent::Reply {
            id,
            result: Err("no message with uuid".into()),
        });
        app.poll_hosts();
        app.request_preview_if_needed();
        app.request_preview_if_needed();
        assert_eq!(sent_count(&link, |r| matches!(r, Request::Show(_))), 1);
    }

    fn fail_last_show(link: &FakeLink, app: &mut App) {
        let (id, req) = link.last_sent().unwrap();
        assert!(matches!(req, Request::Show(_)), "{req:?}");
        link.push(ClientEvent::Reply {
            id,
            result: Err("timeout".into()),
        });
        app.poll_hosts();
    }

    fn shows(link: &FakeLink) -> usize {
        sent_count(link, |r| matches!(r, Request::Show(_)))
    }

    #[test]
    fn failed_show_when_new_search_then_asked_again() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, true);
        app.preview_mode = true;
        app.request_preview_if_needed();
        fail_last_show(&link, &mut app);
        remote_group_selected(&link, &mut app, true);
        app.request_preview_if_needed();
        assert_eq!(shows(&link), 2);
    }

    #[test]
    fn failed_show_when_host_reconnects_then_asked_again() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, true);
        app.preview_mode = true;
        app.request_preview_if_needed();
        fail_last_show(&link, &mut app);
        link.push(ClientEvent::State(HostState::Down {
            reason: "blip".into(),
        }));
        link.push(ClientEvent::State(HostState::Ready));
        app.poll_hosts();
        app.request_preview_if_needed();
        assert_eq!(shows(&link), 2);
    }

    #[test]
    fn failed_tree_show_when_tree_reopened_then_asked_again() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, false);
        let open_tree = |app: &mut App| {
            app.enter_tree_mode();
            let (id, _) = link.last_sent().unwrap();
            link.push(ClientEvent::Reply {
                id,
                result: Ok(Reply::Tree(wire_tree("r1"))),
            });
            app.poll_hosts();
            app.preview_mode = true;
            app.request_preview_if_needed();
        };
        open_tree(&mut app);
        fail_last_show(&link, &mut app);
        app.exit_tree_mode();
        open_tree(&mut app);
        assert_eq!(shows(&link), 2);
    }

    #[test]
    fn preview_when_match_not_truncated_then_no_show_sent() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, false);
        app.preview_mode = true;
        app.request_preview_if_needed();
        assert_eq!(sent_count(&link, |r| matches!(r, Request::Show(_))), 0);
    }

    #[test]
    fn preview_when_show_reply_arrives_then_full_text_used() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, true);
        app.preview_mode = true;
        app.request_preview_if_needed();
        let (id, _) = link.last_sent().unwrap();
        link.push(ClientEvent::Reply {
            id,
            result: Ok(Reply::Show(crate::remote::wire::WireShow {
                content: "the whole message".into(),
                text_content: "the whole message".into(),
            })),
        });
        app.poll_hosts();
        let group = app.search.groups[0].clone();
        let msg = app.preview_message(&group, &group.matches[0]).unwrap();
        assert_eq!(msg.content, "the whole message");
    }

    fn reply_error(link: &FakeLink, app: &mut App, error: &str) {
        let (id, _) = link.last_sent().expect("a request was sent");
        link.push(ClientEvent::Reply {
            id,
            result: Err(error.into()),
        });
        app.poll_hosts();
    }

    #[test]
    fn search_reply_when_host_fails_then_status_shows_error() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        reply_error(&link, &mut app, "timeout");
        assert_eq!(
            app.hosts.status_texts(),
            vec!["Z: ошибка (timeout)".to_string()]
        );
    }

    #[test]
    fn recent_reply_when_host_fails_then_status_shows_error() {
        let link = FakeLink::new("Z", HostState::Connecting);
        let mut app = app_with(&link);
        connected(&link, &mut app);
        reply_error(&link, &mut app, "timeout");
        assert_eq!(
            app.hosts.status_texts(),
            vec!["Z: ошибка (timeout)".to_string()]
        );
    }

    #[test]
    fn tree_reply_when_host_fails_then_tree_mode_left_with_error() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        remote_group_selected(&link, &mut app, false);
        app.enter_tree_mode();
        reply_error(&link, &mut app, "no such file");
        assert!(!app.tree_mode);
        assert_eq!(
            app.search.error.as_deref(),
            Some("Tree load error: no such file")
        );
    }

    #[test]
    fn host_filter_next_when_one_host_then_all_local_remote_cycle() {
        let hosts = vec!["Z".to_string()];
        assert_eq!(HostFilter::All.next(&hosts), HostFilter::Local);
        assert_eq!(HostFilter::Local.next(&hosts), HostFilter::Remote);
        assert_eq!(HostFilter::Remote.next(&hosts), HostFilter::All);
    }

    #[test]
    fn host_filter_next_when_two_hosts_then_includes_each_host() {
        let hosts = vec!["A".to_string(), "B".to_string()];
        assert_eq!(
            HostFilter::Remote.next(&hosts),
            HostFilter::Only("A".into())
        );
        assert_eq!(
            HostFilter::Only("A".into()).next(&hosts),
            HostFilter::Only("B".into())
        );
        assert_eq!(HostFilter::Only("B".into()).next(&hosts), HostFilter::All);
    }

    #[test]
    fn host_filter_when_local_then_remote_rows_hidden_and_no_remote_search_sent() {
        let link = FakeLink::new("Z", HostState::Ready);
        let mut app = app_with(&link);
        type_query(&mut app, "needle");
        reply_search(&link, &mut app, "r1", 11);
        assert!(app.search.groups.iter().any(|g| g.session_id == "r1"));
        app.cycle_host_filter();
        assert_eq!(app.hosts.filter, HostFilter::Local);
        assert!(app.search.groups.iter().all(|g| g.host.is_local()));
        let before = link.sent.lock().unwrap().len();
        type_query(&mut app, "other");
        assert_eq!(link.sent.lock().unwrap().len(), before);
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
