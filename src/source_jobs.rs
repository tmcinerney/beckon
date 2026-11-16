//! Command-only jobs return immutable data; the main-thread cache is the only
//! authoritative source view used by bindings, render and runtime projection.
use crate::{
    config::Config,
    core::{Agent, AgentRef, AgentSource, PresentationTokenWrite},
    display::{DiagnosticCode, Recovery, TypedFailure},
    source_observation::{AgentObservation, SourceAvailability, SourceObservation},
};
use anyhow::{Context, Result, bail};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::Instant,
};

#[derive(Clone, Debug)]
pub enum SourceCommand {
    Fkey(AgentRef, Option<String>),
    Presentation(AgentRef, String),
    Navigate {
        reference: AgentRef,
        key: String,
        confirm: Option<Vec<String>>,
    },
}
pub enum SourceWork {
    Refresh(Config),
    Commands(Config, Vec<SourceCommand>),
}
pub enum SourceResult {
    Refreshed(Vec<SourceObservation>),
    Executed,
}

#[derive(Default)]
pub struct CachedSources {
    observations: Vec<SourceObservation>,
    commands: RefCell<Vec<SourceCommand>>,
}
impl CachedSources {
    pub fn mark_unavailable(&mut self, ids: impl IntoIterator<Item = String>, at: Instant) {
        for id in ids {
            if !self.observations.iter().any(|source| source.id == id) {
                self.observations.push(SourceObservation::unknown(&id));
            }
            let source = self
                .observations
                .iter_mut()
                .find(|source| source.id == id)
                .unwrap();
            source.availability = SourceAvailability::Unavailable;
            source.observed_at = Some(at);
            source.observed_sessions.clear();
            source.last_failure = Some(TypedFailure {
                code: DiagnosticCode::SourceUnavailable,
                at,
                active: true,
                recovery: Recovery::CheckSource,
            });
            for row in &mut source.agents {
                row.available = false;
            }
        }
    }
    pub fn observations(&self) -> Vec<SourceObservation> {
        self.observations.clone()
    }
    pub fn apply(&mut self, mut observations: Vec<SourceObservation>) {
        for source in &mut observations {
            if let Some(previous) = self.observations.iter().find(|old| old.id == source.id) {
                for old in &previous.agents {
                    if !source.observed_sessions.contains(&old.agent.session)
                        && !source
                            .agents
                            .iter()
                            .any(|row| row.agent.reference() == old.agent.reference())
                    {
                        let mut old = old.clone();
                        old.available = false;
                        source.agents.push(old);
                    }
                }
                if source.last_success.is_none() {
                    source.last_success = previous.last_success;
                }
                if source.last_failure.is_none() {
                    source.last_failure = previous.last_failure.clone().map(|mut failure| {
                        failure.active = false;
                        failure
                    });
                }
            }
            // Only history may be pruned; an oversized fresh result is rejected
            // by the executor, never truncated into an authoritative empty scope.
            source
                .agents
                .sort_by_key(|row| (!row.available, std::cmp::Reverse(row.observed_at)));
            source.agents.truncate(256);
        }
        for source in observations {
            if let Some(previous) = self.observations.iter_mut().find(|old| old.id == source.id) {
                *previous = source;
            } else {
                self.observations.push(source);
            }
        }
    }
    pub fn take_commands(&self) -> Vec<SourceCommand> {
        std::mem::take(&mut *self.commands.borrow_mut())
    }
    fn stage(&self, command: SourceCommand) -> Result<()> {
        let mut commands = self.commands.borrow_mut();
        if commands.len() >= 256 {
            bail!("source metadata queue full; ledger remains authoritative");
        }
        commands.push(command);
        Ok(())
    }
}
impl AgentSource for CachedSources {
    fn id(&self) -> &str {
        "sources"
    }
    fn panes(&self) -> Result<Vec<Agent>> {
        Ok(self
            .observations
            .iter()
            .flat_map(|source| &source.agents)
            .filter(|row| row.available && row.observed_at.elapsed().as_millis() < 15000)
            .map(|row| row.agent.clone())
            .collect())
    }
    fn observed_sessions(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .observations
            .iter()
            .flat_map(|source| {
                source
                    .observed_sessions
                    .iter()
                    .map(|scope| format!("{}:{scope}", source.id))
            })
            .collect())
    }
    fn observes(&self, source: &str, scope: &str) -> Result<bool> {
        Ok(self.observations.iter().any(|row| {
            row.id == source
                && row
                    .observed_at
                    .is_some_and(|at| at.elapsed().as_millis() < 15000)
                && row.observed_sessions.contains(scope)
        }))
    }
    fn focus_pane(&self, _: &AgentRef) -> Result<()> {
        bail!("navigation requires the command worker")
    }
    fn write_fkey(&self, pane: &AgentRef, key: Option<&str>) -> Result<()> {
        self.stage(SourceCommand::Fkey(pane.clone(), key.map(str::to_owned)))
    }
    fn write_presentation_tokens(
        &self,
        pane: &AgentRef,
        binding: &str,
    ) -> Result<PresentationTokenWrite> {
        self.stage(SourceCommand::Presentation(pane.clone(), binding.into()))?;
        Ok(PresentationTokenWrite::Written)
    }
}

#[derive(Default)]
pub struct SourceExecutor {
    sockets: BTreeMap<PathBuf, crate::herdr::HerdrSocket>,
}
impl SourceExecutor {
    pub fn execute(&mut self, work: SourceWork, deadline: Instant) -> Result<SourceResult> {
        crate::process_job::with_deadline(deadline, || match work {
            SourceWork::Refresh(config) => self.refresh(&config),
            SourceWork::Commands(config, commands) => {
                if commands.len() > 256 {
                    bail!("source command batch too large");
                }
                for command in commands {
                    if Instant::now() >= deadline {
                        bail!("source job deadline elapsed");
                    }
                    self.command(&config, command)?;
                }
                Ok(SourceResult::Executed)
            }
        })
    }
    fn refresh(&mut self, config: &Config) -> Result<SourceResult> {
        let mut observations = Vec::new();
        let mut captured = 0;
        let deadline = crate::process_job::deadline();
        for (index, spec) in config.sources.iter().enumerate() {
            let source = crate::command_source::CommandSource::new(spec.clone());
            // AIDEV-NOTE: explicit all-source jobs reserve time for later sources
            // and result handoff. Periodic jobs already contain exactly one source.
            let end = source_deadline(
                deadline,
                config.sources.len() - index + usize::from(config.herdr.enabled),
            );
            let _ = crate::process_job::with_deadline(end, || source.panes());
            let mut observation = source.observation();
            if observation.agents.len() > 256 {
                observation.agents.clear();
                observation.observed_sessions.clear();
                observation.availability = SourceAvailability::Unavailable;
                observation.last_failure = Some(failure(DiagnosticCode::SourceSnapshotInvalid));
            }
            let bytes = observation_bytes(&observation)?;
            if captured + bytes > 1048576 {
                observation.agents.clear();
                observation.observed_sessions.clear();
                observation.availability = SourceAvailability::Unavailable;
                observation.last_failure = Some(failure(DiagnosticCode::SourceSnapshotInvalid));
            } else {
                captured += bytes;
            }
            observations.push(observation);
        }
        if config.herdr.enabled {
            let mut observation = SourceObservation::unknown(crate::core::DEFAULT_SOURCE);
            let sessions = crate::herdr::discover_sessions(
                config.herdr.socket.as_deref(),
                config.herdr.allowed_sessions(),
            );
            let paths: BTreeSet<_> = sessions
                .iter()
                .take(256)
                .map(|session| session.socket_path.clone())
                .collect();
            self.sockets.retain(|path, _| paths.contains(path));
            let count = sessions.len().min(256);
            for (index, session) in sessions.into_iter().take(256).enumerate() {
                let socket = self
                    .sockets
                    .entry(session.socket_path.clone())
                    .or_insert_with(|| crate::herdr::HerdrSocket::new(session.socket_path));
                let end = source_deadline(deadline, count - index);
                match crate::process_job::with_deadline(end, || socket.labelled_panes()) {
                    Ok(rows) if observation.agents.len() + rows.len() <= 256 => {
                        let at = Instant::now();
                        observation.last_success = Some(at);
                        observation.observed_at = Some(at);
                        observation.observed_sessions.insert(session.name.clone());
                        observation.agents.extend(rows.into_iter().map(
                            |(mut agent, scope_title)| {
                                agent.session = session.name.clone();
                                let title = agent
                                    .terminal_title_stripped
                                    .clone()
                                    .or(agent.label.clone())
                                    .or(agent.terminal_title.clone());
                                AgentObservation {
                                    agent,
                                    available: true,
                                    observed_at: at,
                                    title_at: title.as_ref().map(|_| at),
                                    scope_title_at: scope_title.as_ref().map(|_| at),
                                    title,
                                    scope_title,
                                }
                            },
                        ));
                    }
                    _ => {
                        observation.last_failure = Some(failure(DiagnosticCode::SourceUnavailable))
                    }
                }
            }
            observation.availability =
                if observation.last_failure.is_some() || observation.last_success.is_none() {
                    SourceAvailability::Unavailable
                } else {
                    SourceAvailability::Available
                };
            if observation.last_success.is_none() {
                observation.last_failure = Some(failure(DiagnosticCode::SourceUnavailable));
            }
            if let Some(failure) = &observation.last_failure {
                observation.observed_at = Some(failure.at);
            }
            if captured + observation_bytes(&observation)? > 1048576 {
                observation.agents.clear();
                observation.observed_sessions.clear();
                observation.availability = SourceAvailability::Unavailable;
                observation.last_failure = Some(failure(DiagnosticCode::SourceSnapshotInvalid));
            }
            observations.push(observation);
        }
        Ok(SourceResult::Refreshed(observations))
    }
    fn command(&self, config: &Config, command: SourceCommand) -> Result<()> {
        let reference = match &command {
            SourceCommand::Fkey(reference, _)
            | SourceCommand::Presentation(reference, _)
            | SourceCommand::Navigate { reference, .. } => reference,
        };
        let source: Box<dyn AgentSource> =
            if reference.source == crate::core::DEFAULT_SOURCE && config.herdr.enabled {
                let session = crate::herdr::discover_sessions(
                    config.herdr.socket.as_deref(),
                    config.herdr.allowed_sessions(),
                )
                .into_iter()
                .find(|session| session.name == reference.session)
                .context("source session unavailable")?;
                Box::new(crate::herdr::HerdrCli::for_session(
                    &config.herdr.cli,
                    &session,
                ))
            } else {
                Box::new(crate::command_source::CommandSource::new(
                    config
                        .sources
                        .iter()
                        .find(|source| source.id == reference.source)
                        .context("unknown source")?
                        .clone(),
                ))
            };
        match command {
            SourceCommand::Fkey(reference, key) => source.write_fkey(&reference, key.as_deref()),
            SourceCommand::Presentation(reference, binding) => source
                .write_presentation_tokens(&reference, &binding)
                .map(|_| ()),
            SourceCommand::Navigate {
                reference,
                key,
                confirm,
            } => {
                if let Some(keys) = confirm {
                    return source.send_keys(
                        &reference,
                        &keys.iter().map(String::as_str).collect::<Vec<_>>(),
                    );
                }
                if reference.source == crate::core::DEFAULT_SOURCE {
                    // AIDEV-NOTE: adopted terminal raising and legacy [focus]
                    // belong only to Herdr. Other sources activate their own host.
                    use crate::focus::FocusAdapter;
                    let terminal = crate::terminal::from_config(&config.terminal)?;
                    let raised = terminal
                        .as_ref()
                        .and_then(|link| link.raise_for_session(&reference.session).ok().flatten());
                    crate::focus::CommandFocus::new(&config.focus).focus_terminal(
                        &crate::focus::FocusContext {
                            key: &key,
                            pane: &reference,
                            terminal_handle: raised.as_ref(),
                        },
                    )?;
                }
                source.focus_pane(&reference)
            }
        }
    }
}
fn source_deadline(deadline: Instant, remaining: usize) -> Instant {
    let now = Instant::now();
    let divisor = u32::try_from(remaining)
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    now + deadline.saturating_duration_since(now) / divisor
}
fn failure(code: DiagnosticCode) -> TypedFailure {
    TypedFailure {
        code,
        at: Instant::now(),
        active: true,
        recovery: Recovery::CheckSource,
    }
}

fn observation_bytes(source: &SourceObservation) -> Result<usize> {
    Ok(serde_json::to_vec(
        &source
            .agents
            .iter()
            .map(|row| (&row.agent, &row.title, &row.scope_title))
            .collect::<Vec<_>>(),
    )?
    .len())
}
