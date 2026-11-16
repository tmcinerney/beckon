//! Exclusive transport ownership. No core, posting or configuration mutation.

use super::*;
use crate::display::{
    CaptureClaim, CaptureConsume, CaptureEvidence, CaptureId, CaptureReject, CaptureRequest,
    CaptureRequestOutcome, CaptureToken, TransportEpoch,
};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64},
};

pub(super) const EVENT_CAP: usize = 256;
pub(super) const ACC_CAP: usize = 65536;
const RETRY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
enum CapturePhase {
    Pending(CaptureRequest),
    Ready(CaptureEvidence),
    Claimed(CaptureEvidence),
    Consumed(CaptureEvidence),
}
impl CapturePhase {
    fn evidence(self) -> Option<CaptureEvidence> {
        match self {
            Self::Pending(_) => None,
            Self::Ready(e) | Self::Claimed(e) | Self::Consumed(e) => Some(e),
        }
    }
}

pub(super) struct Record {
    pub epoch: TransportEpoch,
    pub connection_lost: bool,
    seq: u64,
    pub frame: Option<TimedFrame>,
}

struct Shared {
    epoch: TransportEpoch,
    observation: DeviceObservation,
    events: VecDeque<Record>,
    frontier: u64,
    delivered: u64,
    desired: Option<LightingFrame>,
    desired_epoch: Option<TransportEpoch>,
    revision: u64,
    completed_revision: u64,
    completion_notice: bool,
    clear_bits: u16,
    clear_speed: f32,
    uncertain_clears: u16,
    overflow: Option<Record>,
    held: u16,
    quarantined: u16,
    capture: Option<CapturePhase>,
    request_high: u64,
    ingress: u64,
    last_empty: Option<Instant>,
    partial: bool,
    continuity: InputContinuity,
}

pub(super) struct Delivery {
    pub records: Vec<Record>,
    pub frontier: Option<u64>,
    pub fault: bool,
    pub partial: bool,
    pub continuity: InputContinuity,
}

#[derive(Clone)]
pub(super) struct Endpoint {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

impl Endpoint {
    fn counter_fault(&self, s: &mut Shared, now: Instant) {
        self.stop();
        s.capture = None;
        s.observation.connectivity = Connectivity::Disconnected;
        s.observation.layer = None;
        s.observation.last_failure = Some(TypedFailure {
            code: DiagnosticCode::InputFailed,
            at: now,
            active: true,
            recovery: Recovery::ReconnectDevice,
        });
        if s.overflow.is_none() {
            s.overflow = Some(Record {
                connection_lost: true,
                epoch: s.epoch,
                seq: s.frontier,
                frame: None,
            });
        }
    }
    fn new(actor: u64) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                epoch: TransportEpoch {
                    actor,
                    connection: 0,
                    selector: 0,
                },
                observation: DeviceObservation {
                    observation_supported: true,
                    ..Default::default()
                },
                events: VecDeque::new(),
                frontier: 0,
                delivered: 0,
                desired: None,
                desired_epoch: None,
                revision: 0,
                completed_revision: 0,
                completion_notice: false,
                clear_bits: 0,
                uncertain_clears: 0,
                clear_speed: 0.5,
                overflow: None,
                held: 0,
                quarantined: 0,
                capture: None,
                request_high: 0,
                ingress: 0,
                last_empty: None,
                partial: false,
                continuity: InputContinuity::Unknown,
            })),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    pub fn epoch(&self) -> Option<TransportEpoch> {
        if self.stop.load(Ordering::Acquire) {
            None
        } else {
            self.shared.try_lock().ok().map(|s| s.epoch)
        }
    }
    pub fn request(&self, request: CaptureRequest, now: Instant) -> CaptureRequestOutcome {
        use CaptureRequestOutcome::*;
        if self.stop.load(Ordering::Acquire) {
            return Rejected(CaptureReject::NotStarted);
        }
        let Ok(mut s) = self.shared.try_lock() else {
            return Rejected(CaptureReject::Busy);
        };
        if request.requested_at > now
            || request.not_before < request.requested_at
            || request.expires_at <= request.not_before
            || request.expires_at > request.not_before + Duration::from_millis(10)
        {
            return Rejected(CaptureReject::Stale);
        }
        if now >= request.expires_at {
            return Rejected(CaptureReject::Expired);
        }
        if request.epoch != s.epoch || request.id.actor != s.epoch.actor || request.id.sequence == 0
        {
            return Rejected(CaptureReject::Stale);
        }
        if let Some(CapturePhase::Pending(active)) = s.capture
            && active == request
        {
            return Pending;
        }
        if s.capture.is_some() {
            return Rejected(CaptureReject::Busy);
        }
        if request.id.sequence <= s.request_high {
            return Rejected(CaptureReject::Stale);
        }
        if s.observation.connectivity != Connectivity::Connected {
            return Rejected(CaptureReject::NotStarted);
        }
        s.request_high = request.id.sequence;
        s.capture = Some(CapturePhase::Pending(request));
        Pending
    }
    pub fn evidence(&self, id: CaptureId, now: Instant) -> Option<CaptureEvidence> {
        if self.stop.load(Ordering::Acquire) {
            return None;
        }
        let s = self.shared.try_lock().ok()?;
        match s.capture {
            Some(CapturePhase::Ready(e))
                if e.request.id == id && now >= e.observed_at && now < e.lease_until =>
            {
                Some(e)
            }
            _ => None,
        }
    }
    pub fn release(&self, token: CaptureToken) {
        if let Ok(mut s) = self.shared.try_lock()
            && s.capture.and_then(CapturePhase::evidence) == Some(token.evidence)
        {
            s.capture = None;
        }
    }
    fn validate(
        s: &Shared,
        e: CaptureEvidence,
        now: Instant,
    ) -> std::result::Result<(), CaptureReject> {
        if now >= e.lease_until {
            return Err(CaptureReject::Expired);
        }
        if now < e.observed_at
            || e.started_at < e.request.not_before
            || e.observed_at < e.started_at
            || e.completed_at < e.observed_at
            || now < e.completed_at
            || e.request.epoch != s.epoch
            || e.command_revision != s.revision
            || e.ingress_frontier != s.ingress
            || e.event_frontier != s.frontier
            || e.partial_frame
            || e.backlog
            || e.raw_frames >= 128
            || e.continuity != InputContinuity::Continuous
        {
            return Err(CaptureReject::Stale);
        }
        if s.observation.connectivity != Connectivity::Connected
            || s.overflow.is_some()
            || s.quarantined != 0
            || s.observation
                .last_failure
                .as_ref()
                .is_some_and(|f| f.active)
        {
            return Err(CaptureReject::Faulted);
        }
        Ok(())
    }
    pub fn claim(&self, e: CaptureEvidence, now: Instant) -> CaptureClaim {
        if self.stop.load(Ordering::Acquire) {
            return CaptureClaim::Rejected(CaptureReject::NotStarted);
        }
        let Ok(mut s) = self.shared.try_lock() else {
            return CaptureClaim::Rejected(CaptureReject::Busy);
        };
        if !matches!(s.capture,Some(CapturePhase::Ready(active)) if active==e) {
            return CaptureClaim::Rejected(CaptureReject::Stale);
        }
        if let Err(reason) = Self::validate(&s, e, now) {
            s.capture = None;
            return CaptureClaim::Rejected(reason);
        }
        s.capture = Some(CapturePhase::Claimed(e));
        CaptureClaim::Claimed(CaptureToken { evidence: e })
    }
    pub fn consume(&self, token: CaptureToken, applied: u64, now: Instant) -> CaptureConsume {
        if self.stop.load(Ordering::Acquire) {
            return CaptureConsume::Rejected(CaptureReject::NotStarted);
        }
        let Ok(mut s) = self.shared.try_lock() else {
            return CaptureConsume::Rejected(CaptureReject::Busy);
        };
        let e = token.evidence;
        if !matches!(s.capture,Some(CapturePhase::Claimed(active)) if active==e) {
            return CaptureConsume::Rejected(CaptureReject::Stale);
        }
        if let Err(reason) = Self::validate(&s, e, now) {
            s.capture = None;
            return CaptureConsume::Rejected(reason);
        }
        if applied < e.event_frontier || applied > s.delivered {
            return CaptureConsume::Rejected(CaptureReject::Stale);
        }
        s.capture = Some(CapturePhase::Consumed(e));
        CaptureConsume::Consumed
    }
    pub fn observation(&self) -> Option<DeviceObservation> {
        self.shared.try_lock().ok().map(|s| {
            let mut observation = s.observation.clone();
            if self.stopping() {
                observation.connectivity = Connectivity::Unknown;
                observation.layer = None;
            }
            observation
        })
    }
    pub fn poll(&self, max: usize) -> Delivery {
        let Ok(mut s) = self.shared.try_lock() else {
            return Delivery {
                records: vec![],
                frontier: None,
                fault: false,
                partial: false,
                continuity: InputContinuity::Unknown,
            };
        };
        let fault = s.overflow.is_some();
        let n = max.min(s.events.len());
        let mut records: Vec<_> = s.events.drain(..n).collect();
        if s.events.is_empty()
            && records.len() < max
            && let Some(boundary) = s.overflow.take()
        {
            records.push(boundary);
        }
        if let Some(record) = records.last() {
            s.delivered = record.seq;
        }
        Delivery {
            records,
            frontier: Some(s.delivered),
            fault,
            partial: s.partial,
            continuity: s.continuity,
        }
    }
    pub fn submit(
        &self,
        epoch: Option<TransportEpoch>,
        desired: Option<LightingFrame>,
        clears: &[u8],
        speed: f32,
        now: Instant,
        refresh: Duration,
    ) -> Result<(PublishOutcome, bool)> {
        if self.stop.load(Ordering::Acquire) {
            return Ok((PublishOutcome::Unavailable, false));
        }
        let Ok(mut s) = self.shared.try_lock() else {
            return Ok((PublishOutcome::Unchanged, false));
        };
        if epoch != Some(s.epoch) || s.observation.connectivity != Connectivity::Connected {
            return Ok((PublishOutcome::Unavailable, false));
        }
        let mut bits = 0;
        for &id in clears {
            if !(6..=18).contains(&id) {
                bail!("reserved Creator clear");
            }
            bits |= 1 << (id - 6);
        }
        if let Some(frame) = &desired {
            for entry in frame
                .entries
                .as_array()
                .context("invalid Creator entries")?
            {
                if !entry
                    .get("id")
                    .and_then(Value::as_u64)
                    .is_some_and(|id| (6..=18).contains(&id))
                {
                    bail!("reserved Creator entry");
                }
            }
        }
        let updated = s.completion_notice && s.completed_revision == s.revision;
        s.completion_notice = false;
        let refresh_due = s.completed_revision == s.revision
            && s.observation
                .last_output_success
                .is_some_and(|at| now.saturating_duration_since(at) >= refresh);
        if s.desired != desired || s.desired_epoch != epoch || bits != 0 || refresh_due {
            let Some(revision) = s.revision.checked_add(1) else {
                self.counter_fault(&mut s, now);
                bail!("Creator command revision exhausted");
            };
            s.revision = revision;
            s.desired = desired;
            s.desired_epoch = epoch;
            s.clear_bits |= bits;
            s.clear_speed = speed;
            s.capture = None;
        }
        Ok((
            if updated && s.completed_revision == s.revision {
                PublishOutcome::Updated
            } else {
                PublishOutcome::Unchanged
            },
            true,
        ))
    }
}

pub(super) trait Starter {
    fn start(&mut self) -> Result<(TransportStart, Option<Endpoint>)>;
}

pub(super) struct ProductionStarter;
trait ExitProbe: Send {
    fn exited(&self) -> bool;
}
struct Joined(Arc<AtomicBool>);
impl ExitProbe for Joined {
    fn exited(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Default)]
struct Registry {
    lease: Option<Box<dyn ExitProbe>>,
}
impl Registry {
    fn available(&mut self) -> bool {
        if self.lease.as_ref().is_some_and(|lease| lease.exited()) {
            self.lease = None;
        }
        self.lease.is_none()
    }
}

impl Starter for ProductionStarter {
    fn start(&mut self) -> Result<(TransportStart, Option<Endpoint>)> {
        if cfg!(test) {
            bail!("Creator unit tests must inject a factory/starter; native startup is disabled");
        }
        static REGISTRY: Mutex<Registry> = Mutex::new(Registry { lease: None });
        static ACTOR: AtomicU64 = AtomicU64::new(1);
        static IDS: OnceLock<Arc<AtomicI64>> = OnceLock::new();
        let Ok(mut registry) = REGISTRY.try_lock() else {
            return Ok((TransportStart::Retiring, None));
        };
        if !registry.available() {
            return Ok((TransportStart::Retiring, None));
        }
        let actor = ACTOR
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Creator actor identity exhausted"))?;
        let endpoint = Endpoint::new(actor);
        let owner = endpoint.clone();
        let ids = IDS.get_or_init(|| Arc::new(AtomicI64::new(1))).clone();
        let finished = Arc::new(AtomicBool::new(false));
        let completion = finished.clone();
        let handle = std::thread::Builder::new()
            .name("creator-transport".into())
            .spawn(move || {
                let factory = HidFactory {
                    native: NativeDiscovery::default(),
                    stop: owner.stop.clone(),
                };
                let mut actor = Actor::new(owner, Box::new(factory), ids);
                while !actor.step(Instant::now()) {
                    std::thread::park_timeout(Duration::from_millis(1));
                }
            })?;
        // AIDEV-NOTE: is_finished is NOT proof of OS/TLS exit. This bounded
        // exit observer joins off-main; only an actual completed join frees the
        // registry lease. Spawn failure leaves the lease permanently fail-closed.
        registry.lease = Some(Box::new(Joined(finished)));
        if std::thread::Builder::new()
            .name("creator-exit-observer".into())
            .spawn(move || {
                let _ = handle.join();
                completion.store(true, Ordering::Release);
            })
            .is_err()
        {
            endpoint.stop();
        }
        Ok((TransportStart::Started, Some(endpoint)))
    }
}

trait Factory {
    fn open(&mut self, ids: Arc<AtomicI64>) -> Poll<Result<Pad>>;
}
struct HidFactory<D = NativeDiscovery> {
    native: D,
    stop: Arc<AtomicBool>,
}
impl<D: Discovery> Factory for HidFactory<D> {
    fn open(&mut self, ids: Arc<AtomicI64>) -> Poll<Result<Pad>> {
        Poll::Ready(Pad::discover_with(
            &mut self.native,
            Some(ids),
            Some(self.stop.clone()),
        ))
    }
}

struct Required {
    id: i64,
    deadline: Instant,
}
struct OptionalReply {
    id: i64,
    method: &'static str,
    epoch: TransportEpoch,
    revision: u64,
    confirmed_at: Option<Instant>,
}
struct OutputTx {
    id: i64,
    method: &'static str,
    epoch: TransportEpoch,
    revision: u64,
    reports: Vec<Vec<u8>>,
    index: usize,
    clears: u16,
    final_frame: bool,
}
struct FrameJob {
    epoch: TransportEpoch,
    revision: u64,
    commands: VecDeque<(&'static str, Value)>,
}
struct Actor {
    endpoint: Endpoint,
    factory: Box<dyn Factory>,
    ids: Arc<AtomicI64>,
    pad: Option<Pad>,
    required: Option<Required>,
    closing: bool,
    retry_at: Option<Instant>,
    status_bytes: Option<(i64, Vec<Vec<u8>>, usize)>,
    output: Option<OutputTx>,
    job: Option<FrameJob>,
    optional: VecDeque<OptionalReply>,
    opening: bool,
    blocked_write: Option<Vec<u8>>,
    shutdown_clear: Option<OutputTx>,
    shutdown_prepared: bool,
}

fn reports(method: &str, params: Value, id: i64) -> Result<Vec<Vec<u8>>> {
    let bytes = serde_json::to_vec(&json!({"method":method,"id":id,"params":params}))?;
    if bytes.len() > ACC_CAP {
        bail!("Creator command exceeds bound");
    }
    Ok(bytes
        .chunks(MAX_CHUNK)
        .map(|chunk| {
            let mut report = vec![0; REPORT_SIZE];
            report[..3].copy_from_slice(&[REPORT_ID, CHANNEL_RPC, chunk.len() as u8]);
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect())
}

impl Actor {
    fn new(endpoint: Endpoint, factory: Box<dyn Factory>, ids: Arc<AtomicI64>) -> Self {
        Self {
            endpoint,
            factory,
            ids,
            pad: None,
            required: None,
            closing: false,
            retry_at: None,
            status_bytes: None,
            output: None,
            job: None,
            optional: VecDeque::new(),
            opening: false,
            blocked_write: None,
            shutdown_clear: None,
            shutdown_prepared: false,
        }
    }
    fn enqueue(&mut self, frame: Option<TimedFrame>) {
        self.enqueue_record(frame, false);
    }
    fn enqueue_record(&mut self, frame: Option<TimedFrame>, connection_lost: bool) {
        let mut s = self.endpoint.shared.lock().unwrap();
        let key = frame.as_ref().and_then(|frame| {
            if frame.value["m"] != "v.oai.hid" {
                return None;
            }
            let id = frame
                .value
                .pointer("/p/k")?
                .as_str()?
                .strip_prefix("AG")?
                .parse::<u8>()
                .ok()?;
            if !(6..=18).contains(&id) {
                return None;
            }
            let pressed = match frame.value.pointer("/p/act")?.as_u64()? {
                0 => false,
                1 => true,
                _ => return None,
            };
            Some((1u16 << (id - 6), pressed))
        });
        if let Some((bit, pressed)) = key {
            if pressed {
                s.held |= bit;
            } else {
                s.held &= !bit;
                s.quarantined &= !bit;
            }
            if pressed && s.quarantined & bit != 0 {
                return;
            }
        }
        if s.overflow.is_some() {
            if connection_lost {
                s.overflow.as_mut().unwrap().connection_lost = true;
            }
            s.quarantined |= s.held;
            return;
        }
        if s.events.len() >= EVENT_CAP {
            s.capture = None;
            s.continuity = InputContinuity::Lost;
            let Some(seq) = s.frontier.checked_add(1) else {
                self.endpoint
                    .counter_fault(&mut s, (self.pad.as_ref().unwrap().clock)());
                return;
            };
            s.frontier = seq;
            s.overflow = Some(Record {
                connection_lost,
                seq: s.frontier,
                epoch: s.epoch,
                frame: None,
            });
            s.quarantined |= s.held;
            s.observation.last_failure = Some(TypedFailure {
                code: DiagnosticCode::InputFailed,
                at: (self.pad.as_ref().unwrap().clock)(),
                active: true,
                recovery: Recovery::ReconnectDevice,
            });
            return;
        }
        let Some(seq) = s.frontier.checked_add(1) else {
            self.endpoint
                .counter_fault(&mut s, (self.pad.as_ref().unwrap().clock)());
            return;
        };
        s.frontier = seq;
        let epoch = s.epoch;
        s.events.push_back(Record {
            epoch,
            seq,
            frame,
            connection_lost,
        });
    }
    fn fail(&mut self, code: DiagnosticCode, now: Instant) {
        self.blocked_write = None;
        {
            let mut s = self.endpoint.shared.lock().unwrap();
            s.capture = None;
            s.last_empty = None;
            s.quarantined |= s.held;
            s.continuity = InputContinuity::Lost;
            s.partial = self.pad.as_ref().is_some_and(|p| !p.acc.trim().is_empty());
            s.observation.connectivity = Connectivity::Disconnected;
            s.observation.layer = None;
            s.observation.observed_at = Some(now);
            s.observation.last_failure = Some(TypedFailure {
                code,
                at: now,
                active: true,
                recovery: Recovery::ReconnectDevice,
            });
        }
        self.required = None;
        self.status_bytes = None;
        if let Some(tx) = self.output.take() {
            self.endpoint.shared.lock().unwrap().uncertain_clears |= tx.clears;
        }
        self.job = None;
        self.enqueue_record(None, true);
        self.closing = true;
        self.retry_at = Some(now + RETRY);
    }
    fn step(&mut self, now: Instant) -> bool {
        {
            let mut s = self.endpoint.shared.lock().unwrap();
            if let Some(e) = s.capture.and_then(CapturePhase::evidence) {
                if now < e.lease_until && !self.endpoint.stop.load(Ordering::Acquire) {
                    return false;
                }
                s.capture = None;
            }
            if matches!(s.capture,Some(CapturePhase::Pending(r)) if now>=r.expires_at) {
                s.capture = None;
            }
        }
        if self.endpoint.stop.load(Ordering::Acquire) {
            self.closing = true;
        }
        if self.closing {
            // A pending fake operation represents a native call which has not
            // returned. Stop cannot cancel it or release the lifetime lease.
            if self.opening {
                match self.factory.open(self.ids.clone()) {
                    Poll::Pending => return false,
                    Poll::Ready(result) => {
                        self.pad = result.ok();
                        self.opening = false;
                    }
                }
            }
            if let Some(report) = &self.blocked_write {
                if self
                    .pad
                    .as_ref()
                    .unwrap()
                    .dev
                    .actor_write(report)
                    .is_pending()
                {
                    return false;
                }
                self.blocked_write = None;
                // A returned in-flight report is not proof the whole RPC sent.
                self.shutdown_prepared = true;
            }
            if let Some(tx) = self.output.take() {
                self.endpoint.shared.lock().unwrap().uncertain_clears |= tx.clears;
                if tx.index > 0 {
                    self.shutdown_prepared = true;
                }
            }
            self.job = None;
            if self.endpoint.stopping() && !self.shutdown_prepared {
                self.shutdown_prepared = true;
                let cleanup = {
                    let mut s = self.endpoint.shared.lock().unwrap();
                    s.capture = None;
                    if s.observation.connectivity == Connectivity::Connected
                        && self.pad.is_some()
                        && s.clear_bits != 0
                    {
                        Some((
                            s.epoch,
                            s.revision,
                            std::mem::take(&mut s.clear_bits),
                            s.clear_speed,
                        ))
                    } else {
                        None
                    }
                };
                if let Some((epoch, revision, bits, speed)) = cleanup
                    && let Some(id) = self.next_id(now)
                {
                    let payload = Value::Array(
                        (6..=18)
                            .filter(|id| bits & (1 << (id - 6)) != 0)
                            .map(|id| frame(id, None, speed))
                            .collect(),
                    );
                    self.shutdown_clear = Some(OutputTx {
                        id,
                        method: "v.oai.thstatus",
                        epoch,
                        revision,
                        reports: reports("v.oai.thstatus", payload, id).unwrap(),
                        index: 0,
                        clears: bits,
                        final_frame: false,
                    });
                }
            }
            if let Some(tx) = &mut self.shutdown_clear {
                match self
                    .pad
                    .as_ref()
                    .unwrap()
                    .dev
                    .actor_write(&tx.reports[tx.index])
                {
                    Poll::Pending => return false,
                    Poll::Ready(Ok(n)) if n == REPORT_SIZE => tx.index += 1,
                    _ => {
                        let tx = self.shutdown_clear.take().unwrap();
                        self.endpoint.shared.lock().unwrap().uncertain_clears |= tx.clears;
                        self.fail(DiagnosticCode::OutputFailed, now);
                        return false;
                    }
                }
                if tx.index < tx.reports.len() {
                    return false;
                }
                let at = (self.pad.as_ref().unwrap().clock)();
                self.endpoint
                    .shared
                    .lock()
                    .unwrap()
                    .observation
                    .last_output_success = Some(at);
                self.shutdown_clear = None;
            }
            if let Some(pad) = &self.pad
                && pad.dev.actor_close().is_pending()
            {
                return false;
            }
            self.pad.take();
            self.closing = false;
            if self.endpoint.stop.load(Ordering::Acquire) {
                return true;
            }
            return false;
        }
        if self.pad.is_none() {
            if self.retry_at.is_some_and(|at| now < at) {
                return false;
            }
            self.opening = true;
            match self.factory.open(self.ids.clone()) {
                Poll::Pending => return false,
                Poll::Ready(Err(error)) => {
                    self.opening = false;
                    let absent = error.downcast_ref::<TransportContext>()
                        == Some(&TransportContext::ExpectedAbsence);
                    let mut s = self.endpoint.shared.lock().unwrap();
                    s.observation.connectivity = if absent {
                        Connectivity::Disconnected
                    } else {
                        Connectivity::Unknown
                    };
                    if absent {
                        s.observation.last_failure = Some(TypedFailure {
                            code: DiagnosticCode::DeviceUnavailable,
                            at: now,
                            active: false,
                            recovery: Recovery::None,
                        });
                    }
                    self.retry_at = Some(now + RETRY);
                    return false;
                }
                Poll::Ready(Ok(pad)) => {
                    self.opening = false;
                    self.pad = Some(pad);
                    {
                        let mut s = self.endpoint.shared.lock().unwrap();
                        let Some(connection) = s.epoch.connection.checked_add(1) else {
                            self.endpoint.counter_fault(&mut s, now);
                            return false;
                        };
                        s.epoch.connection = connection;
                        s.uncertain_clears |= s.clear_bits;
                        s.clear_bits = 0;
                        s.desired = None;
                        s.desired_epoch = None;
                        s.completed_revision = s.revision;
                        s.last_empty = None;
                        s.partial = false;
                        s.continuity = InputContinuity::Unknown;
                        s.capture = None;
                        s.completion_notice = false;
                    }
                    let Ok(id) = self
                        .ids
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                            n.checked_add(1).filter(|v| *v > 0)
                        })
                    else {
                        self.fail(DiagnosticCode::OutputFailed, now);
                        self.endpoint.stop();
                        return false;
                    };
                    self.status_bytes =
                        Some((id, reports("device.status", json!(null), id).unwrap(), 0));
                }
            }
        }
        if self.endpoint.stopping() {
            self.closing = true;
            return false;
        }
        if let Some((id, reports, index)) = &mut self.status_bytes {
            match self.pad.as_ref().unwrap().dev.actor_write(&reports[*index]) {
                Poll::Pending => {
                    self.blocked_write = Some(reports[*index].clone());
                    return false;
                }
                Poll::Ready(Ok(n)) if n == REPORT_SIZE => {
                    *index += 1;
                }
                _ => {
                    self.fail(DiagnosticCode::OutputFailed, now);
                    return false;
                }
            }
            self.blocked_write = None;
            if *index == reports.len() {
                let at = (self.pad.as_ref().unwrap().clock)();
                self.required = Some(Required {
                    id: *id,
                    deadline: at + Duration::from_millis(700),
                });
                self.endpoint
                    .shared
                    .lock()
                    .unwrap()
                    .observation
                    .last_output_success = Some(at);
                self.status_bytes = None;
            }
            return false;
        }
        if self.required.as_ref().is_some_and(|r| now >= r.deadline) {
            self.fail(DiagnosticCode::InputFailed, now);
            return false;
        }
        if self.blocked_write.is_some() && self.output.is_some() {
            self.output_step(now);
            return false;
        }
        let mut raw_frames = 0;
        let mut exhausted = false;
        for _ in 0..128 {
            let pad = self.pad.as_mut().unwrap();
            if (pad.clock)() >= now + Duration::from_millis(5) {
                break;
            }
            let buffered = pad.buffered_frame();
            {
                let mut s = self.endpoint.shared.lock().unwrap();
                let Some(ingress) = s.ingress.checked_add(1) else {
                    self.endpoint.counter_fault(&mut s, now);
                    return false;
                };
                s.ingress = ingress;
            }
            let step = if !matches!(buffered, Ok(None)) {
                ReadStep {
                    frame: buffered,
                    report_read: false,
                    empty: false,
                }
            } else {
                let mut bytes = [0; REPORT_SIZE];
                match pad.dev.actor_read(&mut bytes) {
                    Poll::Pending => return false,
                    Poll::Ready(Err(_)) => {
                        self.fail(DiagnosticCode::InputFailed, now);
                        return false;
                    }
                    Poll::Ready(Ok(n)) if n <= REPORT_SIZE => pad.ingest_report(&bytes[..n]),
                    _ => {
                        self.fail(DiagnosticCode::InputFailed, now);
                        return false;
                    }
                }
            };
            if pad.acc.len() > ACC_CAP {
                self.fail(DiagnosticCode::InputFailed, now);
                return false;
            }
            self.endpoint.shared.lock().unwrap().partial = !pad.acc.trim().is_empty();
            if step.report_read {
                raw_frames += 1;
                let mut s = self.endpoint.shared.lock().unwrap();
                s.observation.last_input_success = pad.last_input_success;
                s.observation.observed_at = pad.last_input_success;
            }
            match step.frame {
                Err(_) => {
                    self.fail(DiagnosticCode::InputFailed, now);
                    return false;
                }
                Ok(Some(frame)) => self.frame(frame),
                Ok(None) => {}
            }
            if step.empty {
                let at = (self.pad.as_ref().unwrap().clock)();
                let partial = !self.pad.as_ref().unwrap().acc.trim().is_empty();
                exhausted = !partial && at < now + Duration::from_millis(5);
                let mut s = self.endpoint.shared.lock().unwrap();
                let continuous = s.last_empty.is_some_and(|old| {
                    at >= old && at.duration_since(old) <= Duration::from_millis(50)
                });
                let gap = s.last_empty.is_some_and(|old| {
                    at < old || at.duration_since(old) > Duration::from_millis(50)
                });
                s.last_empty = Some(at);
                if gap {
                    s.capture = None;
                    s.continuity = InputContinuity::Lost;
                    s.quarantined |= s.held;
                    s.observation.last_failure = Some(TypedFailure {
                        code: DiagnosticCode::InputFailed,
                        at,
                        active: true,
                        recovery: Recovery::ReconnectDevice,
                    });
                    drop(s);
                    self.enqueue(None);
                    break;
                }
                if continuous
                    && !partial
                    && s.quarantined == 0
                    && s.overflow.is_none()
                    && s.delivered == s.frontier
                {
                    s.continuity = InputContinuity::Continuous;
                    if let Some(f) = &mut s.observation.last_failure
                        && f.code == DiagnosticCode::InputFailed
                    {
                        f.active = false;
                    }
                }
                if let Some(CapturePhase::Pending(req)) = s.capture
                    && now >= req.not_before
                    && req.epoch == s.epoch
                    && at < req.expires_at
                    && at < now + Duration::from_millis(5)
                    && !partial
                    && continuous
                    && s.overflow.is_none()
                    && s.quarantined == 0
                    && self.required.is_none()
                    && self.output.is_none()
                    && self.job.is_none()
                    && s.clear_bits == 0
                    && s.completed_revision == s.revision
                {
                    s.capture = Some(CapturePhase::Ready(CaptureEvidence {
                        request: req,
                        command_revision: s.revision,
                        ingress_frontier: s.ingress,
                        event_frontier: s.frontier,
                        started_at: now,
                        observed_at: at,
                        completed_at: at,
                        lease_until: req.expires_at.min(at + Duration::from_millis(10)),
                        raw_frames,
                        partial_frame: false,
                        backlog: false,
                        continuity: InputContinuity::Continuous,
                    }));
                }
            }
            if self.closing || step.empty {
                break;
            }
        }
        if !self.closing && self.required.is_none() && exhausted {
            self.output_step(now);
        }
        false
    }
    fn next_id(&mut self, now: Instant) -> Option<i64> {
        match self
            .ids
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(1).filter(|v| *v > 0)
            }) {
            Ok(id) => Some(id),
            Err(_) => {
                self.fail(DiagnosticCode::OutputFailed, now);
                self.endpoint.stop();
                None
            }
        }
    }
    fn output_step(&mut self, now: Instant) {
        if self.endpoint.stopping() {
            return;
        }
        if self.output.is_none() {
            let (epoch, revision, bits, speed, desired, complete) = {
                let mut s = self.endpoint.shared.lock().unwrap();
                if s.observation.connectivity != Connectivity::Connected {
                    return;
                }
                if s.desired_epoch != Some(s.epoch) {
                    s.desired = None;
                    s.completed_revision = s.revision;
                    s.completion_notice = false;
                }
                let bits = std::mem::take(&mut s.clear_bits);
                (
                    s.epoch,
                    s.revision,
                    bits,
                    s.clear_speed,
                    s.desired.clone(),
                    s.completed_revision,
                )
            };
            let (method, payload, final_frame) = if bits != 0 {
                (
                    "v.oai.thstatus",
                    Value::Array(
                        (6..=18)
                            .filter(|id| bits & (1 << (id - 6)) != 0)
                            .map(|id| frame(id, None, speed))
                            .collect(),
                    ),
                    false,
                )
            } else {
                if self
                    .job
                    .as_ref()
                    .is_some_and(|j| j.epoch != epoch || j.revision != revision)
                {
                    self.job = None;
                }
                if self.job.is_none() && complete != revision {
                    if let Some(frame) = desired {
                        self.job = Some(FrameJob {
                            epoch,
                            revision,
                            commands: frame.commands().collect(),
                        });
                    } else {
                        self.endpoint.shared.lock().unwrap().completed_revision = revision;
                        return;
                    }
                }
                let Some(job) = &mut self.job else {
                    return;
                };
                let Some((method, payload)) = job.commands.pop_front() else {
                    self.job = None;
                    return;
                };
                (method, payload, job.commands.is_empty())
            };
            let Some(id) = self.next_id(now) else {
                return;
            };
            self.output = Some(OutputTx {
                id,
                method,
                epoch,
                revision,
                reports: reports(method, payload, id).unwrap(),
                index: 0,
                clears: bits,
                final_frame,
            });
        }
        let epoch = self.endpoint.shared.lock().unwrap().epoch;
        if self.output.as_ref().is_some_and(|tx| tx.epoch != epoch) {
            let tx = self.output.take().unwrap();
            if tx.index > 0 {
                self.endpoint.shared.lock().unwrap().uncertain_clears |= tx.clears;
                self.fail(DiagnosticCode::OutputFailed, now);
            } else if tx.epoch.connection == epoch.connection {
                self.endpoint.shared.lock().unwrap().clear_bits |= tx.clears;
            }
            return;
        }
        let tx = self.output.as_mut().unwrap();
        match self
            .pad
            .as_ref()
            .unwrap()
            .dev
            .actor_write(&tx.reports[tx.index])
        {
            Poll::Pending => {
                self.blocked_write = Some(tx.reports[tx.index].clone());
                return;
            }
            Poll::Ready(Ok(n)) if n == REPORT_SIZE => tx.index += 1,
            _ => {
                self.fail(DiagnosticCode::OutputFailed, now);
                return;
            }
        }
        self.blocked_write = None;
        if tx.index == tx.reports.len() {
            let tx = self.output.take().unwrap();
            let at = (self.pad.as_ref().unwrap().clock)();
            {
                let mut s = self.endpoint.shared.lock().unwrap();
                s.observation.last_output_success = Some(at);
                if tx.epoch == s.epoch && tx.revision == s.revision {
                    if let Some(f) = &mut s.observation.last_failure
                        && f.code == DiagnosticCode::OutputFailed
                    {
                        f.active = false;
                    }
                    if tx.final_frame
                        || (tx.clears != 0 && s.desired.is_none() && s.clear_bits == 0)
                    {
                        s.completed_revision = tx.revision;
                        s.completion_notice = true;
                    }
                }
            }
            if self.optional.len() == 8 {
                self.optional.pop_front();
            }
            self.optional.push_back(OptionalReply {
                id: tx.id,
                method: tx.method,
                epoch: tx.epoch,
                revision: tx.revision,
                confirmed_at: None,
            });
            if tx.final_frame {
                self.job = None;
            }
        }
    }
    fn frame(&mut self, frame: TimedFrame) {
        let value = &frame.value;
        if value.get("m").is_none()
            && let Some(reply) = self
                .optional
                .iter_mut()
                .find(|r| value.get("id").and_then(Value::as_i64) == Some(r.id))
        {
            let mut s = self.endpoint.shared.lock().unwrap();
            if value.get("method").is_some_and(|m| m != reply.method) {
                return;
            }
            if value.get("error").is_some() {
                if reply.epoch == s.epoch && reply.revision == s.revision {
                    s.observation.last_failure = Some(TypedFailure {
                        code: DiagnosticCode::OutputFailed,
                        at: frame.observed_at,
                        active: true,
                        recovery: Recovery::ReconnectDevice,
                    });
                }
            } else if value.pointer("/result/ok").and_then(Value::as_u64) == Some(1) {
                reply.confirmed_at = Some(frame.observed_at);
            }
            return;
        }
        if self
            .required
            .as_ref()
            .is_some_and(|r| value.get("id").and_then(Value::as_i64) == Some(r.id))
        {
            let result = value.get("result");
            let selector = result
                .and_then(|v| {
                    Some((
                        u16::try_from(v.get("profile_index")?.as_u64()?).ok()?,
                        u16::try_from(v.get("layer_index")?.as_u64()?).ok()?,
                    ))
                })
                .filter(|(_, l)| *l > 0);
            if value.get("error").is_some()
                || value.get("m").is_some()
                || value.get("method").is_some_and(|m| m != "device.status")
                || selector.is_none()
            {
                self.fail(DiagnosticCode::InputFailed, frame.observed_at);
                return;
            }
            self.required = None;
            self.pad.as_mut().unwrap().pending.clear();
            let (p, l) = selector.unwrap();
            self.selector(p, l, frame.observed_at);
            return;
        }
        if self.required.is_some() {
            return;
        }
        if value.get("m").and_then(Value::as_str) == Some("kb.pl") {
            if let Some((p, l)) = value
                .get("p")
                .and_then(|v| {
                    Some((
                        u16::try_from(v.get("p")?.as_u64()?).ok()?,
                        u16::try_from(v.get("l")?.as_u64()?).ok()?,
                    ))
                })
                .filter(|(_, l)| *l > 0)
            {
                self.selector(p, l, frame.observed_at);
            }
        } else if value.get("m").is_some() {
            self.enqueue(Some(frame));
        }
    }
    fn selector(&mut self, profile: u16, position: u16, at: Instant) {
        {
            let mut s = self.endpoint.shared.lock().unwrap();
            let old = s
                .observation
                .layer
                .as_ref()
                .map(|l| (l.profile, l.position));
            if old != Some((Some(profile), Some(position))) {
                let Some(selector) = s.epoch.selector.checked_add(1) else {
                    self.endpoint.counter_fault(&mut s, at);
                    return;
                };
                s.epoch.selector = selector;
            }
            if old != Some((Some(profile), Some(position))) {
                s.capture = None;
                s.quarantined |= s.held;
                s.continuity = InputContinuity::Reset;
            }
            s.observation.connectivity = Connectivity::Connected;
            s.observation.observed_at = Some(at);
            s.observation.layer = Some(ObservedLayer {
                name: None,
                profile: Some(profile),
                position: Some(position),
                observed_at: at,
            });
            if let Some(f) = &mut s.observation.last_failure
                && f.code != DiagnosticCode::OutputFailed
            {
                f.active = false;
            }
        }
        self.enqueue(Some(TimedFrame {
            value: json!({"m":"kb.pl","p":{"p":profile,"l":position}}),
            observed_at: at,
        }));
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
