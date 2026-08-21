//! A live, read-only view of Herdr panes.
//!
//! The cache deliberately has no binding policy. It provides a single current
//! pane snapshot to the daemon while [`crate::core::BindingService`] remains
//! the single writer for the durable Beckon binding ledger.

use std::{
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

use anyhow::Result;

use crate::{core::Pane, herdr::HerdrSocket};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
pub struct PaneCache {
    panes: Arc<RwLock<Vec<Pane>>>,
}

impl PaneCache {
    pub fn new(initial: Vec<Pane>) -> Self {
        Self {
            panes: Arc::new(RwLock::new(initial)),
        }
    }

    /// Seed from a complete `pane.list` snapshot, then keep it current from
    /// Herdr's global pane events. A read-only periodic reconciliation is the
    /// correctness backstop for a lost or out-of-order event.
    pub fn start(socket: HerdrSocket) -> Result<Self> {
        let cache = Self::new(socket.panes()?);
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

    pub fn panes(&self) -> Vec<Pane> {
        self.panes.read().expect("pane cache lock poisoned").clone()
    }

    pub fn replace(&self, panes: Vec<Pane>) {
        *self.panes.write().expect("pane cache lock poisoned") = panes;
    }

    pub fn apply(&self, event: PaneEvent) {
        let mut panes = self.panes.write().expect("pane cache lock poisoned");
        match event {
            PaneEvent::Upsert(pane) => {
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
    Upsert(Pane),
    Remove(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn pane(id: &str, revision: u64, status: &str) -> Pane {
        Pane {
            pane_id: id.into(),
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
        let cache = PaneCache::new(vec![pane("p1", 1, "idle")]);
        cache.apply(PaneEvent::Upsert(pane("p2", 1, "working")));
        cache.apply(PaneEvent::Upsert(pane("p1", 2, "blocked")));
        cache.apply(PaneEvent::Remove("p2".into()));
        assert_eq!(cache.panes(), vec![pane("p1", 2, "blocked")]);
    }

    #[test]
    fn ignores_out_of_order_pane_updates() {
        let cache = PaneCache::new(vec![pane("p1", 3, "blocked")]);
        cache.apply(PaneEvent::Upsert(pane("p1", 2, "working")));
        assert_eq!(cache.panes(), vec![pane("p1", 3, "blocked")]);
    }
}
