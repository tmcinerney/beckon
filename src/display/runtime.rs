//! Cached adapter evidence and bounded input interfaces, not a wire protocol.
//!
//! Observations keep their original monotonic timestamps. Reading them must not
//! probe a device or make old evidence fresh; the runtime DTO computes ages.

use std::time::Instant;

use super::DeviceEvent;
use crate::device::Address;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Connectivity {
    #[default]
    Unknown,
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedLayer {
    pub name: Option<String>,
    pub profile: Option<u16>,
    pub position: Option<u16>,
    pub observed_at: Instant,
}

/// A configured association; unlike ObservedLayer this is not hardware proof.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LayerDescriptor {
    pub name: String,
    pub profile: Option<u16>,
    pub position: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MemberDescriptor {
    pub address: Address,
    pub name: Option<String>,
    pub layer: Option<LayerDescriptor>,
}

/// Safe diagnostic vocabulary: never transport error text, argv or stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    DeviceUnavailable,
    InputFailed,
    OutputFailed,
    PostingDenied,
    EventCreationFailed,
    CleanupPending,
    SourceUnavailable,
    SourceSnapshotInvalid,
    LedgerUnavailable,
    LedgerInvalid,
    Recovered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    None,
    ReconnectDevice,
    CheckDeviceConfiguration,
    CheckSource,
    CheckLedger,
    GrantPostingPermission,
    RetryCleanup,
    UpgradeClient,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedFailure {
    pub code: DiagnosticCode,
    pub at: Instant,
    pub active: bool,
    pub recovery: Recovery,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceObservation {
    pub observation_supported: bool,
    pub connectivity: Connectivity,
    pub observed_at: Option<Instant>,
    pub layer: Option<ObservedLayer>,
    pub last_input_success: Option<Instant>,
    pub last_output_success: Option<Instant>,
    pub last_failure: Option<TypedFailure>,
    pub member_descriptors: Vec<MemberDescriptor>,
}

/// Never opens a device, reconnects, issues a status RPC or waits for a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputPollMode {
    DrainOnly,
}

#[derive(Clone, Copy, Debug)]
pub struct InputPollBudget {
    /// An absolute monotonic deadline, shared by the fanout, not per sink.
    pub deadline: Instant,
    /// Counts consumed raw reports, including partial JSON and non-key frames.
    pub max_raw_frames: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputContinuity {
    #[default]
    Unknown,
    Continuous,
    /// A new selector can remain observed despite this gesture-reset boundary.
    Reset,
    Lost,
}

#[derive(Clone, Debug, Default)]
pub struct InputBatch {
    /// Original wire order, including resets interleaved with keys/releases.
    pub events: Vec<DeviceEvent>,
    pub raw_frames: usize,
    /// True ONLY when the adapter actually observed empty/would-block input.
    /// This alone is NOT a timing token; actor admission requires claim/consume.
    pub drained: bool,
    /// Incomplete JSON/report remains buffered, even if no key event decoded.
    pub partial_frame: bool,
    pub continuity: InputContinuity,
    pub transport_epoch: Option<TransportEpoch>,
    /// All events through this frontier delivered in order, not receipt-time proof.
    pub event_frontier: Option<u64>,
}

impl InputBatch {
    /// Legacy drain validation only; does not grant an actor capture lease.
    pub fn is_trusted_drain(&self, budget: InputPollBudget, now: Instant) -> bool {
        self.drained
            && !self.partial_frame
            && self.continuity == InputContinuity::Continuous
            && !self
                .events
                .iter()
                .any(|event| matches!(event, DeviceEvent::Reset { .. }))
            && self.raw_frames < budget.max_raw_frames
            && now < budget.deadline
    }
}

/// Process-monotonic owner identity and its connection/selector generations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportEpoch {
    pub actor: u64,
    pub connection: u64,
    pub selector: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureId {
    pub actor: u64,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureRequest {
    pub id: CaptureId,
    pub epoch: TransportEpoch,
    pub requested_at: Instant,
    pub not_before: Instant,
    pub expires_at: Instant,
}

/// Original actor observations, never freshened by receipt or forwarding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureEvidence {
    pub request: CaptureRequest,
    pub command_revision: u64,
    pub ingress_frontier: u64,
    pub event_frontier: u64,
    pub started_at: Instant,
    pub observed_at: Instant,
    pub completed_at: Instant,
    pub lease_until: Instant,
    pub raw_frames: usize,
    pub partial_frame: bool,
    pub backlog: bool,
    pub continuity: InputContinuity,
}

/// Only the owning actor can validate this token and fence single-use consumption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureToken {
    pub evidence: CaptureEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportStart {
    Unsupported,
    Started,
    Running,
    Retiring,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureReject {
    Unsupported,
    NotStarted,
    Busy,
    Stale,
    Expired,
    Faulted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureRequestOutcome {
    Pending,
    Rejected(CaptureReject),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureClaim {
    Claimed(CaptureToken),
    Rejected(CaptureReject),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureConsume {
    Consumed,
    Rejected(CaptureReject),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectOutcome {
    /// Unsupported, unsafe or waiting for a later explicitly idle turn.
    Deferred,
    /// Incremental handshake is incomplete; a later bounded step may continue.
    InProgress,
    /// A real transport/status observation established the connection.
    Connected,
}

#[derive(Clone, Debug)]
pub struct DeviceInputBatch {
    pub device: String,
    pub batch: InputBatch,
    /// Host completion time, NOT a physical input timestamp.
    pub completed_at: Instant,
    pub failure: Option<TypedFailure>,
}
