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

use crate::{core::Pane, herdr::HerdrSocket};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct PaneCache {
    session: String,
    inner: Arc<RwLock<CacheState>>,
}

struct CacheState {
    panes: Vec<Pane>,
    generation: u64,
    request: u64,
    refresh: u64,
    connected: bool,
    pilot: Option<crate::pilot::Snapshot>,
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
            inner: Arc::new(RwLock::new(CacheState {
                panes,
                generation: 1,
                request: 0,
                refresh: 0,
                connected: false,
                pilot: None,
            })),
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
                    let generation = monitor.generation();
                    if let Err(error) = socket.monitor_ready(
                        || monitor.connected(generation),
                        |event| monitor.apply_fenced(generation, event),
                    ) {
                        monitor.failed(generation);
                        eprintln!("Herdr event stream disconnected: {error:#}");
                    }
                    thread::sleep(Duration::from_secs(1));
                    // Keep the existing reconnect backstop independent of pilot identity.
                    let ticket = monitor.begin_refresh();
                    match socket.panes() {
                        Ok(panes) => monitor.finish_batch(
                            ticket,
                            crate::herdr::PaneRefresh {
                                panes,
                                pilot: Err(anyhow::anyhow!(
                                    "reconnect snapshot has no native pair"
                                )),
                            },
                            socket.path_string(),
                            Instant::now(),
                        ),
                        Err(error) => {
                            monitor.refresh_failed(ticket);
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
                    let ticket = reconcile.begin_refresh();
                    let at = Instant::now();
                    match reconcile_socket.refresh() {
                        Ok(batch) => reconcile.finish_batch(
                            ticket,
                            batch,
                            reconcile_socket.path_string(),
                            at,
                        ),
                        Err(error) => {
                            reconcile.refresh_failed(ticket);
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

    pub fn panes(&self) -> Vec<Pane> {
        self.inner
            .read()
            .expect("pane cache lock poisoned")
            .panes
            .clone()
    }

    pub fn replace(&self, panes: Vec<Pane>) {
        let panes = panes
            .into_iter()
            .map(|pane| stamp_session(pane, &self.session))
            .collect();
        let mut inner = self.inner.write().expect("pane cache lock poisoned");
        inner.panes = panes;
        inner.request += 1;
        inner.pilot = None;
    }

    pub fn generation(&self) -> u64 {
        self.inner.read().unwrap().generation
    }
    fn connected(&self, generation: u64) {
        let mut s = self.inner.write().unwrap();
        if s.generation == generation {
            s.connected = true;
        }
    }
    fn failed(&self, generation: u64) {
        let mut s = self.inner.write().unwrap();
        if s.generation == generation {
            s.generation += 1;
            s.connected = false;
            s.pilot = None;
        }
    }
    fn apply_fenced(&self, generation: u64, event: PaneEvent) {
        // Both callbacks and refresh completions share the same lock/fence.
        let mut s = self.inner.write().unwrap();
        if s.generation != generation || !s.connected {
            return;
        }
        // Invalidate every in-flight snapshot, including one predating a create/remove.
        s.request += 1;
        s.pilot = None;
        match event {
            PaneEvent::Upsert(pane) => {
                let pane = stamp_session(*pane, &self.session);
                if let Some(old) = s.panes.iter_mut().find(|p| p.pane_id == pane.pane_id) {
                    if pane.revision == 0 || pane.revision >= old.revision {
                        *old = pane;
                    }
                } else {
                    s.panes.push(pane);
                }
            }
            PaneEvent::Remove(id) => s.panes.retain(|p| p.pane_id != id),
        }
    }
    pub fn begin_refresh(&self) -> (u64, u64) {
        let mut s = self.inner.write().unwrap();
        s.request += 1;
        (s.generation, s.request)
    }
    pub fn refresh_failed(&self, ticket: (u64, u64)) {
        let mut s = self.inner.write().unwrap();
        if ticket == (s.generation, s.request) {
            s.pilot = None;
        }
    }
    fn finish_batch(
        &self,
        ticket: (u64, u64),
        batch: crate::herdr::PaneRefresh,
        socket: String,
        at: Instant,
    ) {
        let mut s = self.inner.write().unwrap();
        if ticket != (s.generation, s.request) {
            return;
        }
        if batch.panes.iter().any(|new| {
            s.panes
                .iter()
                .any(|old| old.pane_id == new.pane_id && old.revision > new.revision)
        }) {
            s.pilot = None;
            return;
        }
        s.panes = batch
            .panes
            .into_iter()
            .map(|p| stamp_session(p, &self.session))
            .collect();
        s.refresh += 1;
        s.pilot = match batch.pilot {
            Ok(mut observations) if s.connected => {
                for o in &mut observations {
                    o.pane.session = self.session.clone();
                }
                Some(crate::pilot::Snapshot {
                    socket,
                    generation: s.generation,
                    refresh: s.refresh,
                    at,
                    observations,
                })
            }
            _ => None,
        };
    }
    #[cfg(test)]
    fn finish_refresh(
        &self,
        ticket: (u64, u64),
        observations: Vec<crate::pilot::Observation>,
        socket: String,
        at: Instant,
    ) {
        let panes = observations.iter().map(|o| o.pane.clone()).collect();
        self.finish_batch(
            ticket,
            crate::herdr::PaneRefresh {
                panes,
                pilot: Ok(observations),
            },
            socket,
            at,
        );
    }
    pub fn pilot_snapshot(&self) -> Option<crate::pilot::Snapshot> {
        self.inner.read().unwrap().pilot.clone()
    }

    pub fn apply(&self, event: PaneEvent) {
        let mut inner = self.inner.write().expect("pane cache lock poisoned");
        // Native event payloads do not prove terminal/session identity. Wait for a full pair.
        inner.request += 1;
        inner.pilot = None;
        let panes = &mut inner.panes;
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
    #[test]
    fn pilot_fences_disconnect_late_refresh_and_event_revision() {
        let cache = PaneCache::new("test", vec![pane("p1", 3, "working")]);
        cache.connected(1);
        let first = cache.begin_refresh();
        let second = cache.begin_refresh();
        let observed = || {
            vec![crate::pilot::Observation {
                pane: pane("p1", 3, "unknown"),
                terminal_id: "t1".into(),
                agent_session: None,
            }]
        };
        cache.finish_refresh(first, observed(), "/s".into(), Instant::now());
        assert!(cache.pilot_snapshot().is_none());
        cache.finish_refresh(second, observed(), "/s".into(), Instant::now());
        assert!(cache.pilot_snapshot().is_some());
        cache.failed(1);
        cache.apply_fenced(1, PaneEvent::Remove("p1".into()));
        assert_eq!(cache.panes().len(), 1);
        cache.finish_refresh(second, observed(), "/s".into(), Instant::now());
        assert!(cache.pilot_snapshot().is_none());
        cache.connected(2);
        let ticket = cache.begin_refresh();
        cache.apply_fenced(2, PaneEvent::Upsert(Box::new(pane("p1", 5, "blocked"))));
        cache.finish_refresh(ticket, observed(), "/s".into(), Instant::now());
        assert_eq!(cache.panes()[0].revision, 5);
        assert!(cache.pilot_snapshot().is_none());
    }
    #[test]
    fn valid_panes_reconcile_despite_missing_or_inconsistent_pilot_identity() {
        for missing_agent in [true, false] {
            let cache = PaneCache::new("test", vec![pane("p1", 1, "idle")]);
            cache.connected(1);
            let mut newest = pane("p1", 2, "blocked");
            newest.agent = Some("codex".into());
            let mut raw = serde_json::to_value(&newest).unwrap();
            raw["terminal_id"] = serde_json::json!("t1");
            raw["agent_session"] =
                serde_json::json!({"agent":"codex","kind":"id","value":"native1"});
            let agent_data = if missing_agent {
                serde_json::json!([])
            } else {
                let mut raced = raw.clone();
                raced["agent_status"] = serde_json::json!("working");
                serde_json::json!([raced])
            };
            let pilot = crate::pilot::decode_pair(serde_json::json!([raw]), agent_data);
            assert!(pilot.is_err());
            cache.finish_batch(
                cache.begin_refresh(),
                crate::herdr::PaneRefresh {
                    panes: vec![newest.clone()],
                    pilot,
                },
                "/s".into(),
                Instant::now(),
            );
            newest.session = "test".into();
            assert_eq!(cache.panes(), vec![newest]);
            assert!(cache.pilot_snapshot().is_none());
        }
        let cache = PaneCache::new("test", vec![pane("p1", 1, "idle")]);
        cache.connected(1);
        cache.finish_batch(
            cache.begin_refresh(),
            crate::herdr::PaneRefresh {
                panes: vec![pane("p1", 2, "blocked")],
                pilot: Err(anyhow::anyhow!("agent.list failed")),
            },
            "/s".into(),
            Instant::now(),
        );
        assert_eq!(cache.panes()[0].revision, 2);
        assert!(cache.pilot_snapshot().is_none());
    }

    #[test]
    fn intervening_remove_cannot_be_restored_by_old_refresh() {
        let cache = PaneCache::new("test", vec![pane("p1", 1, "idle")]);
        cache.connected(1);
        let ticket = cache.begin_refresh();
        cache.apply_fenced(1, PaneEvent::Remove("p1".into()));
        cache.finish_refresh(
            ticket,
            vec![crate::pilot::Observation {
                pane: pane("p1", 1, "idle"),
                terminal_id: "t1".into(),
                agent_session: None,
            }],
            "/s".into(),
            Instant::now(),
        );
        assert!(cache.panes().is_empty());
        assert!(cache.pilot_snapshot().is_none());
    }

    #[test]
    fn intervening_create_cannot_be_erased_by_old_refresh() {
        let cache = PaneCache::new("test", vec![pane("p1", 1, "idle")]);
        cache.connected(1);
        let ticket = cache.begin_refresh();
        cache.apply_fenced(1, PaneEvent::Upsert(Box::new(pane("p2", 1, "working"))));
        cache.finish_refresh(
            ticket,
            vec![crate::pilot::Observation {
                pane: pane("p1", 1, "idle"),
                terminal_id: "t1".into(),
                agent_session: None,
            }],
            "/s".into(),
            Instant::now(),
        );
        assert_eq!(cache.panes().len(), 2);
        assert!(cache.pilot_snapshot().is_none());
    }
}
