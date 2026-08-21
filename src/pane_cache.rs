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
    /// Herdr's global pane events. The monitor reconnects after a Herdr restart.
    pub fn start(socket: HerdrSocket) -> Result<Self> {
        let cache = Self::new(socket.panes()?);
        let monitor = cache.clone();
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
                    *existing = pane;
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

    fn pane(id: &str, status: &str) -> Pane {
        Pane {
            pane_id: id.into(),
            agent_status: status.into(),
            agent: None,
            label: None,
            cwd: None,
            tokens: BTreeMap::new(),
        }
    }

    #[test]
    fn applies_create_update_and_close_events() {
        let cache = PaneCache::new(vec![pane("p1", "idle")]);
        cache.apply(PaneEvent::Upsert(pane("p2", "working")));
        cache.apply(PaneEvent::Upsert(pane("p1", "blocked")));
        cache.apply(PaneEvent::Remove("p2".into()));
        assert_eq!(cache.panes(), vec![pane("p1", "blocked")]);
    }
}
