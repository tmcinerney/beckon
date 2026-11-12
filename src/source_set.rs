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
        self.sources
            .iter()
            .find(|candidate| candidate.id() == source)
            .map(|candidate| candidate.as_ref())
            .ok_or_else(|| anyhow!("no agent source named {source:?}"))
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
        Ok(self.route(source)?.observed_sessions()?.contains(scope))
    }

    fn focus_pane(&self, pane: &AgentRef) -> Result<()> {
        self.route(&pane.source)?.focus_pane(pane)
    }

    fn write_fkey(&self, pane: &AgentRef, key: Option<&str>) -> Result<()> {
        self.route(&pane.source)?.write_fkey(pane, key)
    }

    fn write_presentation_tokens(
        &self,
        pane: &AgentRef,
        binding: &str,
    ) -> Result<PresentationTokenWrite> {
        self.route(&pane.source)?
            .write_presentation_tokens(pane, binding)
    }

    fn send_keys(&self, pane: &AgentRef, keys: &[&str]) -> Result<()> {
        self.route(&pane.source)?.send_keys(pane, keys)
    }
}
