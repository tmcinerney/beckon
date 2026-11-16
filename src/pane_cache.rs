//! A live, read-only view of one Herdr session's panes.
//!
//! The cache deliberately has no binding policy. It provides a single current
//! pane snapshot to the daemon while [`crate::core::BindingService`] remains
//! the single writer for the durable Beckon binding ledger. One cache exists
//! per live session; it stamps that session onto every pane it stores because
//! Herdr's wire payloads carry only the session-local pane ID.

use std::{
    sync::{Arc, RwLock},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{core::Agent, herdr::HerdrSocket};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct PaneCache {
    session: String,
    panes: Arc<RwLock<Vec<Agent>>>,
    observation: Arc<RwLock<crate::source_observation::SourceObservation>>,
}

impl PaneCache {
    pub fn new(session: impl Into<String>, initial: Vec<Agent>) -> Self {
        let session = session.into();
        let panes: Vec<_> = initial
            .into_iter()
            .map(|pane| stamp_session(pane, &session))
            .collect();
        let cache = Self {
            session,
            panes: Arc::new(RwLock::new(panes)),
            observation: Arc::new(RwLock::new(
                crate::source_observation::SourceObservation::unknown("herdr"),
            )),
        };
        let rows = cache
            .panes()
            .into_iter()
            .map(|agent| (agent, None))
            .collect();
        cache.replace_labelled(rows);
        cache
    }

    /// Seed from a complete `pane.list` snapshot, then keep it current from
    /// Herdr's global pane events. A read-only periodic reconciliation is the
    /// correctness backstop for a lost or out-of-order event.
    pub fn start(socket: HerdrSocket, session: &str) -> Result<Self> {
        let rows = socket.labelled_panes()?;
        let cache = Self::new(session, Vec::new());
        cache.replace_labelled(rows);
        let monitor = cache.clone();
        let reconcile = cache.clone();
        let reconcile_socket = socket.clone();
        thread::Builder::new()
            .name("beckon-herdr-events".into())
            .spawn(move || {
                loop {
                    if let Err(error) = socket.monitor(|event| monitor.apply(event)) {
                        monitor.mark_unavailable();
                        eprintln!("Herdr event stream disconnected: {error:#}");
                    }
                    thread::sleep(Duration::from_secs(1));
                    match socket.labelled_panes() {
                        Ok(panes) => monitor.replace_labelled(panes),
                        Err(error) => {
                            monitor.mark_unavailable();
                            eprintln!("refresh Herdr pane snapshot: {error:#}");
                        }
                    }
                }
            })?;
        thread::Builder::new()
            .name("beckon-herdr-reconcile".into())
            .spawn(move || {
                loop {
                    thread::sleep(RECONCILE_INTERVAL);
                    match reconcile_socket.labelled_panes() {
                        Ok(panes) => reconcile.replace_labelled(panes),
                        Err(error) => {
                            reconcile.mark_unavailable();
                            eprintln!("refresh Herdr pane snapshot: {error:#}");
                        }
                    }
                }
            })?;
        Ok(cache)
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn panes(&self) -> Vec<Agent> {
        self.panes.read().expect("pane cache lock poisoned").clone()
    }

    pub fn replace(&self, panes: Vec<Agent>) {
        self.replace_labelled(panes.into_iter().map(|pane| (pane, None)).collect());
    }

    pub fn observation(&self) -> crate::source_observation::SourceObservation {
        self.observation
            .read()
            .expect("observation lock poisoned")
            .clone()
    }

    pub fn mark_unavailable(&self) {
        let mut observation = self.observation.write().expect("observation lock poisoned");
        let now = Instant::now();
        observation.availability = crate::source_observation::SourceAvailability::Unavailable;
        observation.observed_at = Some(now);
        observation.observed_sessions.clear();
        observation.last_failure = Some(crate::display::TypedFailure {
            code: crate::display::DiagnosticCode::SourceUnavailable,
            at: now,
            active: true,
            recovery: crate::display::Recovery::CheckSource,
        });
        for agent in &mut observation.agents {
            agent.available = false;
        }
    }

    pub fn replace_labelled(&self, rows: Vec<(Agent, Option<String>)>) {
        use crate::source_observation::{AgentObservation, SourceAvailability, real_title};
        let now = Instant::now();
        let mut panes = self.panes.write().expect("pane cache lock poisoned");
        let mut observation = self.observation.write().expect("observation lock poisoned");
        let agents: Vec<_> = rows
            .into_iter()
            .map(|(agent, scope_title)| {
                let agent = stamp_session(agent, &self.session);
                let title = real_title(&agent);
                AgentObservation {
                    agent,
                    available: true,
                    observed_at: now,
                    title_at: title.as_ref().map(|_| now),
                    title,
                    scope_title_at: scope_title.as_ref().map(|_| now),
                    scope_title,
                }
            })
            .collect();
        *panes = agents.iter().map(|row| row.agent.clone()).collect();
        observation.id = "herdr".into();
        observation.availability = SourceAvailability::Available;
        observation.last_success = Some(now);
        observation.observed_at = Some(now);
        observation.observed_sessions = std::collections::BTreeSet::from([self.session.clone()]);
        if let Some(failure) = observation.last_failure.as_mut() {
            failure.active = false;
        }
        observation.agents = agents;
    }

    pub fn apply(&self, event: PaneEvent) {
        let mut panes = self.panes.write().expect("pane cache lock poisoned");
        match event {
            PaneEvent::Upsert(pane) => {
                let pane = stamp_session(*pane, &self.session);
                if let Some(existing) = panes.iter_mut().find(|item| item.pane_id == pane.pane_id) {
                    // Herdr's revision is monotonic per pane. Never let a
                    // delayed event roll a current cached status backward.
                    if pane.revision == 0 || pane.revision >= existing.revision {
                        *existing = pane;
                    } else {
                        return;
                    }
                } else {
                    panes.push(pane);
                }
            }
            PaneEvent::Remove(pane_id) => panes.retain(|pane| pane.pane_id != pane_id),
        }
        use crate::source_observation::{AgentObservation, SourceAvailability, real_title};
        let now = Instant::now();
        let mut observation = self.observation.write().expect("observation lock poisoned");
        let available = observation.availability == SourceAvailability::Available;
        let previous: std::collections::BTreeMap<_, _> = observation
            .agents
            .iter()
            .map(|row| (row.agent.reference(), row.clone()))
            .collect();
        observation.agents = panes
            .iter()
            .map(|agent| {
                let title = real_title(agent);
                let old = previous.get(&agent.reference());
                // Only a changed pane revision/title refreshes its evidence; an event
                // on another pane cannot freshen this pane or its workspace label.
                if let Some(old) = old
                    && old.agent == *agent
                {
                    return old.clone();
                }
                AgentObservation {
                    agent: agent.clone(),
                    available,
                    observed_at: now,
                    title_at: title.as_ref().map(|_| now),
                    title,
                    scope_title: old.and_then(|row| row.scope_title.clone()),
                    scope_title_at: old.and_then(|row| row.scope_title_at),
                }
            })
            .collect();
        observation.observed_at = Some(now);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    Upsert(Box<Agent>),
    Remove(String),
}

/// Herdr's wire payloads carry only the session-local pane ID. A pane stored in
/// this cache always belongs to the cache's session.
fn stamp_session(mut pane: Agent, session: &str) -> Agent {
    pane.session = session.to_string();
    pane
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn pane(id: &str, revision: u64, status: &str) -> Agent {
        Agent {
            source: crate::core::DEFAULT_SOURCE.into(),
            pane_id: id.into(),
            session: crate::core::DEFAULT_SESSION.into(),
            revision,
            agent_status: status.into(),
            agent: None,
            label: None,
            cwd: None,
            terminal_title: None,
            terminal_title_stripped: None,
            focused: false,
            tokens: BTreeMap::new(),
        }
    }

    #[test]
    fn applies_create_update_and_close_events() {
        let cache = PaneCache::new(crate::core::DEFAULT_SESSION, vec![pane("p1", 1, "idle")]);
        cache.apply(PaneEvent::Upsert(Box::new(pane("p2", 1, "working"))));
        cache.apply(PaneEvent::Upsert(Box::new(pane("p1", 2, "blocked"))));
        cache.apply(PaneEvent::Remove("p2".into()));
        assert_eq!(cache.panes(), vec![pane("p1", 2, "blocked")]);
    }

    #[test]
    fn ignores_out_of_order_pane_updates() {
        let cache = PaneCache::new(crate::core::DEFAULT_SESSION, vec![pane("p1", 3, "blocked")]);
        cache.apply(PaneEvent::Upsert(Box::new(pane("p1", 2, "working"))));
        assert_eq!(cache.panes(), vec![pane("p1", 3, "blocked")]);
    }

    #[test]
    fn stamps_every_stored_pane_with_the_cache_session() {
        let cache = PaneCache::new("agent-workspace", vec![pane("w1:p1", 1, "idle")]);
        cache.apply(PaneEvent::Upsert(Box::new(pane("w1:p2", 1, "working"))));
        cache.replace(vec![pane("w1:p3", 1, "idle")]);

        assert_eq!(cache.session(), "agent-workspace");
        assert_eq!(cache.panes()[0].session, "agent-workspace");
        assert_eq!(cache.panes()[0].pane_id, "w1:p3");
    }
}
