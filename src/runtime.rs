//! Cache-only coherent runtime projection. No source/device I/O, binding
//! reconciliation or settings writes are reachable from capture/reply methods.
use crate::{
    config::ConfigurationModel,
    core::{AgentRef, BindingState, STATE_VERSION, validate_bindings},
    display::{
        Connectivity, DeviceObservation, DiagnosticCode, MemberDescriptor, Recovery, TypedFailure,
    },
    source_observation::{AgentObservation, SourceAvailability, SourceObservation},
    switches::{ControlPhase, ControlSnapshot},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};

pub const STALE_AFTER_MS: u64 = 15000;
fn age(now: Instant, at: Option<Instant>) -> Option<u64> {
    at.and_then(|at| now.checked_duration_since(at))
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
}
fn fresh(now: Instant, at: Option<Instant>) -> bool {
    age(now, at).is_some_and(|ms| ms < STALE_AFTER_MS)
}

#[derive(Clone, Debug, Serialize)]
pub struct BuildIdentity {
    pub version: String,
    pub revision: Option<String>,
    pub dirty: Option<bool>,
}
impl BuildIdentity {
    pub fn compiled() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            revision: option_env!("BECKON_BUILD_REVISION")
                .filter(|value| {
                    !value.is_empty()
                        && value.len() <= 64
                        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .map(str::to_string),
            dirty: option_env!("BECKON_BUILD_DIRTY").and_then(|value| match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PostingPermission {
    #[default]
    Unknown,
    Allowed,
    Denied,
}
#[derive(Clone, Debug, Default)]
pub struct PostingObservation {
    pub permission: PostingPermission,
    pub checked_at: Option<Instant>,
    pub last_success: Option<Instant>,
    pub last_failure: Option<TypedFailure>,
    pub cleanup_pending: bool,
}
pub enum LedgerRead {
    Available(BindingState),
    Unavailable,
    Invalid,
}
pub struct RuntimeInputs {
    pub configuration: ConfigurationModel,
    pub ledger: LedgerRead,
    pub sources: Vec<SourceObservation>,
    pub devices: BTreeMap<String, DeviceObservation>,
    pub controls: Vec<ControlSnapshot>,
    pub posting: PostingObservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Free,
    Live,
    Dormant,
    Unknown,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}
#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticEvent {
    pub id: u64,
    pub age_ms: u64,
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub device: Option<String>,
    pub control: Option<String>,
    pub source: Option<String>,
    pub active: bool,
    pub count: u64,
    pub recovery: Recovery,
}
#[derive(Clone)]
struct StoredEvent {
    event: DiagnosticEvent,
    at: Instant,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EventScope {
    Posting,
    Ledger,
    Source,
    Device,
}
type Context = (EventScope, Option<String>, Option<String>, Option<String>);
fn event_context(
    code: DiagnosticCode,
    device: Option<String>,
    control: Option<String>,
    source: Option<String>,
) -> Context {
    let scope = if source.is_some() {
        EventScope::Source
    } else if device.is_some() {
        EventScope::Device
    } else if matches!(
        code,
        DiagnosticCode::LedgerInvalid | DiagnosticCode::LedgerUnavailable
    ) {
        EventScope::Ledger
    } else {
        EventScope::Posting
    };
    (scope, device, control, source)
}

#[derive(Debug, Serialize)]
pub struct BindingLegend {
    pub key: String,
    pub configured: bool,
    pub owner: Option<AgentRef>,
    pub availability: Availability,
    pub scope_title: Option<String>,
    pub title: Option<String>,
    pub labels_stale: bool,
    pub age_ms: Option<u64>,
    pub labels_age_ms: Option<u64>,
}
#[derive(Debug, Serialize)]
pub struct ObservedPane {
    pub reference: AgentRef,
    pub scope_title: Option<String>,
    pub title: Option<String>,
    pub agent_status: String,
    pub focused: bool,
    pub availability: Availability,
    pub labels_stale: bool,
    pub age_ms: Option<u64>,
    pub labels_age_ms: Option<u64>,
}
#[derive(Debug, Serialize)]
pub struct SourceHealth {
    pub id: String,
    pub availability: SourceAvailability,
    pub observed_age_ms: Option<u64>,
    pub last_success_age_ms: Option<u64>,
    pub last_error: Option<DiagnosticEvent>,
}
#[derive(Debug, Serialize)]
pub struct ObservedLayer {
    pub name: Option<String>,
    pub profile: Option<u16>,
    pub position: Option<u16>,
    pub observed_age_ms: u64,
}
#[derive(Debug, Serialize)]
pub struct RuntimeControl {
    pub id: String,
    pub phase: ControlPhase,
    pub pressed: bool,
    pub cleanup_pending: bool,
}
#[derive(Debug, Serialize)]
pub struct DeviceHealth {
    pub id: String,
    pub adapter: String,
    pub observation_supported: bool,
    pub connectivity: Connectivity,
    pub observed_age_ms: Option<u64>,
    pub member_descriptors: Vec<MemberDescriptor>,
    pub layer: Option<ObservedLayer>,
    pub last_input_success_age_ms: Option<u64>,
    pub last_output_success_age_ms: Option<u64>,
    pub last_error: Option<DiagnosticEvent>,
    pub controls: Vec<RuntimeControl>,
}
#[derive(Debug, Serialize)]
pub struct PostingHealth {
    pub permission: PostingPermission,
    pub last_checked_age_ms: Option<u64>,
    pub last_success_age_ms: Option<u64>,
    pub last_error: Option<DiagnosticEvent>,
    pub cleanup_pending: bool,
}

#[derive(Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub version: u32,
    pub instance_id: String,
    pub sequence: u64,
    pub sampled_at_unix_ms: u64,
    pub age_ms: u64,
    pub stale_after_ms: u64,
    pub build: BuildIdentity,
    pub configuration_revision: String,
    pub configuration: ConfigurationModel,
    pub ledger_available: bool,
    pub posting: PostingHealth,
    pub devices: Vec<DeviceHealth>,
    pub bindings: Vec<BindingLegend>,
    pub panes: Vec<ObservedPane>,
    pub sources: Vec<SourceHealth>,
    pub events: Vec<DiagnosticEvent>,
    #[serde(skip)]
    captured_at: Instant,
    #[serde(skip)]
    legacy_agents: Vec<crate::core::Agent>,
}
impl RuntimeSnapshot {
    /// Legacy shape, projected from this same captured ledger/source view.
    /// Unlike BindingService::status/panes this cannot reconcile or write tokens.
    pub fn legacy_status_value(&self) -> serde_json::Value {
        let bindings: Vec<_> = self.bindings.iter().filter(|binding| binding.availability == Availability::Live)
            .filter_map(|binding| {
                let reference = binding.owner.as_ref()?;
                let agent = self.legacy_agents.iter().find(|agent| agent.matches(reference))?;
                Some(serde_json::json!({"key":binding.key,"source":reference.source,"session":reference.session,"pane":agent}))
            }).collect();
        let panes: Vec<_> = self.legacy_agents.iter().map(|agent| {
            let reference = agent.reference();
            let binding = self.bindings.iter().find(|binding| binding.owner.as_ref() == Some(&reference))
                .map(|binding| binding.key.to_ascii_uppercase()).unwrap_or_else(|| "unbound".into());
            serde_json::json!({"source":agent.source,"session":agent.session,"pane_id":agent.pane_id,
                "title":agent.display_title(),"binding":binding,"agent_status":agent.agent_status,"focused":agent.focused})
        }).collect();
        serde_json::json!({"bindings":bindings,"panes":panes})
    }
    pub fn reply_line(&self, version: u32, now: Instant) -> anyhow::Result<Vec<u8>> {
        let reply = if version != 1 {
            serde_json::json!({"type":"error","message":"unsupported runtime snapshot version"})
        } else {
            let mut value = serde_json::to_value(self)?;
            value["age_ms"] =
                serde_json::json!(age(now, Some(self.captured_at)).unwrap_or(u64::MAX));
            serde_json::json!({"type":"ok","data":value})
        };
        let mut bytes = serde_json::to_vec(&reply)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

pub struct RuntimeState {
    instance_id: String,
    build: BuildIdentity,
    sequence: u64,
    next_event: u64,
    events: VecDeque<StoredEvent>,
    latest: BTreeMap<Context, StoredEvent>,
    labels: BTreeMap<AgentRef, AgentObservation>,
}
impl RuntimeState {
    pub fn new(instance_id: String, build: BuildIdentity) -> Self {
        Self {
            instance_id,
            build,
            sequence: 0,
            next_event: 0,
            events: VecDeque::new(),
            latest: BTreeMap::new(),
            labels: BTreeMap::new(),
        }
    }

    pub fn record(
        &mut self,
        failure: &TypedFailure,
        device: Option<String>,
        control: Option<String>,
        source: Option<String>,
        now: Instant,
    ) -> DiagnosticEvent {
        let context = event_context(
            failure.code,
            device.clone(),
            control.clone(),
            source.clone(),
        );
        if let Some(current) = self.latest.get_mut(&context)
            && current.at == failure.at
            && current.event.code == failure.code
        {
            current.event.active = failure.active;
            for stored in &mut self.events {
                if stored.event.id == current.event.id {
                    stored.event.active = failure.active;
                }
            }
            let mut event = current.event.clone();
            event.age_ms = age(now, Some(current.at)).unwrap_or(u64::MAX);
            return event;
        }
        // A new failure supersedes the active state in this exact safe context;
        // historical events remain, never raw external messages.
        for old in &mut self.events {
            if event_context(
                old.event.code,
                old.event.device.clone(),
                old.event.control.clone(),
                old.event.source.clone(),
            ) == context
            {
                old.event.active = false;
            }
        }
        self.next_event = self
            .next_event
            .checked_add(1)
            .expect("diagnostic id exhausted");
        let severity = match failure.code {
            DiagnosticCode::DeviceUnavailable | DiagnosticCode::Recovered => Severity::Info,
            DiagnosticCode::SourceUnavailable | DiagnosticCode::SourceSnapshotInvalid => {
                Severity::Warning
            }
            _ => Severity::Error,
        };
        let mut event = DiagnosticEvent {
            id: self.next_event,
            age_ms: 0,
            severity,
            code: failure.code,
            device,
            control,
            source,
            active: failure.active && failure.code != DiagnosticCode::DeviceUnavailable,
            count: 1,
            recovery: failure.recovery,
        };
        if let Some(last) = self.events.back_mut()
            && last.event.code == event.code
            && event_context(
                last.event.code,
                last.event.device.clone(),
                last.event.control.clone(),
                last.event.source.clone(),
            ) == context
            && last.event.recovery == event.recovery
        {
            event.id = last.event.id;
            event.count = last.event.count.saturating_add(1);
            self.events.pop_back();
        }
        let stored = StoredEvent {
            event: event.clone(),
            at: failure.at,
        };
        self.latest.insert(context, stored.clone());
        self.events.push_back(stored);
        if self.events.len() > 64 {
            self.events.pop_front();
        }
        event.age_ms = age(now, Some(failure.at)).unwrap_or(u64::MAX);
        event
    }

    pub fn capture(
        &mut self,
        input: RuntimeInputs,
        now: Instant,
        unix_ms: u64,
    ) -> anyhow::Result<RuntimeSnapshot> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("runtime sequence exhausted"))?;
        let mut current = BTreeMap::new();
        let mut historical_labels = BTreeSet::new();
        for source in &input.sources {
            for row in &source.agents {
                let reference = row.agent.reference();
                let mut merged = row.clone();
                if let Some(old) = self.labels.get(&reference) {
                    if merged.title.is_none() && old.title.is_some() {
                        merged.title = old.title.clone();
                        merged.title_at = old.title_at;
                        historical_labels.insert(reference.clone());
                    }
                    if merged.scope_title.is_none() && old.scope_title.is_some() {
                        merged.scope_title = old.scope_title.clone();
                        merged.scope_title_at = old.scope_title_at;
                        historical_labels.insert(reference.clone());
                    }
                }
                self.labels.insert(reference.clone(), merged.clone());
                current.insert(reference, merged);
            }
        }
        while self.labels.len() > 256 {
            let key = self
                .labels
                .iter()
                .min_by_key(|(_, row)| row.observed_at)
                .map(|(key, _)| key.clone())
                .expect("nonempty labels");
            self.labels.remove(&key);
        }
        let source_by_id: BTreeMap<_, _> = input
            .sources
            .iter()
            .map(|source| (source.id.clone(), source))
            .collect();
        let availability = |reference: &AgentRef| {
            if current
                .get(reference)
                .is_some_and(|row| row.available && fresh(now, Some(row.observed_at)))
            {
                Availability::Live
            } else if source_by_id
                .get(&reference.source)
                .is_none_or(|source| source.availability == SourceAvailability::Unknown)
            {
                Availability::Unknown
            } else {
                Availability::Dormant
            }
        };
        let label_info = |reference: &AgentRef, status: Availability| {
            let row = current
                .get(reference)
                .or_else(|| self.labels.get(reference));
            let ages: Vec<_> = row
                .into_iter()
                .flat_map(|row| [row.title_at, row.scope_title_at])
                .flatten()
                .map(|at| age(now, Some(at)).unwrap_or(u64::MAX))
                .collect();
            let labels_age = ages.into_iter().max();
            (
                row.and_then(|row| row.scope_title.clone()),
                row.and_then(|row| row.title.clone()),
                status != Availability::Live
                    || historical_labels.contains(reference)
                    || labels_age.is_some_and(|age| age >= STALE_AFTER_MS)
                    || row.is_some_and(|row| {
                        (row.title.is_some() && row.title_at.is_none())
                            || (row.scope_title.is_some() && row.scope_title_at.is_none())
                    }),
                row.and_then(|row| age(now, Some(row.observed_at))),
                labels_age,
            )
        };
        let legacy_agents = current
            .iter()
            .filter(|(reference, _)| availability(reference) == Availability::Live)
            .map(|(_, row)| row.agent.clone())
            .collect();
        let panes = current
            .iter()
            .map(|(reference, row)| {
                let status = availability(reference);
                let (scope_title, title, labels_stale, age_ms, labels_age_ms) =
                    label_info(reference, status);
                ObservedPane {
                    reference: reference.clone(),
                    scope_title,
                    title,
                    agent_status: row.agent.agent_status.clone(),
                    focused: row.agent.focused && status == Availability::Live,
                    availability: status,
                    labels_stale,
                    age_ms,
                    labels_age_ms,
                }
            })
            .collect();
        let (ledger_available, ledger) = match &input.ledger {
            LedgerRead::Available(state)
                if state.state_version == STATE_VERSION
                    && validate_bindings(&state.bindings).is_ok() =>
            {
                (true, Some(state))
            }
            _ => (false, None),
        };
        let mut keys = input.configuration.bindings.clone();
        if let Some(ledger) = ledger {
            for binding in &ledger.bindings {
                if !keys.contains(&binding.key) {
                    keys.push(binding.key.clone());
                }
            }
        }
        let bindings = keys
            .into_iter()
            .map(|key| {
                let owner = ledger
                    .and_then(|state| state.bindings.iter().find(|binding| binding.key == key))
                    .map(|binding| binding.reference());
                let status = if !ledger_available {
                    Availability::Unknown
                } else {
                    owner.as_ref().map_or(Availability::Free, &availability)
                };
                let (scope_title, title, labels_stale, age_ms, labels_age_ms) = owner
                    .as_ref()
                    .map(|reference| label_info(reference, status))
                    .unwrap_or((None, None, false, None, None));
                BindingLegend {
                    configured: input.configuration.bindings.contains(&key),
                    key,
                    owner,
                    availability: status,
                    scope_title,
                    title,
                    labels_stale,
                    age_ms,
                    labels_age_ms,
                }
            })
            .collect();
        let mut sources = Vec::new();
        let mut source_ids: BTreeSet<_> = input.configuration.sources.iter().cloned().collect();
        source_ids.extend(source_by_id.keys().cloned());
        for id in source_ids {
            let source = source_by_id.get(&id);
            let failure = source.and_then(|source| source.last_failure.as_ref());
            let last_error =
                failure.map(|failure| self.record(failure, None, None, Some(id.clone()), now));
            sources.push(SourceHealth {
                id,
                availability: source
                    .map_or(SourceAvailability::Unknown, |source| source.availability),
                observed_age_ms: source.and_then(|source| age(now, source.observed_at)),
                last_success_age_ms: source.and_then(|source| age(now, source.last_success)),
                last_error,
            });
        }
        let mut devices = Vec::new();
        for device in &input.configuration.devices {
            let observation = input.devices.get(&device.id).cloned().unwrap_or_default();
            let last_error = observation
                .last_failure
                .as_ref()
                .map(|failure| self.record(failure, Some(device.id.clone()), None, None, now));
            let controls = input
                .controls
                .iter()
                .filter(|control| control.device == device.id)
                .map(|control| RuntimeControl {
                    id: control.id.clone(),
                    phase: control.phase,
                    pressed: control.pressed,
                    cleanup_pending: control.cleanup_pending,
                })
                .collect();
            let observed_age_ms = age(now, observation.observed_at);
            devices.push(DeviceHealth {
                id: device.id.clone(),
                adapter: device.definition.adapter.clone(),
                observation_supported: observation.observation_supported,
                connectivity: if observed_age_ms.is_none() {
                    Connectivity::Unknown
                } else {
                    observation.connectivity
                },
                observed_age_ms,
                member_descriptors: observation.member_descriptors,
                layer: observation.layer.and_then(|layer| {
                    age(now, Some(layer.observed_at)).map(|observed_age_ms| ObservedLayer {
                        name: layer.name,
                        profile: layer.profile,
                        position: layer.position,
                        observed_age_ms,
                    })
                }),
                last_input_success_age_ms: age(now, observation.last_input_success),
                last_output_success_age_ms: age(now, observation.last_output_success),
                last_error,
                controls,
            });
        }
        let last_error = input
            .posting
            .last_failure
            .as_ref()
            .map(|failure| self.record(failure, None, None, None, now));
        let posting = PostingHealth {
            permission: input.posting.permission,
            last_checked_age_ms: age(now, input.posting.checked_at),
            last_success_age_ms: age(now, input.posting.last_success),
            last_error,
            cleanup_pending: input.posting.cleanup_pending
                || input.controls.iter().any(|control| control.cleanup_pending),
        };
        if !ledger_available {
            let code = match input.ledger {
                LedgerRead::Unavailable => DiagnosticCode::LedgerUnavailable,
                _ => DiagnosticCode::LedgerInvalid,
            };
            // Observe only a transition, not one fabricated occurrence per UI read.
            if self
                .latest
                .get(&(EventScope::Ledger, None, None, None))
                .is_none_or(|event| event.event.code != code || !event.event.active)
            {
                self.record(
                    &TypedFailure {
                        code,
                        at: now,
                        active: true,
                        recovery: Recovery::CheckLedger,
                    },
                    None,
                    None,
                    None,
                    now,
                );
            }
        } else if let Some(event) = self.latest.get_mut(&(EventScope::Ledger, None, None, None)) {
            event.event.active = false;
            for stored in &mut self.events {
                if stored.event.id == event.event.id {
                    stored.event.active = false;
                }
            }
        }
        let events = self
            .events
            .iter()
            .map(|stored| {
                let mut event = stored.event.clone();
                event.age_ms = age(now, Some(stored.at)).unwrap_or(u64::MAX);
                event
            })
            .collect();
        Ok(RuntimeSnapshot {
            version: 1,
            instance_id: self.instance_id.clone(),
            sequence: self.sequence,
            sampled_at_unix_ms: unix_ms,
            age_ms: 0,
            stale_after_ms: STALE_AFTER_MS,
            build: self.build.clone(),
            configuration_revision: input.configuration.revision.clone(),
            configuration: input.configuration,
            ledger_available,
            posting,
            devices,
            bindings,
            panes,
            sources,
            events,
            captured_at: now,
            legacy_agents,
        })
    }
}
