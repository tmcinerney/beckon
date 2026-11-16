//! Hardware-neutral display fan-out and output adapters.

mod process;
mod runtime;

#[cfg(test)]
mod transport_tests;

pub use runtime::{
    CaptureClaim, CaptureConsume, CaptureEvidence, CaptureId, CaptureReject, CaptureRequest,
    CaptureRequestOutcome, CaptureToken, Connectivity, DeviceInputBatch, DeviceObservation,
    DiagnosticCode, InputBatch, InputContinuity, InputPollBudget, InputPollMode, LayerDescriptor,
    MemberDescriptor, ObservedLayer, ReconnectOutcome, Recovery, TransportEpoch, TransportStart,
    TypedFailure,
};

use anyhow::Result;
use std::collections::BTreeMap;

use crate::{
    config::Config,
    device::{Address, Treatment},
    hid::{self, StatusWriter, UsbStatusWriter},
    render::RenderPlan,
};

use process::ProcessDisplay;

/// Adapter registry boundary. Generic settings carry named channels but never
/// interpret a device's zones or rewrite its transport options themselves.
pub fn mutable_channels(device: &crate::device::DeviceConfig) -> &'static [&'static str] {
    match device.definition.adapter.as_str() {
        "creator-micro" => crate::creator_micro::mutable_channels(),
        _ => &[],
    }
}

pub fn apply_channels(
    device: &mut crate::device::DeviceConfig,
    channels: &BTreeMap<String, Treatment>,
) -> Result<()> {
    match device.definition.adapter.as_str() {
        "creator-micro" => crate::creator_micro::apply_channels(device, channels),
        _ => anyhow::bail!(
            "device {:?} exposes no mutable appearance channels",
            device.id
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    Key { address: Address, pressed: bool },
    Reset { device: String },
}

/// Result of offering the current render plan to one display integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The adapter sent a new state to its device or service.
    Updated,
    /// Nothing was sent because the state is unchanged or retry is deferred.
    Unchanged,
    /// The optional target is not currently attached or available.
    Unavailable,
}

/// One independently managed consumer of Beckon's hardware-neutral state.
pub trait DisplaySink {
    /// Explicit nonwaiting lifecycle entry; defaults never invoke legacy I/O.
    fn start_transport(&mut self) -> Result<TransportStart> {
        Ok(TransportStart::Unsupported)
    }
    fn transport_epoch(&self) -> Option<TransportEpoch> {
        None
    }
    fn request_capture(&mut self, _request: CaptureRequest) -> CaptureRequestOutcome {
        CaptureRequestOutcome::Rejected(CaptureReject::Unsupported)
    }
    fn capture_evidence(&self, _id: CaptureId) -> Option<CaptureEvidence> {
        None
    }
    fn try_claim_capture(
        &mut self,
        _evidence: CaptureEvidence,
        _now: std::time::Instant,
    ) -> CaptureClaim {
        CaptureClaim::Rejected(CaptureReject::Unsupported)
    }
    fn consume_capture(
        &mut self,
        _token: CaptureToken,
        _applied_through: u64,
        _now: std::time::Instant,
    ) -> CaptureConsume {
        CaptureConsume::Rejected(CaptureReject::Unsupported)
    }
    fn release_capture(&mut self, _token: CaptureToken) {}
    fn stop_transport(&mut self) {}
    /// Cached evidence only: no I/O, probing or timestamp refresh on a read.
    fn diagnostics(&self) -> DeviceObservation {
        DeviceObservation::default()
    }
    fn refresh_presentation(&mut self, _device: &crate::device::DeviceConfig) -> Result<()> {
        Ok(())
    }
    fn id(&self) -> &str;
    fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome>;
    /// Physical key events observed by a sink that owns a device. Write-only
    /// sinks return an empty list; the daemon maps AG ids onto logical keys.
    fn poll_input(&mut self) -> Result<Vec<DeviceEvent>> {
        Ok(Vec::new())
    }
    /// Bounded input seam. The compatibility default is deliberately unknown:
    /// it must NOT delegate to possibly blocking legacy poll_input().
    fn poll_input_batch(
        &mut self,
        _mode: InputPollMode,
        _budget: InputPollBudget,
    ) -> Result<InputBatch> {
        Ok(InputBatch::default())
    }
    /// One nonblocking handshake step, only scheduled by an explicitly idle
    /// caller. Unsupported adapters stay deferred, never fall back to legacy I/O.
    fn reconnect_step(&mut self, _deadline: std::time::Instant) -> Result<ReconnectOutcome> {
        Ok(ReconnectOutcome::Deferred)
    }
    fn publish_feedback(
        &mut self,
        plan: &RenderPlan,
        _feedback: &BTreeMap<Address, Treatment>,
    ) -> Result<PublishOutcome> {
        self.publish(plan)
    }
}

/// The Glove80's optional wired USB status display.
pub struct Glove80UsbDisplay {
    worker: hid::StatusPublicationWorker,
    bindings: Option<Vec<String>>,
    id: String,
}

impl Default for Glove80UsbDisplay {
    fn default() -> Self {
        Self {
            worker: hid::StatusPublicationWorker::new(UsbStatusWriter::default),
            bindings: None,
            id: "glove80".into(),
        }
    }
}

impl Glove80UsbDisplay {
    pub fn new<W: StatusWriter + Send + 'static>(writer: W) -> Self {
        Self {
            worker: {
                #[cfg(test)]
                {
                    hid::StatusPublicationWorker::isolated(move || writer)
                }
                #[cfg(not(test))]
                {
                    hid::StatusPublicationWorker::new(move || writer)
                }
            },
            bindings: None,
            id: "glove80".into(),
        }
    }
}

impl DisplaySink for Glove80UsbDisplay {
    fn start_transport(&mut self) -> Result<TransportStart> {
        self.worker.start()
    }
    fn stop_transport(&mut self) {
        self.worker.stop();
    }
    fn diagnostics(&self) -> DeviceObservation {
        self.worker.diagnostics()
    }
    fn id(&self) -> &str {
        &self.id
    }

    fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome> {
        let projected;
        let plan = if let Some(bindings) = &self.bindings {
            projected = RenderPlan {
                keys: bindings
                    .iter()
                    .enumerate()
                    .map(|(index, binding)| {
                        let mut render = plan
                            .keys
                            .iter()
                            .find(|render| &render.key == binding)
                            .cloned()
                            .ok_or_else(|| {
                                anyhow::anyhow!("Glove80 binding {binding:?} missing")
                            })?;
                        render.key = format!("f{}", index + 1);
                        Ok(render)
                    })
                    .collect::<Result<Vec<_>>>()?,
                treatments: plan.treatments.clone(),
            };
            &projected
        } else {
            plan
        };
        self.worker.publish(plan)
    }
}

/// A newly reportable failure from one display integration.
#[derive(Debug, PartialEq, Eq)]
pub struct DisplayFailure {
    pub adapter: String,
    pub message: String,
}

struct ManagedSink {
    sink: Box<dyn DisplaySink>,
    last_error: Option<String>,
    last_input_error: Option<String>,
}

/// Fans one render plan out to zero or more independent display integrations.
///
/// Adapter absence and failures never block input or pane navigation. Errors
/// are de-duplicated per adapter and retried by the adapter on later ticks.
#[derive(Default)]
pub struct DisplaySet {
    sinks: Vec<ManagedSink>,
    next_input: usize,
}

impl DisplaySet {
    /// Called once by the explicit daemon startup, never by polling or diagnostics.
    pub fn start_transports(&mut self) -> Vec<(String, Result<TransportStart>)> {
        self.sinks
            .iter_mut()
            .map(|managed| (managed.sink.id().to_owned(), managed.sink.start_transport()))
            .collect()
    }

    /// Signal retirement without joining; the transport owner handles teardown.
    pub fn stop_transports(&mut self) {
        for managed in &mut self.sinks {
            managed.sink.stop_transport();
        }
    }

    /// Device-scoped nonwaiting dispatch only. No legacy polling fallback.
    pub fn transport_epoch(&self, device: &str) -> Option<TransportEpoch> {
        self.sinks
            .iter()
            .find(|managed| managed.sink.id() == device)
            .and_then(|managed| managed.sink.transport_epoch())
    }

    pub fn request_capture(
        &mut self,
        device: &str,
        request: CaptureRequest,
    ) -> CaptureRequestOutcome {
        self.sinks
            .iter_mut()
            .find(|managed| managed.sink.id() == device)
            .map_or(
                CaptureRequestOutcome::Rejected(CaptureReject::NotStarted),
                |managed| managed.sink.request_capture(request),
            )
    }

    pub fn capture_evidence(&self, device: &str, id: CaptureId) -> Option<CaptureEvidence> {
        self.sinks
            .iter()
            .find(|managed| managed.sink.id() == device)
            .and_then(|managed| managed.sink.capture_evidence(id))
    }

    pub fn try_claim_capture(
        &mut self,
        device: &str,
        evidence: CaptureEvidence,
        now: std::time::Instant,
    ) -> CaptureClaim {
        self.sinks
            .iter_mut()
            .find(|managed| managed.sink.id() == device)
            .map_or(
                CaptureClaim::Rejected(CaptureReject::NotStarted),
                |managed| managed.sink.try_claim_capture(evidence, now),
            )
    }

    pub fn consume_capture(
        &mut self,
        device: &str,
        token: CaptureToken,
        applied_through: u64,
        now: std::time::Instant,
    ) -> CaptureConsume {
        self.sinks
            .iter_mut()
            .find(|managed| managed.sink.id() == device)
            .map_or(
                CaptureConsume::Rejected(CaptureReject::NotStarted),
                |managed| managed.sink.consume_capture(token, applied_through, now),
            )
    }

    pub fn release_capture(&mut self, device: &str, token: CaptureToken) {
        if let Some(managed) = self
            .sinks
            .iter_mut()
            .find(|managed| managed.sink.id() == device)
        {
            managed.sink.release_capture(token);
        }
    }

    /// One ordered bounded-input batch per sink; never calls legacy poll_input.
    /// Missing/unknown batches must not be treated as exhausted input.
    pub fn poll_input_batches<F>(
        &mut self,
        mode: InputPollMode,
        budget: InputPollBudget,
        mut now: F,
    ) -> Vec<DeviceInputBatch>
    where
        F: FnMut() -> std::time::Instant,
    {
        let mut found = Vec::new();
        if budget.max_raw_frames == 0 {
            return found;
        }
        let sink_count = self.sinks.len();
        for _ in 0..sink_count {
            if now() >= budget.deadline {
                break;
            }
            let managed = &mut self.sinks[self.next_input];
            self.next_input = (self.next_input + 1) % sink_count;
            let device = managed.sink.id().to_owned();
            let result = managed.sink.poll_input_batch(mode, budget);
            let completed_at = now();
            let (batch, failure) = match result {
                Ok(batch) => (batch, None),
                Err(_) => (
                    InputBatch {
                        events: vec![DeviceEvent::Reset {
                            device: device.clone(),
                        }],
                        continuity: InputContinuity::Lost,
                        ..Default::default()
                    },
                    Some(TypedFailure {
                        code: DiagnosticCode::InputFailed,
                        at: completed_at,
                        active: true,
                        recovery: Recovery::ReconnectDevice,
                    }),
                ),
            };
            found.push(DeviceInputBatch {
                device,
                batch,
                completed_at,
                failure,
            });
        }
        found
    }

    /// Read each sink's cached evidence without input polling or output writes.
    /// Timestamps remain unchanged; the runtime caller converts them to ages.
    pub fn diagnostics(&self) -> BTreeMap<String, DeviceObservation> {
        self.sinks
            .iter()
            .map(|managed| (managed.sink.id().to_owned(), managed.sink.diagnostics()))
            .collect()
    }

    pub fn refresh_presentation(&mut self, devices: &[crate::device::DeviceConfig]) -> Result<()> {
        for managed in &mut self.sinks {
            if let Some(device) = devices.iter().find(|device| device.id == managed.sink.id()) {
                managed.sink.refresh_presentation(device)?;
            }
        }
        Ok(())
    }
    pub fn from_config(config: &Config) -> Result<Self> {
        config.validate()?;
        if config
            .devices
            .iter()
            .filter(|device| device.definition.adapter == "glove80")
            .count()
            > 1
        {
            anyhow::bail!(
                "only one status publication owner is supported; select a single Glove80 device"
            );
        }
        let mut sinks = Vec::new();
        for device in &config.devices {
            let sink: Box<dyn DisplaySink> = match device.definition.adapter.as_str() {
                "creator-micro" => Box::new(crate::creator_micro::CreatorMicroDisplay::new(
                    device.clone(),
                )?),
                "glove80" => {
                    let options: crate::input::GloveOptions =
                        toml::Value::Table(device.definition.options.clone()).try_into()?;
                    let display = Glove80UsbDisplay {
                        id: device.id.clone(),
                        bindings: Some(options.status_bindings),
                        ..Glove80UsbDisplay::default()
                    };
                    Box::new(display)
                }
                _ => continue,
            };
            sinks.push(ManagedSink {
                sink,
                last_error: None,
                last_input_error: None,
            });
        }
        for plugin in &config.outputs.plugins {
            sinks.push(ManagedSink {
                sink: Box::new(ProcessDisplay::new(plugin.clone())?),
                last_error: None,
                last_input_error: None,
            });
        }
        Ok(Self {
            sinks,
            next_input: 0,
        })
    }

    /// Assemble inert owned integrations, including injected transport factories.
    /// Starting transports is a separate explicit daemon lifecycle operation.
    pub fn new(sinks: Vec<Box<dyn DisplaySink>>) -> Self {
        Self {
            next_input: 0,
            sinks: sinks
                .into_iter()
                .map(|sink| ManagedSink {
                    sink,
                    last_error: None,
                    last_input_error: None,
                })
                .collect(),
        }
    }

    pub fn publish(&mut self, plan: &RenderPlan) -> Vec<DisplayFailure> {
        self.publish_feedback(plan, &BTreeMap::new())
    }

    pub fn publish_feedback(
        &mut self,
        plan: &RenderPlan,
        feedback: &BTreeMap<Address, Treatment>,
    ) -> Vec<DisplayFailure> {
        let mut failures = Vec::new();
        for managed in &mut self.sinks {
            match managed.sink.publish_feedback(plan, feedback) {
                Ok(PublishOutcome::Updated) => managed.last_error = None,
                Ok(PublishOutcome::Unchanged | PublishOutcome::Unavailable) => {}
                Err(error) => {
                    let message = format!("{error:#}");
                    if managed.last_error.as_deref() != Some(message.as_str()) {
                        failures.push(DisplayFailure {
                            adapter: managed.sink.id().to_string(),
                            message: message.clone(),
                        });
                        managed.last_error = Some(message);
                    }
                }
            }
        }
        failures
    }

    /// Collect physical key events from every sink that can produce them.
    /// Errors are de-duplicated per adapter, matching `publish`, so a
    /// disconnected pad does not flood the daemon log on every tick.
    pub fn poll_input(&mut self) -> (Vec<DeviceEvent>, Vec<DisplayFailure>) {
        let mut events = Vec::new();
        let mut failures = Vec::new();
        for managed in &mut self.sinks {
            match managed.sink.poll_input() {
                Ok(mut found) => {
                    managed.last_input_error = None;
                    events.append(&mut found);
                }
                Err(error) => {
                    events.push(DeviceEvent::Reset {
                        device: managed.sink.id().to_string(),
                    });
                    let message = format!("{error:#}");
                    if managed.last_input_error.as_deref() != Some(message.as_str()) {
                        failures.push(DisplayFailure {
                            adapter: managed.sink.id().to_string(),
                            message: message.clone(),
                        });
                        managed.last_input_error = Some(message);
                    }
                }
            }
        }
        (events, failures)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    use super::*;
    use crate::render::DisplayConfig;

    #[test]
    fn daemon_turn_glove80_publication_contention_keeps_latest_projection_and_real_completion() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};
        struct BlockedWriter {
            gate: mpsc::Receiver<()>,
            entered: mpsc::SyncSender<()>,
            writes: Arc<Mutex<Vec<hid::StatusSnapshot>>>,
        }
        impl StatusWriter for BlockedWriter {
            fn write_snapshot(&mut self, snapshot: hid::StatusSnapshot) -> Result<()> {
                if snapshot.sequence == 0 {
                    self.entered.send(()).unwrap();
                    let _ = self.gate.recv();
                }
                self.writes.lock().unwrap().push(snapshot);
                Ok(())
            }
        }
        let (release, gate) = mpsc::sync_channel(1);
        let (entered, observed) = mpsc::sync_channel(1);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut display = Glove80UsbDisplay::new(BlockedWriter {
            gate,
            entered,
            writes: writes.clone(),
        });
        assert_eq!(display.start_transport().unwrap(), TransportStart::Started);
        display.bindings = Some((1..=10).map(|i| format!("key{i}")).collect());
        let mut desired = plan();
        for (i, key) in desired.keys.iter_mut().enumerate() {
            key.key = format!("key{}", i + 1);
        }
        assert_eq!(
            display.publish(&desired).unwrap(),
            PublishOutcome::Unchanged
        );
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut displays = DisplaySet::new(vec![Box::new(display)]);
        desired.keys[0].state = Some(crate::render::AgentState::Working);
        for _ in 0..100 {
            assert!(displays.publish(&desired).is_empty());
        }
        desired.keys[0].state = Some(crate::render::AgentState::Done);
        displays.publish(&desired);
        assert!(
            displays.diagnostics()["glove80"]
                .last_output_success
                .is_none()
        );
        assert!(writes.lock().unwrap().is_empty());
        release.send(()).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(displays.publish(&desired).is_empty());
            if writes.lock().unwrap().len() == 2
                && displays.diagnostics()["glove80"]
                    .last_output_success
                    .is_some()
            {
                break;
            }
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        assert_eq!(writes.lock().unwrap()[1].slots[0], hid::Status::Done);
        assert_eq!(writes.lock().unwrap()[1].sequence, 1);
        assert!(
            displays.diagnostics()["glove80"]
                .last_output_success
                .is_some()
        );
        displays.stop_transports();
    }

    struct LegacyInputSink;

    impl DisplaySink for LegacyInputSink {
        fn id(&self) -> &str {
            "legacy-pad"
        }

        fn publish(&mut self, _: &RenderPlan) -> Result<PublishOutcome> {
            panic!("diagnostics must not publish")
        }

        fn poll_input(&mut self) -> Result<Vec<DeviceEvent>> {
            panic!("diagnostics must not poll legacy input")
        }
    }

    #[test]
    fn foundation_default_observation_is_unknown_without_io() {
        let sink: Box<dyn DisplaySink> = Box::new(LegacyInputSink);
        let observation = sink.diagnostics();
        assert_eq!(observation.connectivity, Connectivity::Unknown);
        assert!(!observation.observation_supported);
        assert!(observation.observed_at.is_none());
        assert!(observation.layer.is_none());
        assert!(observation.last_input_success.is_none());
        assert!(observation.last_output_success.is_none());
        assert!(observation.last_failure.is_none());
        assert!(observation.member_descriptors.is_empty());
    }

    #[test]
    fn foundation_diagnostics_fanout_preserves_device_scope_and_evidence_time() {
        struct ObservedSink(DeviceObservation);
        impl DisplaySink for ObservedSink {
            fn id(&self) -> &str {
                "another-pad"
            }
            fn publish(&mut self, _: &RenderPlan) -> Result<PublishOutcome> {
                panic!("diagnostics must not publish")
            }
            fn diagnostics(&self) -> DeviceObservation {
                self.0.clone()
            }
        }
        let at = std::time::Instant::now();
        let observation = DeviceObservation {
            observation_supported: true,
            connectivity: Connectivity::Connected,
            observed_at: Some(at),
            layer: Some(ObservedLayer {
                name: None,
                profile: Some(7),
                position: Some(29),
                observed_at: at,
            }),
            ..Default::default()
        };
        let displays = DisplaySet::new(vec![
            Box::new(LegacyInputSink),
            Box::new(ObservedSink(observation.clone())),
        ]);
        let first = displays.diagnostics();
        assert_eq!(first["legacy-pad"], DeviceObservation::default());
        assert_eq!(first["another-pad"], observation);
        assert_eq!(displays.diagnostics(), first);
    }

    #[test]
    fn foundation_drain_and_reconnect_defaults_never_call_legacy_io() {
        let mut sink: Box<dyn DisplaySink> = Box::new(LegacyInputSink);
        let now = std::time::Instant::now();
        let budget = InputPollBudget {
            deadline: now + std::time::Duration::from_millis(5),
            max_raw_frames: 128,
        };
        let batch = sink
            .poll_input_batch(InputPollMode::DrainOnly, budget)
            .unwrap();
        assert!(batch.events.is_empty());
        assert!(!batch.drained);
        assert_eq!(batch.continuity, InputContinuity::Unknown);
        assert!(!batch.is_trusted_drain(budget, now));
        assert_eq!(
            sink.reconnect_step(budget.deadline).unwrap(),
            ReconnectOutcome::Deferred
        );
    }

    #[test]
    fn foundation_partial_json_is_not_exhausted_even_without_decoded_events() {
        let now = std::time::Instant::now();
        let budget = InputPollBudget {
            deadline: now + std::time::Duration::from_millis(5),
            max_raw_frames: 128,
        };
        let batch = InputBatch {
            events: Vec::new(),
            raw_frames: 1,
            drained: true,
            partial_frame: true,
            continuity: InputContinuity::Continuous,
            ..Default::default()
        };
        assert!(!batch.is_trusted_drain(budget, now));
    }

    #[test]
    fn foundation_exhausted_raw_or_time_budget_never_authorizes_input() {
        let now = std::time::Instant::now();
        let deadline = now + std::time::Duration::from_millis(5);
        let budget = InputPollBudget {
            deadline,
            max_raw_frames: 128,
        };
        let clean = InputBatch {
            drained: true,
            continuity: InputContinuity::Continuous,
            ..Default::default()
        };
        assert!(clean.is_trusted_drain(budget, now));
        for raw_frames in [128, 129] {
            let batch = InputBatch {
                raw_frames,
                ..clean.clone()
            };
            assert!(!batch.is_trusted_drain(budget, now));
        }
        assert!(!clean.is_trusted_drain(budget, deadline));
        assert!(!clean.is_trusted_drain(
            InputPollBudget {
                max_raw_frames: 0,
                ..budget
            },
            now
        ));
    }

    #[test]
    fn foundation_reset_or_loss_is_a_continuity_boundary_even_when_transport_is_empty() {
        let now = std::time::Instant::now();
        let budget = InputPollBudget {
            deadline: now + std::time::Duration::from_millis(5),
            max_raw_frames: 128,
        };
        let reset = InputBatch {
            events: vec![DeviceEvent::Reset {
                device: "pad".into(),
            }],
            drained: true,
            continuity: InputContinuity::Continuous,
            ..Default::default()
        };
        assert!(!reset.is_trusted_drain(budget, now));
        for continuity in [
            InputContinuity::Unknown,
            InputContinuity::Reset,
            InputContinuity::Lost,
        ] {
            let batch = InputBatch {
                events: Vec::new(),
                continuity,
                ..reset.clone()
            };
            assert!(!batch.is_trusted_drain(budget, now));
        }
    }

    #[test]
    fn foundation_bounded_input_fanout_preserves_order_and_isolates_sanitized_failure() {
        struct BatchSink {
            id: &'static str,
            result: Option<Result<InputBatch>>,
        }
        impl DisplaySink for BatchSink {
            fn id(&self) -> &str {
                self.id
            }
            fn publish(&mut self, _: &RenderPlan) -> Result<PublishOutcome> {
                panic!("input must not publish")
            }
            fn poll_input(&mut self) -> Result<Vec<DeviceEvent>> {
                panic!("bounded input must not fall back to legacy input")
            }
            fn poll_input_batch(
                &mut self,
                _: InputPollMode,
                _: InputPollBudget,
            ) -> Result<InputBatch> {
                self.result
                    .take()
                    .expect("only one batch per sink per call")
            }
        }
        let now = std::time::Instant::now();
        let budget = InputPollBudget {
            deadline: now + std::time::Duration::from_millis(5),
            max_raw_frames: 128,
        };
        let events = vec![
            DeviceEvent::Key {
                address: "pad:enter".to_string().try_into().unwrap(),
                pressed: true,
            },
            DeviceEvent::Reset {
                device: "pad".into(),
            },
            DeviceEvent::Key {
                address: "pad:enter".to_string().try_into().unwrap(),
                pressed: false,
            },
        ];
        let mut displays = DisplaySet::new(vec![
            Box::new(BatchSink {
                id: "pad",
                result: Some(Ok(InputBatch {
                    events: events.clone(),
                    raw_frames: 3,
                    drained: true,
                    continuity: InputContinuity::Reset,
                    ..Default::default()
                })),
            }),
            Box::new(BatchSink {
                id: "failed-pad",
                result: Some(Err(anyhow::anyhow!("private raw transport details"))),
            }),
            Box::new(LegacyInputSink),
        ]);
        let found = displays.poll_input_batches(InputPollMode::DrainOnly, budget, || now);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].device, "pad");
        assert_eq!(found[0].batch.events, events);
        assert!(!found[0].batch.is_trusted_drain(budget, now));
        assert_eq!(found[1].device, "failed-pad");
        assert_eq!(found[1].batch.continuity, InputContinuity::Lost);
        assert_eq!(
            found[1].batch.events,
            vec![DeviceEvent::Reset {
                device: "failed-pad".into()
            }]
        );
        assert_eq!(
            found[1].failure.as_ref().unwrap().code,
            DiagnosticCode::InputFailed
        );
        assert!(!format!("{:?}", found[1]).contains("private raw transport details"));
        assert_eq!(found[2].batch.continuity, InputContinuity::Unknown);
        assert!(!found[2].batch.is_trusted_drain(budget, now));
    }

    #[test]
    fn foundation_input_fanout_deadline_yields_and_resumes_at_next_sink() {
        use std::{
            cell::Cell,
            time::{Duration, Instant},
        };
        struct SlowSink {
            id: &'static str,
            clock: Rc<Cell<Instant>>,
        }
        impl DisplaySink for SlowSink {
            fn id(&self) -> &str {
                self.id
            }
            fn publish(&mut self, _: &RenderPlan) -> Result<PublishOutcome> {
                unreachable!()
            }
            fn poll_input_batch(
                &mut self,
                _: InputPollMode,
                _: InputPollBudget,
            ) -> Result<InputBatch> {
                self.clock.set(self.clock.get() + Duration::from_millis(5));
                Ok(InputBatch {
                    drained: true,
                    continuity: InputContinuity::Continuous,
                    ..Default::default()
                })
            }
        }
        let start = Instant::now();
        let clock = Rc::new(Cell::new(start));
        let mut displays = DisplaySet::new(vec![
            Box::new(SlowSink {
                id: "first",
                clock: clock.clone(),
            }),
            Box::new(SlowSink {
                id: "second",
                clock: clock.clone(),
            }),
        ]);
        for expected in ["first", "second"] {
            let budget = InputPollBudget {
                deadline: clock.get() + Duration::from_millis(5),
                max_raw_frames: 128,
            };
            let found =
                displays.poll_input_batches(InputPollMode::DrainOnly, budget, || clock.get());
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].device, expected);
            assert!(
                !found[0]
                    .batch
                    .is_trusted_drain(budget, found[0].completed_at)
            );
        }
        let expired = InputPollBudget {
            deadline: clock.get(),
            max_raw_frames: 128,
        };
        assert!(
            displays
                .poll_input_batches(InputPollMode::DrainOnly, expired, || clock.get())
                .is_empty()
        );
        let zero = InputPollBudget {
            deadline: clock.get() + Duration::from_millis(5),
            max_raw_frames: 0,
        };
        assert!(
            displays
                .poll_input_batches(InputPollMode::DrainOnly, zero, || clock.get())
                .is_empty()
        );
    }

    struct FakeSink {
        id: &'static str,
        outcomes: VecDeque<Result<PublishOutcome>>,
        calls: Rc<RefCell<usize>>,
    }

    impl DisplaySink for FakeSink {
        fn id(&self) -> &str {
            self.id
        }

        fn publish(&mut self, _plan: &RenderPlan) -> Result<PublishOutcome> {
            *self.calls.borrow_mut() += 1;
            self.outcomes
                .pop_front()
                .unwrap_or(Ok(PublishOutcome::Unchanged))
        }
    }

    fn plan() -> RenderPlan {
        crate::render::render(&DisplayConfig::default(), &[]).unwrap()
    }

    #[test]
    fn empty_display_set_is_valid() {
        assert!(DisplaySet::default().publish(&plan()).is_empty());
    }

    #[test]
    fn unavailable_sink_is_an_expected_condition() {
        let calls = Rc::new(RefCell::new(0));
        let sink = FakeSink {
            id: "optional",
            outcomes: [Ok(PublishOutcome::Unavailable)].into(),
            calls: calls.clone(),
        };
        let mut displays = DisplaySet::new(vec![Box::new(sink)]);

        assert!(displays.publish(&plan()).is_empty());
        assert_eq!(*calls.borrow(), 1);
    }

    struct UnavailableWriter;

    impl StatusWriter for UnavailableWriter {
        fn write_snapshot(&mut self, _snapshot: hid::StatusSnapshot) -> Result<()> {
            Err(hid::StatusEndpointUnavailable.into())
        }
    }

    #[test]
    fn glove80_adapter_maps_a_missing_endpoint_to_unavailable() {
        let mut display = Glove80UsbDisplay::new(UnavailableWriter);
        display.start_transport().unwrap();
        assert_eq!(display.publish(&plan()).unwrap(), PublishOutcome::Unchanged);
        let limit = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while display.diagnostics().connectivity != Connectivity::Disconnected {
            assert!(std::time::Instant::now() < limit);
            std::thread::yield_now();
        }
        assert_eq!(
            display.publish(&plan()).unwrap(),
            PublishOutcome::Unavailable
        );
    }

    #[test]
    fn isolates_and_deduplicates_sink_failures() {
        let failed_calls = Rc::new(RefCell::new(0));
        let healthy_calls = Rc::new(RefCell::new(0));
        let failed = FakeSink {
            id: "failed",
            outcomes: [
                Err(anyhow::anyhow!("transport failed")),
                Err(anyhow::anyhow!("transport failed")),
                Ok(PublishOutcome::Updated),
                Err(anyhow::anyhow!("transport failed")),
            ]
            .into(),
            calls: failed_calls.clone(),
        };
        let healthy = FakeSink {
            id: "healthy",
            outcomes: [
                Ok(PublishOutcome::Updated),
                Ok(PublishOutcome::Updated),
                Ok(PublishOutcome::Updated),
                Ok(PublishOutcome::Updated),
            ]
            .into(),
            calls: healthy_calls.clone(),
        };
        let mut displays = DisplaySet::new(vec![Box::new(failed), Box::new(healthy)]);

        assert_eq!(displays.publish(&plan()).len(), 1);
        assert!(displays.publish(&plan()).is_empty());
        assert!(displays.publish(&plan()).is_empty());
        assert_eq!(displays.publish(&plan()).len(), 1);
        assert_eq!(*failed_calls.borrow(), 4);
        assert_eq!(*healthy_calls.borrow(), 4);
    }

    #[test]
    fn unchanged_does_not_claim_recovery_from_an_error() {
        let calls = Rc::new(RefCell::new(0));
        let sink = FakeSink {
            id: "backing-off",
            outcomes: [
                Err(anyhow::anyhow!("transport failed")),
                Ok(PublishOutcome::Unchanged),
                Err(anyhow::anyhow!("transport failed")),
            ]
            .into(),
            calls,
        };
        let mut displays = DisplaySet::new(vec![Box::new(sink)]);

        assert_eq!(displays.publish(&plan()).len(), 1);
        assert!(displays.publish(&plan()).is_empty());
        assert!(displays.publish(&plan()).is_empty());
    }
}
