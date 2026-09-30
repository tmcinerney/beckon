//! Generic routing of session-qualified pane operations to per-session
//! directories.
//!
//! Herdr pane IDs are only unique within one session, so Beckon's core works
//! with [`PaneRef`] values that name both. This module owns the routing policy
//! — which session a pane belongs to, and how snapshots from several sessions
//! combine — while the per-session directory implementations stay ignorant of
//! each other. Nothing here understands Herdr sockets or macOS; the daemon
//! composes it over live socket caches and one-shot CLI paths compose it over
//! CLI-backed directories.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};

use crate::core::{Pane, PaneDirectory, PaneRef, PresentationTokenWrite};

pub struct SessionRouter<D> {
    directories: BTreeMap<String, D>,
}

impl<D> SessionRouter<D> {
    pub fn new(directories: BTreeMap<String, D>) -> Self {
        Self { directories }
    }

    /// Session names in deterministic order. This is the set of sessions whose
    /// pane snapshots are considered authoritative.
    pub fn sessions(&self) -> impl Iterator<Item = &str> {
        self.directories.keys().map(String::as_str)
    }

    fn route(&self, pane: &PaneRef) -> Result<&D> {
        self.directories.get(&pane.session).with_context(|| {
            format!(
                "pane {} belongs to session {} which is not managed{}",
                pane.pane_id,
                pane.session,
                self.known_sessions_note()
            )
        })
    }

    fn known_sessions_note(&self) -> String {
        let known = self.directories.keys().cloned().collect::<Vec<_>>();
        if known.is_empty() {
            String::new()
        } else {
            format!(" (managed sessions: {})", known.join(", "))
        }
    }
}

impl<D: PaneDirectory> PaneDirectory for SessionRouter<D> {
    fn panes(&self) -> Result<Vec<Pane>> {
        let mut panes = Vec::new();
        for (session, directory) in &self.directories {
            for mut pane in directory
                .panes()
                .with_context(|| format!("read panes of Herdr session {session}"))?
            {
                pane.session.clone_from(session);
                panes.push(pane);
            }
        }
        panes.sort_by(|left, right| {
            left.session
                .cmp(&right.session)
                .then_with(|| left.pane_id.cmp(&right.pane_id))
        });
        Ok(panes)
    }

    fn observed_sessions(&self) -> Result<BTreeSet<String>> {
        Ok(self.directories.keys().cloned().collect())
    }

    fn write_fkey(&self, pane: &PaneRef, key: Option<&str>) -> Result<()> {
        self.route(pane)?.write_fkey(pane, key)
    }

    fn write_presentation_tokens(
        &self,
        pane: &PaneRef,
        binding: Option<&str>,
    ) -> Result<PresentationTokenWrite> {
        self.route(pane)?.write_presentation_tokens(pane, binding)
    }

    fn focus_pane(&self, pane: &PaneRef) -> Result<()> {
        self.route(pane)?.focus_pane(pane)
    }

    fn send_keys(&self, pane: &PaneRef, keys: &[&str]) -> Result<()> {
        self.route(pane)?.send_keys(pane, keys)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct RecordingDirectory {
        session: &'static str,
        panes: Vec<Pane>,
        focused: RefCell<Vec<PaneRef>>,
    }

    impl RecordingDirectory {
        fn new(session: &'static str, pane_ids: &[&str]) -> Self {
            Self {
                session,
                panes: pane_ids
                    .iter()
                    .map(|pane_id| Pane {
                        pane_id: (*pane_id).into(),
                        session: session.into(),
                        revision: 0,
                        agent_status: "idle".into(),
                        agent: None,
                        label: None,
                        cwd: None,
                        terminal_title: None,
                        terminal_title_stripped: None,
                        focused: false,
                        tokens: BTreeMap::new(),
                    })
                    .collect(),
                focused: RefCell::new(Vec::new()),
            }
        }
    }

    impl PaneDirectory for RecordingDirectory {
        fn panes(&self) -> Result<Vec<Pane>> {
            Ok(self.panes.clone())
        }

        fn observed_sessions(&self) -> Result<BTreeSet<String>> {
            Ok(BTreeSet::from([self.session.to_string()]))
        }

        fn write_fkey(&self, _pane: &PaneRef, _key: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn write_presentation_tokens(
            &self,
            _pane: &PaneRef,
            _binding: Option<&str>,
        ) -> Result<PresentationTokenWrite> {
            Ok(PresentationTokenWrite::Written)
        }

        fn focus_pane(&self, pane: &PaneRef) -> Result<()> {
            assert_eq!(pane.session, self.session, "routed to the wrong session");
            self.focused.borrow_mut().push(pane.clone());
            Ok(())
        }

        fn send_keys(&self, _pane: &PaneRef, _keys: &[&str]) -> Result<()> {
            Ok(())
        }
    }

    fn router() -> SessionRouter<RecordingDirectory> {
        SessionRouter::new(BTreeMap::from([
            (
                "agent-workspace".to_string(),
                RecordingDirectory::new("agent-workspace", &["w6:p1"]),
            ),
            (
                "default".to_string(),
                RecordingDirectory::new("default", &["wB:p1", "wB:p2"]),
            ),
        ]))
    }

    #[test]
    fn merges_pane_snapshots_with_sessions_and_deterministic_order() {
        let panes = router().panes().unwrap();
        let ids = panes
            .iter()
            .map(|pane| format!("{}:{}", pane.session, pane.pane_id))
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["agent-workspace:w6:p1", "default:wB:p1", "default:wB:p2"]
        );
    }

    #[test]
    fn routes_mutations_to_the_owning_session() {
        let router = router();
        router
            .focus_pane(&PaneRef::new("agent-workspace", "w6:p1"))
            .unwrap();
        router.focus_pane(&PaneRef::new(DEFAULT, "wB:p2")).unwrap();

        let agent = router.directories.get("agent-workspace").unwrap();
        assert_eq!(
            *agent.focused.borrow(),
            vec![PaneRef::new("agent-workspace", "w6:p1")]
        );
        let default = router.directories.get("default").unwrap();
        assert_eq!(
            *default.focused.borrow(),
            vec![PaneRef::new("default", "wB:p2")]
        );
    }

    const DEFAULT: &str = crate::core::DEFAULT_SESSION;

    #[test]
    fn reports_observed_sessions_and_rejects_unknown_ones() {
        let router = router();
        assert_eq!(
            router.observed_sessions().unwrap(),
            BTreeSet::from(["agent-workspace".to_string(), "default".to_string()])
        );

        let error = router
            .focus_pane(&PaneRef::new("stopped-session", "w1:p1"))
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("stopped-session"), "{message}");
        assert!(
            message.contains("managed sessions: agent-workspace, default"),
            "{message}"
        );
    }
}
