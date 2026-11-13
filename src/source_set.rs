//! Fans a set of independent agent integrations into one `AgentSource`.
//! Routing is by the `source` recorded on each identity, so the binding core
//! never needs to know how many integrations exist or how any of them names
//! its agents.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow};

use crate::core::{Agent, AgentRef, AgentSource, PresentationTokenWrite};

pub struct SourceSet {
    sources: Vec<Box<dyn AgentSource>>,
}

impl SourceSet {
    pub fn new(sources: Vec<Box<dyn AgentSource>>) -> Self {
        Self { sources }
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    fn route(&self, source: &str) -> Result<&dyn AgentSource> {
        self.route_opt(source)
            .ok_or_else(|| anyhow!("no agent source named {source:?}"))
    }

    /// A source may simply not be running. Dormant bindings from an absent
    /// source must not break release/publish paths.
    fn route_opt(&self, source: &str) -> Option<&dyn AgentSource> {
        self.sources
            .iter()
            .find(|candidate| candidate.id() == source)
            .map(|candidate| candidate.as_ref())
    }

    /// Identities reported by sources that declare `bind` environment vars.
    pub fn current_identities(&self) -> Vec<AgentRef> {
        self.sources
            .iter()
            .filter_map(|source| source.current_identity())
            .collect()
    }
}

impl AgentSource for SourceSet {
    fn id(&self) -> &str {
        "sources"
    }

    fn refresh(&self) {
        for source in &self.sources {
            source.refresh();
        }
    }

    fn panes(&self) -> Result<Vec<Agent>> {
        let mut agents = Vec::new();
        for source in &self.sources {
            agents.extend(source.panes()?);
        }
        Ok(agents)
    }

    fn observed_sessions(&self) -> Result<BTreeSet<String>> {
        let mut observed = BTreeSet::new();
        for source in &self.sources {
            for scope in source.observed_sessions()? {
                observed.insert(format!("{}:{}", source.id(), scope));
            }
        }
        Ok(observed)
    }

    fn observes(&self, source: &str, scope: &str) -> Result<bool> {
        match self.route_opt(source) {
            Some(child) => Ok(child.observed_sessions()?.contains(scope)),
            None => Ok(false),
        }
    }

    fn focus_pane(&self, pane: &AgentRef) -> Result<()> {
        self.route(&pane.source)?.focus_pane(pane)
    }

    fn write_fkey(&self, pane: &AgentRef, key: Option<&str>) -> Result<()> {
        match self.route_opt(&pane.source) {
            Some(child) => child.write_fkey(pane, key),
            None => Ok(()),
        }
    }

    fn write_presentation_tokens(
        &self,
        pane: &AgentRef,
        binding: &str,
    ) -> Result<PresentationTokenWrite> {
        match self.route_opt(&pane.source) {
            Some(child) => child.write_presentation_tokens(pane, binding),
            None => Ok(PresentationTokenWrite::PaneGone),
        }
    }

    fn send_keys(&self, pane: &AgentRef, keys: &[&str]) -> Result<()> {
        self.route(&pane.source)?.send_keys(pane, keys)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::BTreeSet, rc::Rc};

    use anyhow::Result;

    use crate::core::{Agent, AgentRef, AgentSource};

    use super::SourceSet;

    struct CountingSource {
        id: String,
        refreshes: Rc<Cell<usize>>,
    }

    impl AgentSource for CountingSource {
        fn id(&self) -> &str {
            &self.id
        }

        fn panes(&self) -> Result<Vec<Agent>> {
            Ok(Vec::new())
        }

        fn observed_sessions(&self) -> Result<BTreeSet<String>> {
            Ok(BTreeSet::new())
        }

        fn focus_pane(&self, _pane: &AgentRef) -> Result<()> {
            Ok(())
        }

        fn refresh(&self) {
            self.refreshes.set(self.refreshes.get() + 1);
        }
    }

    #[test]
    fn refresh_fans_out_to_every_source() {
        let first = Rc::new(Cell::new(0));
        let second = Rc::new(Cell::new(0));
        let sources = SourceSet::new(vec![
            Box::new(CountingSource {
                id: "first".into(),
                refreshes: first.clone(),
            }),
            Box::new(CountingSource {
                id: "second".into(),
                refreshes: second.clone(),
            }),
        ]);

        sources.refresh();

        assert_eq!(first.get(), 1);
        assert_eq!(second.get(), 1);
    }
}
