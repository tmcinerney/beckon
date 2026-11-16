//! One off-main status owner. The registry keeps its lease through native close
//! and panic unwinding; frontend retirement is never evidence of thread exit.
use super::*;
use crate::display::{
    Connectivity, DeviceObservation, DiagnosticCode, PublishOutcome, Recovery, TransportStart,
    TypedFailure,
};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::thread::JoinHandle;

struct ExitLease {
    exited: Arc<AtomicBool>,
    _observer: Option<JoinHandle<()>>,
}
type Registry = Arc<Mutex<Option<ExitLease>>>;
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;
type Runner = Box<dyn FnOnce(Arc<Shared>, Receiver<()>, Clock) + Send>;
type Signature = ([Status; SLOT_COUNT], [Treatment; 5]);
static REGISTRY: OnceLock<Registry> = OnceLock::new();

struct Desired {
    revision: u64,
    plan: RenderPlan,
    signature: Signature,
}
#[derive(Clone, Copy)]
enum Completion {
    Updated,
    Unchanged,
    Unavailable,
    Failed,
}
struct Slots {
    desired: Option<Desired>,
    completion: Option<(u64, u64, Completion)>,
    observation: DeviceObservation,
}
struct Shared {
    stop: AtomicBool,
    slots: Mutex<Slots>,
}

pub(crate) struct StatusPublicationWorker {
    shared: Arc<Shared>,
    registry: Registry,
    clock: Clock,
    runner: Option<Runner>,
    wake: Option<SyncSender<()>>,
    result_seen: u64,
    revision: u64,
}
impl StatusPublicationWorker {
    pub(crate) fn new<F, W>(factory: F) -> Self
    where
        F: FnOnce() -> W + Send + 'static,
        W: StatusWriter + 'static,
    {
        Self {
            shared: Arc::new(Shared {
                stop: AtomicBool::new(false),
                slots: Mutex::new(Slots {
                    desired: None,
                    completion: None,
                    observation: DeviceObservation {
                        observation_supported: true,
                        ..Default::default()
                    },
                }),
            }),
            registry: REGISTRY.get_or_init(|| Arc::new(Mutex::new(None))).clone(),
            clock: Arc::new(Instant::now),
            runner: Some(Box::new(move |shared, wake, clock| {
                run(RenderSink::new(factory()), shared, wake, clock)
            })),
            wake: None,
            result_seen: 0,
            revision: 0,
        }
    }
    #[cfg(test)]
    pub(crate) fn isolated<F, W>(factory: F) -> Self
    where
        F: FnOnce() -> W + Send + 'static,
        W: StatusWriter + 'static,
    {
        let mut worker = Self::new(factory);
        worker.registry = Arc::new(Mutex::new(None));
        worker
    }
    pub(crate) fn start(&mut self) -> Result<TransportStart> {
        if self.shared.stop.load(Ordering::Acquire) {
            return Ok(TransportStart::Retiring);
        }
        if self.wake.is_some() {
            return Ok(TransportStart::Running);
        }
        let Ok(mut lease) = self.registry.try_lock() else {
            return Ok(TransportStart::Retiring);
        };
        if lease
            .as_ref()
            .is_some_and(|lease| !lease.exited.load(Ordering::Acquire))
        {
            return Ok(TransportStart::Retiring);
        }
        // Only the observer's completed join proves native/TLS exit. In
        // particular, JoinHandle::is_finished alone permits a TLS teardown race.
        lease.take();
        let runner = self
            .runner
            .take()
            .ok_or_else(|| anyhow::anyhow!("status owner cannot restart"))?;
        let (wake, rx) = mpsc::sync_channel(1);
        let shared = self.shared.clone();
        let clock = self.clock.clone();
        let owner = std::thread::Builder::new()
            .name("beckon-status-publication".into())
            .spawn(move || runner(shared, rx, clock))?;
        let exited = Arc::new(AtomicBool::new(false));
        let exit = exited.clone();
        let shared = self.shared.clone();
        let clock = self.clock.clone();
        let observer = std::thread::Builder::new()
            .name("beckon-status-exit".into())
            .spawn(move || {
                if owner.join().is_err() {
                    let mut slots = shared
                        .slots
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    slots.observation.connectivity = Connectivity::Unknown;
                    slots.observation.last_failure = Some(TypedFailure {
                        code: DiagnosticCode::OutputFailed,
                        at: clock(),
                        active: true,
                        recovery: Recovery::ReconnectDevice,
                    });
                }
                shared.stop.store(true, Ordering::Release);
                exit.store(true, Ordering::Release);
            });
        self.wake = Some(wake);
        match observer {
            Ok(observer) => {
                *lease = Some(ExitLease {
                    exited,
                    _observer: Some(observer),
                })
            }
            Err(error) => {
                self.shared.stop.store(true, Ordering::Release);
                *lease = Some(ExitLease {
                    exited,
                    _observer: None,
                });
                return Err(error.into());
            }
        }
        Ok(TransportStart::Started)
    }
    pub(crate) fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }
    pub(crate) fn diagnostics(&self) -> DeviceObservation {
        let mut observation = self
            .shared
            .slots
            .try_lock()
            .map(|slots| slots.observation.clone())
            .unwrap_or(DeviceObservation {
                observation_supported: true,
                ..Default::default()
            });
        if self.shared.stop.load(Ordering::Acquire) {
            observation.connectivity = Connectivity::Unknown;
        }
        observation
    }
    pub(crate) fn publish(&mut self, plan: &RenderPlan) -> Result<PublishOutcome> {
        let signature = (slots_for_plan(plan)?, treatments_for_plan(plan));
        if self.wake.is_none() || self.shared.stop.load(Ordering::Acquire) {
            return Ok(PublishOutcome::Unavailable);
        }
        let Ok(mut slots) = self.shared.slots.try_lock() else {
            return Ok(PublishOutcome::Unchanged);
        };
        if slots
            .desired
            .as_ref()
            .is_none_or(|desired| desired.signature != signature)
        {
            self.revision = self
                .revision
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("status revision exhausted"))?;
            slots.desired = Some(Desired {
                revision: self.revision,
                plan: plan.clone(),
                signature,
            });
        }
        let result = slots.completion.filter(|(serial, revision, _)| {
            *serial > self.result_seen && *revision == self.revision
        });
        drop(slots);
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
        match result {
            Some((serial, _, outcome)) => {
                self.result_seen = serial;
                match outcome {
                    Completion::Updated => Ok(PublishOutcome::Updated),
                    Completion::Unchanged => Ok(PublishOutcome::Unchanged),
                    Completion::Unavailable => Ok(PublishOutcome::Unavailable),
                    Completion::Failed => {
                        bail!("status publication failed; see device diagnostics")
                    }
                }
            }
            None => Ok(PublishOutcome::Unchanged),
        }
    }
}
impl Drop for StatusPublicationWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run<W: StatusWriter>(
    mut sink: RenderSink<W>,
    shared: Arc<Shared>,
    wake: Receiver<()>,
    clock: Clock,
) {
    let mut applied = 0;
    let mut serial = 0u64;
    while !shared.stop.load(Ordering::Acquire) {
        let now = clock();
        let desired = {
            let slots = shared
                .slots
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            slots.desired.as_ref().and_then(|desired| {
                let retry_due = sink.retry_after.is_some_and(|at| now >= at);
                let deferred = sink.retry_after.is_some_and(|at| now < at);
                (!deferred && (desired.revision != applied || retry_due))
                    .then(|| (desired.revision, desired.plan.clone()))
            })
        };
        if let Some((revision, plan)) = desired {
            // AIDEV-NOTE: no mailbox lock spans context/open/write/drop. A blocked
            // HIDAPI call retains this sole registry lease, not the main thread.
            let result = sink.publish_at(&plan, now);
            let completed_at = sink.writer.last_attempt_at().unwrap_or_else(|| clock());
            if result.is_err() {
                // A short/failed intervening write makes an earlier successful
                // snapshot uncertain too. Retry the LATEST desired state rather
                // than deduplicating a revert forever (or spinning on retry_due).
                sink.last_snapshot = None;
                sink.retry_after = Some(clock() + DISCONNECTED_RETRY_DELAY);
            }
            applied = revision;
            let Some(next) = serial.checked_add(1) else {
                break;
            };
            serial = next;
            let mut slots = shared
                .slots
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let current = slots
                .desired
                .as_ref()
                .is_some_and(|desired| desired.revision == revision);
            let outcome = match result {
                Ok(true) => {
                    slots.observation.observed_at = Some(completed_at);
                    slots.observation.connectivity = Connectivity::Connected;
                    slots.observation.last_output_success = Some(completed_at);
                    if current && let Some(failure) = &mut slots.observation.last_failure {
                        failure.active = false;
                    }
                    Completion::Updated
                }
                Ok(false) => Completion::Unchanged,
                Err(error) => {
                    let unavailable = is_status_endpoint_unavailable(&error);
                    slots.observation.observed_at = Some(completed_at);
                    slots.observation.connectivity = if unavailable {
                        Connectivity::Disconnected
                    } else {
                        Connectivity::Unknown
                    };
                    slots.observation.last_failure = Some(TypedFailure {
                        code: if unavailable {
                            DiagnosticCode::DeviceUnavailable
                        } else {
                            DiagnosticCode::OutputFailed
                        },
                        at: completed_at,
                        active: !unavailable,
                        recovery: Recovery::ReconnectDevice,
                    });
                    if unavailable {
                        Completion::Unavailable
                    } else {
                        Completion::Failed
                    }
                }
            };
            slots.completion = Some((serial, revision, outcome));
            continue;
        }
        // Main never waits here. The owner's finite wake interval positively
        // services an unavailable desired state even without another submission.
        let _ = wake.recv_timeout(Duration::from_millis(10));
    }
    // sink/writer (including any blocking native destructor) drops on THIS thread.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn status_owner_reverted_desired_after_failed_write_is_reconciled_not_cached_as_delivered() {
        struct Writer {
            attempt: u8,
            writes: SyncSender<StatusSnapshot>,
        }
        impl StatusWriter for Writer {
            fn write_snapshot(&mut self, snapshot: StatusSnapshot) -> Result<()> {
                self.attempt += 1;
                self.writes.send(snapshot).unwrap();
                if self.attempt == 2 {
                    bail!("fake short write");
                }
                Ok(())
            }
        }
        let start = Instant::now();
        let millis = Arc::new(AtomicU64::new(0));
        let clock = millis.clone();
        let (writes, seen) = mpsc::sync_channel(4);
        let mut worker = StatusPublicationWorker::isolated(move || Writer { attempt: 0, writes });
        worker.clock =
            Arc::new(move || start + Duration::from_millis(clock.load(Ordering::Acquire)));
        worker.start().unwrap();
        let mut plan =
            crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        worker.publish(&plan).unwrap();
        assert_eq!(
            seen.recv_timeout(Duration::from_secs(5)).unwrap().sequence,
            0
        );
        millis.store(10, Ordering::Release);
        plan.keys[0].state = Some(AgentState::Working);
        worker.publish(&plan).unwrap();
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        while worker.diagnostics().last_failure.is_none() {
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        plan.keys[0].state = None;
        worker.publish(&plan).unwrap();
        assert!(worker.diagnostics().last_failure.unwrap().active);
        millis.store(2010, Ordering::Release);
        let reconciled = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(reconciled.sequence, 1);
        assert_eq!(reconciled.slots[0], Status::Unbound);
        worker.stop();
    }

    #[test]
    fn status_owner_native_tls_exit_not_just_main_function_completion_fences_successor() {
        struct ExitGate {
            entered: SyncSender<()>,
            release: Receiver<()>,
        }
        impl Drop for ExitGate {
            fn drop(&mut self) {
                let _ = self.entered.send(());
                let _ = self.release.recv();
            }
        }
        thread_local! {static EXIT:std::cell::RefCell<Option<ExitGate>>=const {std::cell::RefCell::new(None)};}
        struct Discard;
        impl StatusWriter for Discard {
            fn write_snapshot(&mut self, _: StatusSnapshot) -> Result<()> {
                Ok(())
            }
        }
        let (entered, seen) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let mut first = StatusPublicationWorker::isolated(move || {
            EXIT.with(|slot| {
                *slot.borrow_mut() = Some(ExitGate {
                    entered,
                    release: gate,
                })
            });
            Discard
        });
        let registry = first.registry.clone();
        first.start().unwrap();
        drop(first);
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut successor = StatusPublicationWorker::isolated(|| Discard);
        successor.registry = registry;
        assert_eq!(
            successor.start().unwrap(),
            TransportStart::Retiring,
            "native TLS teardown has not returned"
        );
        release.send(()).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            if successor.start().unwrap() == TransportStart::Started {
                break;
            }
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        successor.stop();
    }

    #[test]
    fn status_owner_keeps_original_send_time_when_native_teardown_finishes_later() {
        struct Writer(Instant);
        impl StatusWriter for Writer {
            fn write_snapshot(&mut self, _: StatusSnapshot) -> Result<()> {
                Ok(())
            }
            fn last_attempt_at(&self) -> Option<Instant> {
                Some(self.0)
            }
        }
        let at = Instant::now();
        let mut worker = StatusPublicationWorker::isolated(move || Writer(at));
        worker.clock = Arc::new(move || at + Duration::from_secs(8));
        worker.start().unwrap();
        worker
            .publish(&crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap())
            .unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(observed) = worker.diagnostics().last_output_success {
                assert_eq!(observed, at);
                break;
            }
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        worker.stop();
    }
    #[test]
    fn status_owner_factory_is_lazy_and_blocked_close_keeps_lease_until_actual_exit() {
        struct Writer {
            entered: SyncSender<()>,
            gate: Receiver<()>,
        }
        impl StatusWriter for Writer {
            fn write_snapshot(&mut self, _: StatusSnapshot) -> Result<()> {
                Ok(())
            }
        }
        impl Drop for Writer {
            fn drop(&mut self) {
                let _ = self.entered.send(());
                let _ = self.gate.recv();
            }
        }
        let (closed, seen) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let (created, construction) = mpsc::sync_channel(1);
        let mut first = StatusPublicationWorker::isolated(move || {
            created.send(()).unwrap();
            Writer {
                entered: closed,
                gate,
            }
        });
        assert!(construction.try_recv().is_err());
        assert_eq!(first.start().unwrap(), TransportStart::Started);
        construction.recv_timeout(Duration::from_secs(5)).unwrap();
        struct Discard;
        impl StatusWriter for Discard {
            fn write_snapshot(&mut self, _: StatusSnapshot) -> Result<()> {
                Ok(())
            }
        }
        let mut successor = StatusPublicationWorker::isolated(|| Discard);
        successor.registry = first.registry.clone();
        drop(first);
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(successor.start().unwrap(), TransportStart::Retiring);
        release.send(()).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            if successor.start().unwrap() == TransportStart::Started {
                break;
            }
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        successor.stop();
    }
    #[test]
    fn status_owner_unavailable_retry_positively_delivers_without_resubmission_and_preserves_time()
    {
        struct Writer {
            attempt: bool,
            writes: SyncSender<StatusSnapshot>,
        }
        impl StatusWriter for Writer {
            fn write_snapshot(&mut self, snapshot: StatusSnapshot) -> Result<()> {
                if !self.attempt {
                    self.attempt = true;
                    return Err(StatusEndpointUnavailable.into());
                }
                self.writes.send(snapshot).unwrap();
                Ok(())
            }
        }
        let at = Instant::now();
        let elapsed = Arc::new(AtomicU64::new(0));
        let clock = elapsed.clone();
        let (writes, seen) = mpsc::sync_channel(1);
        let mut worker = StatusPublicationWorker::isolated(move || Writer {
            attempt: false,
            writes,
        });
        worker.clock = Arc::new(move || at + Duration::from_millis(clock.load(Ordering::Acquire)));
        worker.start().unwrap();
        let mut plan =
            crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
        for key in &mut plan.keys {
            key.state = Some(AgentState::Working);
        }
        worker.publish(&plan).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        while worker.diagnostics().connectivity != Connectivity::Disconnected {
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        for _ in 0..100 {
            worker.publish(&plan).unwrap();
        }
        assert!(seen.try_recv().is_err());
        assert!(worker.diagnostics().last_output_success.is_none());
        elapsed.store(2000, Ordering::Release);
        // No frontend submission/wake: the owner must service its retained slot.
        let sent = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(sent.sequence, 0);
        assert_eq!(sent.slots, [Status::Working; SLOT_COUNT]);
        while worker.diagnostics().last_output_success.is_none() {
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        elapsed.store(9000, Ordering::Release);
        assert_eq!(
            worker.diagnostics().last_output_success,
            Some(at + Duration::from_secs(2))
        );
        worker.stop();
    }
}
