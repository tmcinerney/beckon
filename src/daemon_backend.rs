//! The actual daemon owner. Input/cleanup precede publication, cached sampling,
//! bounded IPC and one command-job result/handoff. No source/HID I/O on reads.
use super::{PresentationPublisher, Request, Response, dispatch_config_with_store};
use anyhow::{Result, bail};
use beckon::{
    config::Config,
    core::{AgentPresentation, AgentRef, AgentSource, BindingService, BindingStore},
    daemon_turn::TurnController,
    display::{DeviceInputBatch, DisplaySet, InputPollBudget},
    ipc::{MAX_ACCEPTS_PER_TURN, PeerPump},
    runtime::{BuildIdentity, LedgerRead, RuntimeInputs, RuntimeSnapshot, RuntimeState},
    slow_job::{JobOutcome, SourceJobs},
    source_jobs::{CachedSources, SourceCommand, SourceResult, SourceWork},
    switches::{Emitter, Switches},
};
use std::{
    collections::BTreeMap,
    os::unix::net::{UnixListener, UnixStream},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
mod tests;

enum Pending {
    Periodic {
        source: String,
    },
    Mutation {
        peer: u64,
        request: Request,
        revision: String,
    },
    Navigation {
        peer: Option<u64>,
        arm: Option<(String, AgentRef)>,
    },
    Metadata,
}
pub(super) struct State<E: Emitter, S: BindingStore> {
    pub config: Config,
    pub switches: Switches<E>,
    displays: DisplaySet,
    turns: TurnController,
    sources: CachedSources,
    jobs: SourceJobs<SourceWork, Result<SourceResult, String>>,
    pending: Option<(u64, Pending)>,
    job_navigation: bool,
    peers: PeerPump<UnixStream>,
    store: S,
    runtime: RuntimeState,
    snapshot: Option<RuntimeSnapshot>,
    plan: beckon::render::RenderPlan,
    refresh_due: BTreeMap<String, Instant>,
    refresh_cursor: usize,
    presentation: PresentationPublisher,
    confirm: beckon::action::RepeatPressConfirm,
}
impl<E: Emitter, S: BindingStore> State<E, S> {
    pub fn new(
        config: Config,
        switches: Switches<E>,
        displays: DisplaySet,
        jobs: SourceJobs<SourceWork, Result<SourceResult, String>>,
        store: S,
    ) -> Result<Self> {
        let instance = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        Ok(Self {
            turns: TurnController::new(&config.devices),
            switches,
            displays,
            sources: CachedSources::default(),
            jobs,
            pending: None,
            job_navigation: false,
            peers: PeerPump::default(),
            store,
            runtime: RuntimeState::new(instance, BuildIdentity::compiled()),
            snapshot: None,
            plan: beckon::render::render(&config.display, &[])?,
            refresh_due: BTreeMap::new(),
            refresh_cursor: 0,
            presentation: PresentationPublisher::default(),
            confirm: Default::default(),
            config,
        })
    }
    pub fn start(&mut self) -> Result<()> {
        let started = self.displays.start_transports();
        for device in self.config.devices.iter().filter(|device| {
            matches!(
                device.definition.adapter.as_str(),
                "creator-micro" | "glove80"
            )
        }) {
            if !started.iter().any(|(id, result)| {
                id == &device.id
                    && matches!(
                        result,
                        Ok(beckon::display::TransportStart::Started
                            | beckon::display::TransportStart::Running)
                    )
            }) {
                self.displays.stop_transports();
                bail!(
                    "device {} has no exclusive running transport owner; refusing legacy main-thread HID fallback",
                    device.id
                );
            }
        }
        Ok(())
    }
    pub fn stop(&mut self) {
        for device in &self.config.devices {
            let _ = self.switches.reset_device(&device.id);
        }
        self.displays.stop_transports();
    }
    pub fn step(&mut self, listener: &UnixListener, host: Vec<DeviceInputBatch>) -> Instant {
        let now = Instant::now();
        let navigation_active = self.navigation_active();
        let outcome = self.turns.service_transports(
            &mut self.switches,
            &mut self.displays,
            host,
            InputPollBudget {
                deadline: now + Duration::from_millis(5),
                max_raw_frames: 128,
            },
            Instant::now,
            navigation_active,
        );
        let immediate = outcome.needs_service;
        for failure in outcome.control_failures {
            eprintln!(
                "control {}.{}: {:?}",
                failure.device, failure.control, failure.code
            );
        }
        for (device, control, failure) in self.switches.control_observations() {
            self.runtime
                .record(&failure, Some(device), Some(control), None, now);
        }
        for (device, failure) in outcome.input_failures {
            self.runtime.record(&failure, Some(device), None, None, now);
        }
        for address in outcome.activations {
            let binding = self
                .config
                .devices
                .iter()
                .filter(|device| device.id == address.device)
                .flat_map(|device| device.definition.keys.values())
                .find(|key| key.members.contains(&address))
                .and_then(|key| key.binding.clone());
            if let Some(key) = binding
                && let Err(error) = self.navigate(key, None)
            {
                eprintln!("navigation: {error:#}");
            }
        }
        for failure in self
            .displays
            .publish_feedback(&self.plan, &self.switches.treatments())
        {
            eprintln!("display {}: {}", failure.adapter, failure.message);
        }
        self.turns
            .request_captures(&mut self.switches, &mut self.displays, Instant::now());
        if let Err(error) = self.sample(Instant::now()) {
            eprintln!("runtime sample: {error:#}");
        }
        // Each accepted socket remains nonblocking; no accepted-client loop,
        // read_line, flush or write_all is reachable in this production path.
        for _ in 0..MAX_ACCEPTS_PER_TURN {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = self.peers.admit_unix(stream, Instant::now());
                }
                Err(_) => break,
            }
        }
        let now = Instant::now();
        if let Some(request) = self.peers.poll(now) {
            match serde_json::from_slice::<Request>(&request.bytes) {
                Ok(value) => {
                    if let Err(error) = self.request(request.id, value, now) {
                        self.reply(request.id, Err(error), now);
                    }
                }
                Err(error) => self.reply(request.id, Err(error.into()), now),
            }
        }
        self.complete(Instant::now());
        if !self.jobs.is_busy() && !self.switches.input_active() {
            let commands = self.sources.take_commands();
            if !commands.is_empty() {
                let _ = self.submit(
                    SourceWork::Commands(self.config.clone(), commands),
                    Pending::Metadata,
                    Instant::now(),
                );
            } else if let Some((source, config)) = self.next_source_refresh(now) {
                let _ = self.submit(
                    SourceWork::Refresh(config),
                    Pending::Periodic { source },
                    Instant::now(),
                );
            }
        }
        if immediate {
            return Instant::now();
        }
        self.switches
            .next_deadline()
            .into_iter()
            .chain(self.turns.next_capture_wake())
            .min()
            .map_or(now + Duration::from_millis(10), |at| {
                at.min(now + Duration::from_millis(10))
            })
            .max(now)
    }
    fn next_source_refresh(&mut self, now: Instant) -> Option<(String, Config)> {
        let mut targets = self
            .config
            .sources
            .iter()
            .map(|source| (source.id.clone(), source.refresh_ms.max(1)))
            .collect::<Vec<_>>();
        if self.config.herdr.enabled {
            targets.push((beckon::core::DEFAULT_SOURCE.into(), 1000));
        }
        for offset in 0..targets.len() {
            let index = (self.refresh_cursor + offset) % targets.len();
            let (source, interval) = &targets[index];
            if self.refresh_due.get(source).is_some_and(|due| now < *due) {
                continue;
            }
            self.refresh_cursor = (index + 1) % targets.len();
            self.refresh_due
                .insert(source.clone(), now + Duration::from_millis(*interval));
            let mut config = self.config.clone();
            config.sources.retain(|spec| spec.id == *source);
            config.herdr.enabled =
                self.config.herdr.enabled && source == beckon::core::DEFAULT_SOURCE;
            return Some((source.clone(), config));
        }
        None
    }
    fn sample(&mut self, now: Instant) -> Result<()> {
        let observations = self.sources.observations();
        let ledger = match self.store.load() {
            Ok(Some(state)) => LedgerRead::Available(state),
            Ok(None) => LedgerRead::Unavailable,
            Err(_) => LedgerRead::Invalid,
        };
        if let LedgerRead::Available(state) = &ledger {
            let agents = self.sources.panes()?;
            let bound = state
                .bindings
                .iter()
                .filter_map(|binding| {
                    agents
                        .iter()
                        .find(|agent| binding.matches_agent(agent))
                        .map(|agent| (binding.clone(), agent.clone()))
                })
                .collect::<Vec<_>>();
            self.plan = beckon::render::render(&self.config.display, &bound)?;
        }
        // AIDEV-NOTE: source attention is independent of ledger readability and
        // shares the same immutable sample as runtime; publication rechecks age.
        self.plan.source_attention = observations
            .iter()
            .map(|source| {
                (
                    source.id.clone(),
                    beckon::render::attention::SourceAttention::from_observation(source, now),
                )
            })
            .collect();
        self.snapshot = Some(
            self.runtime.capture(
                RuntimeInputs {
                    configuration: self.config.model()?,
                    ledger,
                    sources: observations,
                    devices: self.displays.diagnostics(),
                    controls: self.switches.snapshot(now),
                    posting: self.switches.posting_observation(),
                },
                now,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)?
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
            )?,
        );
        Ok(())
    }
    fn request(&mut self, peer: u64, request: Request, now: Instant) -> Result<()> {
        match request {
            Request::RuntimeSnapshot { version } => {
                let bytes = self
                    .snapshot
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("runtime snapshot not yet available"))?
                    .reply_line(version, now)?;
                let _ = self.peers.respond(peer, bytes, now);
                Ok(())
            }
            Request::Status {} => {
                let value = self
                    .snapshot
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("runtime snapshot not yet available"))?
                    .legacy_status_value();
                self.reply(peer, Ok(value), now);
                Ok(())
            }
            Request::FocusKey { key } => self.navigate(key, Some(peer)),
            Request::ConfigModel {}
            | Request::SetPresentation { .. }
            | Request::SetTheme { .. } => {
                let changed = !matches!(request, Request::ConfigModel {});
                let result = dispatch_config_with_store(
                    request,
                    &self.sources,
                    &mut self.config,
                    &self.store,
                );
                if result.is_ok() && changed {
                    self.switches.refresh_presentation(&self.config.devices);
                    self.displays.refresh_presentation(&self.config.devices)?;
                }
                self.reply(peer, result, now);
                Ok(())
            }
            Request::ResetControls { device } => {
                if !self
                    .config
                    .devices
                    .iter()
                    .any(|candidate| candidate.id == device)
                {
                    bail!("unknown device {device:?}");
                }
                let result = self
                    .switches
                    .reset_device(&device)
                    .map(|()| serde_json::json!({"reset":device}));
                self.reply(peer, result, now);
                Ok(())
            }
            mutation => {
                self.prepare_slow()?;
                let revision = self.config.model()?.revision;
                self.submit(
                    SourceWork::Refresh(self.config.clone()),
                    Pending::Mutation {
                        peer,
                        request: mutation,
                        revision,
                    },
                    now,
                )
            }
        }
    }
    fn prepare_slow(&mut self) -> Result<()> {
        for device in &self.config.devices {
            self.switches.cancel_pending(&device.id);
        }
        if self.jobs.is_busy() || self.switches.slow_work_blocked() {
            bail!("busy; input/output or a source job is active");
        }
        Ok(())
    }
    fn navigate(&mut self, key: String, peer: Option<u64>) -> Result<()> {
        self.prepare_slow()?;
        let reference = BindingService::new(&self.store, &self.sources)
            .with_keys(&self.config.bindings)
            .pane_for_key(&key)?;
        let now = Instant::now();
        let focused = self
            .sources
            .panes()?
            .iter()
            .any(|agent| agent.matches(&reference) && agent.focused);
        let confirmed = self.config.actions.confirm.enabled
            && self.confirm.take_if_ready(&key, &reference, focused, now);
        let arm = (!confirmed && self.config.actions.confirm.enabled)
            .then(|| (key.clone(), reference.clone()));
        let command = SourceCommand::Navigate {
            reference,
            key,
            confirm: confirmed.then(|| self.config.actions.confirm.keys.clone()),
        };
        self.submit(
            SourceWork::Commands(self.config.clone(), vec![command]),
            Pending::Navigation { peer, arm },
            now,
        )
    }
    fn submit(&mut self, work: SourceWork, pending: Pending, now: Instant) -> Result<()> {
        let id = self
            .jobs
            .submit(work, now + Duration::from_millis(1000))
            .map_err(anyhow::Error::msg)?;
        self.job_navigation = matches!(pending, Pending::Navigation { .. });
        self.pending = Some((id, pending));
        Ok(())
    }
    fn navigation_active(&self) -> bool {
        // Timeout consumes the reply, not the executing lease. Retain the job's
        // original kind until actual worker return; background reads never navigate.
        self.jobs.is_busy() && self.job_navigation
    }
    fn complete(&mut self, now: Instant) {
        let Some(outcome) = self.jobs.poll(now) else {
            return;
        };
        let (id, result) = match outcome {
            JobOutcome::Completed { id, result } => (id, result.map_err(anyhow::Error::msg)),
            JobOutcome::TimedOut { id } | JobOutcome::WorkerStopped { id } => (
                id,
                Err(anyhow::anyhow!(
                    "source job expired/stopped; external outcome may be unknown, not replayed"
                )),
            ),
        };
        let Some((expected, pending)) = self.pending.take() else {
            return;
        };
        if id != expected {
            return;
        }
        match pending {
            Pending::Mutation {
                peer,
                request,
                revision,
            } => {
                if !self.peers.contains(peer, now) {
                    return;
                }
                let result = (|| {
                    if self.config.model()?.revision != revision {
                        bail!("configuration changed while source job was pending");
                    }
                    // A failed aggregate job has no source-specific new evidence.
                    // Keep original samples/ages; per-source failures arrive in results.
                    let SourceResult::Refreshed(observations) = result? else {
                        bail!("unexpected source result");
                    };
                    self.sources.apply(observations);
                    if matches!(request, Request::Refresh {}) {
                        return Ok(serde_json::json!({"refreshed":true}));
                    }
                    dispatch_config_with_store(
                        request,
                        &self.sources,
                        &mut self.config,
                        &self.store,
                    )
                })();
                self.reply(peer, result, now);
            }
            Pending::Periodic { source } => {
                if let Ok(SourceResult::Refreshed(observations)) = result
                    && observations.len() == 1
                    && observations[0].id == source
                {
                    self.sources.apply(observations);
                    if !self.switches.input_active() {
                        // AIDEV-NOTE: background refresh is not a binding operation.
                        // Never reconcile/import tokens or establish an empty ledger here.
                        if let Ok(Some(ledger)) = self.store.load()
                            && let Ok(agents) = self.sources.panes()
                        {
                            let presentation = agents
                                .into_iter()
                                .map(|agent| {
                                    let binding = ledger
                                        .bindings
                                        .iter()
                                        .find(|binding| binding.matches_agent(&agent))
                                        .map(|binding| binding.key.to_ascii_uppercase())
                                        .unwrap_or_else(|| "unbound".into());
                                    AgentPresentation {
                                        title: agent.display_title(),
                                        binding,
                                        source: agent.source,
                                        session: agent.session,
                                        pane_id: agent.pane_id,
                                        agent_status: agent.agent_status,
                                        focused: agent.focused,
                                    }
                                })
                                .collect();
                            let _ = self.presentation.publish(&self.sources, presentation);
                        }
                    }
                } else {
                    self.sources.mark_unavailable([source], now);
                }
            }
            Pending::Navigation { peer, arm } => {
                if result.is_ok()
                    && peer.is_none_or(|peer| self.peers.contains(peer, now))
                    && let Some((key, reference)) = arm
                {
                    self.confirm.arm(
                        &key,
                        &reference,
                        Duration::from_millis(self.config.actions.confirm.repeat_press_ms),
                        now,
                    );
                }
                if let Some(peer) = peer {
                    self.reply(
                        peer,
                        result.map(|_| serde_json::json!({"focused":true})),
                        now,
                    );
                }
            }
            Pending::Metadata => {
                if let Err(error) = result {
                    eprintln!("source metadata: {error:#}");
                }
            }
        }
    }
    fn reply(&mut self, peer: u64, result: Result<serde_json::Value>, now: Instant) {
        let response = match result {
            Ok(data) => Response::Ok { data },
            Err(error) => Response::Error {
                message: format!("{error:#}"),
            },
        };
        if let Ok(bytes) = serde_json::to_vec(&response) {
            let _ = self.peers.respond(peer, bytes, now);
        }
    }
}
