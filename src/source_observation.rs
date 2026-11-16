//! Immutable cached source evidence. Accessors must never execute commands or
//! reconcile binding ownership. Times belong to the producing refresh/event.
use crate::{core::Agent, display::TypedFailure};
use std::{collections::BTreeSet, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    Unknown,
    Available,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct AgentObservation {
    pub agent: Agent,
    pub available: bool,
    pub observed_at: Instant,
    pub title: Option<String>,
    pub title_at: Option<Instant>,
    pub scope_title: Option<String>,
    pub scope_title_at: Option<Instant>,
}

#[derive(Clone, Debug)]
pub struct SourceObservation {
    pub id: String,
    pub availability: SourceAvailability,
    pub observed_at: Option<Instant>,
    pub last_success: Option<Instant>,
    pub last_failure: Option<TypedFailure>,
    pub observed_sessions: BTreeSet<String>,
    pub agents: Vec<AgentObservation>,
}
impl SourceObservation {
    pub fn unknown(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            availability: SourceAvailability::Unknown,
            observed_at: None,
            last_success: None,
            last_failure: None,
            observed_sessions: BTreeSet::new(),
            agents: Vec::new(),
        }
    }
}

pub fn real_title(agent: &Agent) -> Option<String> {
    [
        agent.terminal_title_stripped.as_deref(),
        agent.label.as_deref(),
        agent.terminal_title.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find(|title| !title.trim().is_empty())
    .map(|title| title.trim().to_string())
}
