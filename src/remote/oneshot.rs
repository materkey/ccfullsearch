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
        match event {
            ClientEvent::Reply { id, result } if Some(id) == query.id => {
                query.outcome = Some(match result {
                    Ok(reply) => HostOutcome::Reply(reply),
                    Err(e) if e == "timeout" => HostOutcome::Timeout,
                    Err(e) => HostOutcome::Failed(e),
                });
            }
            ClientEvent::State(HostState::Down { reason }) => {
                query.outcome = Some(HostOutcome::Down(reason));
            }
            ClientEvent::State(HostState::Incompatible { version }) => {
                query.outcome = Some(HostOutcome::Incompatible(version));
            }
            _ => {}
        }
    }
    if query.outcome.is_none() && query.id.is_none() && query.client.state() == &HostState::Ready {
        query.id = query.client.send(request.clone());
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
}
