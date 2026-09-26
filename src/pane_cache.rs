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
    time::Duration,
};

use anyhow::Result;

use crate::{core::Pane, herdr::HerdrSocket};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct PaneCache {
    session: String,
    panes: Arc<RwLock<Vec<Pane>>>,
}

impl PaneCache {
    pub fn new(session: impl Into<String>, initial: Vec<Pane>) -> Self {
        let session = session.into();
        let panes = initial
            .into_iter()
            .map(|pane| stamp_session(pane, &session))
            .collect();
        Self {
            session,
            panes: Arc::new(RwLock::new(panes)),
        }
    }

    /// Seed from a complete `pane.list` snapshot, then keep it current from
    /// Herdr's global pane events. A read-only periodic reconciliation is the
    /// correctness backstop for a lost or out-of-order event.
    pub fn start(socket: HerdrSocket, session: &str) -> Result<Self> {
        let cache = Self::new(session, socket.panes()?);
        let monitor = cache.clone();
        let reconcile = cache.clone();
        let reconcile_socket = socket.clone();
        thread::Builder::new()
            .name("beckon-herdr-events".into())
            .spawn(move || {
                loop {
                    if let Err(error) = socket.monitor(|event| monitor.apply(event)) {
                        eprintln!("Herdr event stream disconnected: {error:#}");
                    }
                    thread::sleep(Duration::from_secs(1));
                    match socket.panes() {
                        Ok(panes) => monitor.replace(panes),
                        Err(error) => eprintln!("refresh Herdr pane snapshot: {error:#}"),
                    }
                }
            })?;
        thread::Builder::new()
            .name("beckon-herdr-reconcile".into())
            .spawn(move || {
                loop {
                    thread::sleep(RECONCILE_INTERVAL);
                    match reconcile_socket.panes() {
                        Ok(panes) => reconcile.replace(panes),
                        Err(error) => eprintln!("refresh Herdr pane snapshot: {error:#}"),
                    }
                }
            })?;
        Ok(cache)
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn panes(&self) -> Vec<Pane> {
        self.panes.read().expect("pane cache lock poisoned").clone()
    }

    pub fn replace(&self, panes: Vec<Pane>) {
        let panes = panes
            .into_iter()
            .map(|pane| stamp_session(pane, &self.session))
            .collect();
        *self.panes.write().expect("pane cache lock poisoned") = panes;
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
                    }
                } else {
                    panes.push(pane);
                }
            }
            PaneEvent::Remove(pane_id) => panes.retain(|pane| pane.pane_id != pane_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    Upsert(Box<Pane>),
    Remove(String),
}

/// Herdr's wire payloads carry only the session-local pane ID. A pane stored in
/// this cache always belongs to the cache's session.
fn stamp_session(mut pane: Pane, session: &str) -> Pane {
    pane.session = session.to_string();
    pane
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn pane(id: &str, revision: u64, status: &str) -> Pane {
        Pane {
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
