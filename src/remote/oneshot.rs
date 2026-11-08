//! One request to several hosts at once, for CLI commands: connect, ask,
//! wait up to a deadline, report what each host answered.

use super::client::{ClientEvent, ClientTimeouts, HostClient, HostLink, HostState, Reply, Request};
use super::config::HostConfig;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum HostOutcome {
    Reply(Reply),
    Down(String),
    Incompatible(Option<String>),
    Timeout,
    Failed(String),
}

impl HostOutcome {
    /// `status` and `error` as the CLI summary prints them.
    pub fn status(&self) -> (&'static str, Option<String>) {
        match self {
            HostOutcome::Reply(_) => ("ok", None),
            HostOutcome::Down(reason) => ("down", Some(reason.clone())),
            HostOutcome::Incompatible(version) => (
                "incompatible",
                Some(match version {
                    Some(v) => format!("обнови ccs ({v})"),
                    None => "обнови ccs".to_string(),
                }),
            ),
            HostOutcome::Timeout => ("timeout", None),
            HostOutcome::Failed(e) => ("error", Some(e.clone())),
        }
    }
}

struct Query {
    client: HostClient,
    id: Option<u64>,
    outcome: Option<HostOutcome>,
}

/// Ask every host `request` in parallel; give up on a host after `wait`.
pub fn query_hosts(
    hosts: &[HostConfig],
    request: &Request,
    wait: Duration,
) -> Vec<(String, HostOutcome)> {
    let timeouts = ClientTimeouts {
        request: wait,
        retry: Duration::MAX,
        ..ClientTimeouts::default()
    };
    let mut queries: Vec<Query> = hosts
        .iter()
        .map(|h| Query {
            client: HostClient::spawn(h.clone(), timeouts),
            id: None,
            outcome: None,
        })
        .collect();
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline && queries.iter().any(|q| q.outcome.is_none()) {
        for query in queries.iter_mut().filter(|q| q.outcome.is_none()) {
            step(query, request);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    queries
        .into_iter()
        .map(|q| {
            let name = q.client.name().to_string();
            (name, q.outcome.unwrap_or(HostOutcome::Timeout))
        })
        .collect()
}

fn step(query: &mut Query, request: &Request) {
    for event in query.client.poll(Instant::now()) {
        if let Some(outcome) = event_outcome(event, query.id) {
            query.outcome = Some(outcome);
        }
    }
    if query.outcome.is_none() && query.id.is_none() && query.client.state() == &HostState::Ready {
        query.id = query.client.send(request.clone());
    }
}

/// The outcome `event` settles for a query whose request went out as `sent`:
/// the reply to that request, or the host going down or turning out
/// incompatible. Any other event leaves the query waiting.
fn event_outcome(event: ClientEvent, sent: Option<u64>) -> Option<HostOutcome> {
    match event {
        ClientEvent::Reply { id, result } if Some(id) == sent => Some(reply_outcome(result)),
        ClientEvent::State(HostState::Down { reason }) => Some(HostOutcome::Down(reason)),
        ClientEvent::State(HostState::Incompatible { version }) => {
            Some(HostOutcome::Incompatible(version))
        }
        _ => None,
    }
}

/// A reply result as the CLI reports it; the client's own request timeout
/// counts as `Timeout`, not as a host error.
fn reply_outcome(result: Result<Reply, String>) -> HostOutcome {
    match result {
        Ok(reply) => HostOutcome::Reply(reply),
        Err(e) if e == "timeout" => HostOutcome::Timeout,
        Err(e) => HostOutcome::Failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_outcomes_when_summarized_then_status_and_error_match() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            HostOutcome::Down("no route".into()).status(),
            ("down", some("no route"))
        );
        assert_eq!(
            HostOutcome::Incompatible(some("0.16.0")).status(),
            ("incompatible", some("обнови ccs (0.16.0)"))
        );
        assert_eq!(
            HostOutcome::Incompatible(None).status(),
            ("incompatible", some("обнови ccs"))
        );
        assert_eq!(HostOutcome::Timeout.status(), ("timeout", None));
        assert_eq!(
            HostOutcome::Failed("boom".into()).status(),
            ("error", some("boom"))
        );
    }

    fn reply_event(id: u64, result: Result<Reply, String>) -> ClientEvent {
        ClientEvent::Reply { id, result }
    }

    #[test]
    fn event_outcome_when_reply_to_sent_request_then_reply_timeout_or_failed() {
        assert_eq!(
            event_outcome(reply_event(7, Ok(Reply::Recent(Vec::new()))), Some(7)),
            Some(HostOutcome::Reply(Reply::Recent(Vec::new())))
        );
        assert_eq!(
            event_outcome(reply_event(7, Err("timeout".into())), Some(7)),
            Some(HostOutcome::Timeout)
        );
        assert_eq!(
            event_outcome(reply_event(7, Err("boom".into())), Some(7)),
            Some(HostOutcome::Failed("boom".into()))
        );
    }

    #[test]
    fn event_outcome_when_reply_to_other_request_then_none() {
        assert_eq!(
            event_outcome(reply_event(3, Err("boom".into())), Some(7)),
            None
        );
        assert_eq!(
            event_outcome(reply_event(3, Err("boom".into())), None),
            None
        );
    }

    #[test]
    fn event_outcome_when_host_down_or_incompatible_then_settles() {
        assert_eq!(
            event_outcome(
                ClientEvent::State(HostState::Down {
                    reason: "no route".into()
                }),
                None
            ),
            Some(HostOutcome::Down("no route".into()))
        );
        assert_eq!(
            event_outcome(
                ClientEvent::State(HostState::Incompatible {
                    version: Some("0.16.0".into())
                }),
                Some(1)
            ),
            Some(HostOutcome::Incompatible(Some("0.16.0".into())))
        );
    }

    #[test]
    fn event_outcome_when_ready_or_hello_then_none() {
        assert_eq!(
            event_outcome(ClientEvent::State(HostState::Ready), None),
            None
        );
        assert_eq!(
            event_outcome(
                ClientEvent::Hello {
                    ccs_version: "0.18.1".into()
                },
                None
            ),
            None
        );
    }
}
