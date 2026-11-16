//! Source attention is original cached evidence, never binding/focus authority.
use super::AgentState;
use crate::{
    runtime::STALE_AFTER_MS,
    source_observation::{SourceAvailability, SourceObservation},
};
use std::{collections::BTreeSet, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionState {
    Observed(AgentState),
    Empty,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceAttention {
    sampled_at: Instant,
    winner: Option<(AgentState, Instant)>,
    complete_since: Option<Instant>,
}
impl SourceAttention {
    pub fn from_observation(source: &SourceObservation, now: Instant) -> Self {
        let mut complete = source.availability == SourceAvailability::Available;
        let mut oldest = source.complete_since;
        for at in [
            source.complete_since,
            source.observed_at,
            source.last_success,
        ] {
            complete &= at.is_some_and(|at| fresh(at, now));
            oldest = oldest.zip(at).map(|(a, b)| a.min(b));
        }
        let mut identities = BTreeSet::new();
        let mut winner: Option<(AgentState, Instant)> = None;
        for row in &source.agents {
            // AIDEV-NOTE: authoritative command membership replaces unavailable
            // history retained by CachedSources; history cannot prevent Empty.
            if !row.available {
                continue;
            }
            if row.agent.source != source.id
                || row.agent.session.is_empty()
                || row.agent.pane_id.is_empty()
                || !identities.insert(row.agent.reference())
                || !fresh(row.observed_at, now)
            {
                complete = false;
                continue;
            }
            oldest = oldest.map(|at| at.min(row.observed_at));
            let state = AgentState::from_status(&row.agent.agent_status);
            if winner.is_none_or(|(previous, at)| {
                state.attention_rank() > previous.attention_rank()
                    || (state == previous && row.observed_at > at)
            }) {
                winner = Some((state, row.observed_at));
            }
        }
        Self {
            sampled_at: now,
            winner,
            complete_since: complete.then_some(oldest).flatten(),
        }
    }
    pub fn at(&self, now: Instant) -> AttentionState {
        if now < self.sampled_at {
            return AttentionState::Unknown;
        }
        let winner = self
            .winner
            .filter(|(_, at)| fresh(*at, now))
            .map(|(state, _)| state);
        if winner == Some(AgentState::Blocked) {
            return AttentionState::Observed(AgentState::Blocked);
        }
        if self.complete_since.is_some_and(|at| fresh(at, now)) {
            winner.map_or(AttentionState::Empty, AttentionState::Observed)
        } else {
            AttentionState::Unknown
        }
    }
}
fn fresh(at: Instant, now: Instant) -> bool {
    now.checked_duration_since(at)
        .is_some_and(|age| age.as_millis() < u128::from(STALE_AFTER_MS))
}
