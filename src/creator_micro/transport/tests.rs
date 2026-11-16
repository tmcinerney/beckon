use super::*;
use crate::display::{
    CaptureClaim, CaptureConsume, CaptureId, CaptureReject, CaptureRequest, CaptureRequestOutcome,
    InputPollBudget, InputPollMode,
};
use std::cell::{Cell, RefCell};

struct ProbeState {
    stop: Arc<AtomicBool>,
    mode: &'static str,
    time: Rc<Cell<Instant>>,
    log: RefCell<Vec<String>>,
    live: Cell<usize>,
    owner: std::thread::ThreadId,
}
impl ProbeState {
    fn record(&self, event: String) {
        assert_eq!(std::thread::current().id(), self.owner);
        self.log.borrow_mut().push(event);
    }
    fn stopping_at(&self, point: &str) {
        if self.mode == point {
            self.stop.store(true, Ordering::Release);
        }
    }
}
struct ProbeDiscovery(Rc<ProbeState>);
impl Discovery for ProbeDiscovery {
    type Candidate = usize;
    fn enumerate(&mut self, _: Option<&AtomicBool>) -> Result<Vec<usize>> {
        self.0.record("enum".into());
        self.0.stopping_at("enum");
        Ok(vec![0, 1])
    }
    fn open(&mut self, candidate: &usize) -> Result<Box<dyn ReportIo>> {
        self.0.record(format!("open{candidate}"));
        self.0.stopping_at("open");
        self.0.stopping_at("open-error");
        if self.0.mode == "open-error" {
            bail!("fake native open error");
        }
        assert_eq!(self.0.live.get(), 0);
        self.0.live.set(1);
        Ok(Box::new(ProbeDevice {
            state: self.0.clone(),
            candidate: *candidate,
            output: RefCell::new(vec![]),
            reads: RefCell::new(VecDeque::new()),
        }))
    }
    fn clock(&self) -> Clock {
        let time = self.0.time.clone();
        Rc::new(move || time.get())
    }
}
struct ProbeDevice {
    state: Rc<ProbeState>,
    candidate: usize,
    output: RefCell<Vec<u8>>,
    reads: RefCell<VecDeque<Vec<u8>>>,
}
impl Drop for ProbeDevice {
    fn drop(&mut self) {
        self.state.record(format!("close{}", self.candidate));
        self.state.live.set(self.state.live.get() - 1);
    }
}
impl ReportIo for ProbeDevice {
    fn write(&self, report: &[u8]) -> Result<usize> {
        self.state.record(format!("write{}", self.candidate));
        self.state.stopping_at("write");
        let mut output = self.output.borrow_mut();
        output.extend_from_slice(&report[3..3 + report[2] as usize]);
        if let Ok(value) = serde_json::from_slice::<Value>(&output) {
            self.state.record(format!(
                "rpc:{}:{}",
                value["method"].as_str().unwrap(),
                value["id"]
            ));
            let response = if value["method"] == "device.status" {
                json!({"id":value["id"],"result":{"profile_index":1,"layer_index":3}})
            } else if self.candidate == 0 && matches!(self.state.mode, "next" | "never") {
                json!({"id":value["id"],"error":"wrong interface"})
            } else {
                json!({"id":value["id"],"result":{"version":"v".repeat(120)}})
            };
            for bytes in response.to_string().as_bytes().chunks(MAX_CHUNK) {
                self.reads
                    .borrow_mut()
                    .push_back(raw(std::str::from_utf8(bytes).unwrap()));
            }
            output.clear();
        }
        Ok(report.len())
    }
    fn read_timeout(&self, report: &mut [u8], _: i32) -> Result<usize> {
        self.state.record(format!("read{}", self.candidate));
        self.state.stopping_at("read");
        self.state.stopping_at("next");
        self.state.time.set(self.state.time.get() + ms(1));
        let bytes = self.reads.borrow_mut().pop_front().unwrap_or_default();
        report[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

fn use_production_discovery(h: &mut Harness, mode: &'static str) -> Rc<ProbeState> {
    let state = Rc::new(ProbeState {
        stop: h.actor.endpoint.stop.clone(),
        mode,
        time: h.io.time.clone(),
        log: RefCell::new(vec![]),
        live: Cell::new(0),
        owner: std::thread::current().id(),
    });
    h.actor.factory = Box::new(HidFactory {
        native: ProbeDiscovery(state.clone()),
        stop: state.stop.clone(),
    });
    state
}

#[test]
fn creator_transport_fix_production_discovery_observes_stop_between_native_operations() {
    for mode in ["open", "enum", "open-error", "write", "read", "next"] {
        let mut h = Harness::new();
        let state = use_production_discovery(&mut h, mode);
        let exited = Arc::new(AtomicBool::new(false));
        let mut registry = Registry {
            lease: Some(Box::new(Joined(exited.clone()))),
        };
        h.display.start_transport().unwrap();
        assert!(!h.step());
        let log = state.log.borrow();
        let opens = log.iter().filter(|v| v.starts_with("open")).count();
        assert_eq!(
            opens,
            if mode == "enum" { 0 } else { 1 },
            "no next native candidate after stop: {mode}: {log:?}"
        );
        if matches!(mode, "enum" | "open" | "open-error") {
            assert!(
                !log.iter().any(|v| v.starts_with("write")),
                "no ordinary probe after stop: {mode}: {log:?}"
            );
        }
        if mode == "write" {
            assert!(
                !log.iter().any(|v| v.starts_with("read")),
                "no reply read after stopped send"
            );
        }
        if mode == "read" {
            assert_eq!(
                log.iter().filter(|v| v.starts_with("read")).count(),
                1,
                "stop after first fragmented reply forbids another read"
            );
        }
        assert!(!log.iter().any(|v| v.starts_with("rpc:device.status")));
        drop(log);
        assert_eq!(
            state.live.get(),
            0,
            "returned handle closes on the owner before discovery returns"
        );
        assert!(!registry.available());
        assert!(h.step());
        assert!(!registry.available());
        drop(h.actor);
        exited.store(true, Ordering::Release);
        assert!(registry.available());
    }
}

#[test]
fn creator_transport_fix_production_discovery_uninterrupted_candidate_probe_is_functional() {
    let mut h = Harness::new();
    let state = use_production_discovery(&mut h, "never");
    h.connect();
    h.poll();
    let log = state.log.borrow();
    let close = log.iter().position(|v| v == "close0").unwrap();
    let next = log.iter().position(|v| v == "open1").unwrap();
    assert!(close < next);
    let rpc: Vec<_> = log.iter().filter(|v| v.starts_with("rpc:")).collect();
    assert_eq!(rpc.len(), 3);
    assert!(rpc[0].starts_with("rpc:sys.version:"));
    assert!(rpc[1].starts_with("rpc:sys.version:"));
    assert!(rpc[2].starts_with("rpc:device.status:"));
    let ids: Vec<i64> = rpc
        .iter()
        .map(|v| v.rsplit(':').next().unwrap().parse().unwrap())
        .collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    drop(log);
    h.display.stop_transport();
    assert!(h.step());
    assert_eq!(state.live.get(), 0);
}

#[test]
fn creator_transport_fix_invalid_actions_never_release_known_down_quarantine() {
    for reconnect in [false, true] {
        let mut h = Harness::new();
        h.connect();
        h.poll();
        feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":1}}));
        h.step();
        h.poll();
        if reconnect {
            h.io.reads
                .borrow_mut()
                .push_back(Err(anyhow::anyhow!("lost while down")));
            h.step();
            h.poll();
            h.io.time.set(h.now() + Duration::from_secs(2));
            h.connect();
            h.poll();
        } else {
            h.io.time.set(h.now() + ms(51));
            h.step();
            h.poll();
        }
        for action in [
            json!(2),
            json!(9),
            json!(u64::MAX),
            json!(-1),
            json!(1.5),
            json!("0"),
            Value::Null,
        ] {
            feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":action}}));
            h.step();
            h.poll();
            h.step();
            let s = h.actor.endpoint.shared.lock().unwrap();
            assert_ne!(
                s.held & (1 << 10),
                0,
                "invalid action cannot prove release: {action}"
            );
            assert_ne!(
                s.quarantined & (1 << 10),
                0,
                "quarantine must survive: {action}"
            );
        }
        let req = request(&mut h, 1);
        assert_eq!(
            h.display.request_capture(req),
            CaptureRequestOutcome::Pending
        );
        h.step();
        assert_eq!(h.display.capture_evidence(req.id), None);
        h.io.time.set(h.now() + ms(11)); // retire the unsuccessful request before the positive rearm control
        feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":0}}));
        h.step();
        h.poll();
        h.step();
        let req = request(&mut h, 2);
        assert_eq!(
            h.display.request_capture(req),
            CaptureRequestOutcome::Pending
        );
        h.step();
        let e = h.display.capture_evidence(req.id).unwrap();
        assert!(matches!(
            h.display.try_claim_capture(e, h.now()),
            CaptureClaim::Claimed(_)
        ));
    }
}

#[test]
fn creator_transport_fix_delayed_loss_cannot_relabel_frontend_clear_debt() {
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    let mut h = Harness::new();
    h.connect();
    h.poll();
    h.display.publish(&plan).unwrap();
    feed(&h, json!({"m":"kb.pl","p":{"p":1,"l":6}}));
    h.step();
    h.poll();
    assert!(!h.display.clear_pending.is_empty());
    let old = h.display.transport_applied_epoch.unwrap();
    h.io.reads.borrow_mut().push_back(Err(anyhow::anyhow!(
        "lost before frontend published clears"
    )));
    h.step();
    h.io.time.set(h.now() + Duration::from_secs(2));
    h.connect(); // deliberately NO main poll until NEW status is Connected
    assert_eq!(
        h.display.diagnostics().connectivity,
        Connectivity::Connected
    );
    assert!(h.display.transport_epoch().unwrap().connection > old.connection);
    let batch = h.poll();
    assert!(
        batch
            .events
            .iter()
            .any(|e| matches!(e, DeviceEvent::Reset { .. }))
    );
    assert!(
        h.display.clear_pending.is_empty(),
        "old debt must retire from ordered loss/session transition, not latest Connected cache"
    );
    assert_ne!(h.display.transport_orphaned_clears, 0);
    h.display.publish(&plan).unwrap();
    assert_eq!(
        h.actor.endpoint.shared.lock().unwrap().clear_bits,
        0,
        "old-connection clear debt cannot be accepted by the new handle"
    );
    // Same-connection selector and gap Reset MUST keep a real owned clear union.
    let mut h = Harness::new();
    h.connect();
    h.poll();
    h.display.publish(&plan).unwrap();
    feed(&h, json!({"m":"kb.pl","p":{"p":1,"l":6}}));
    h.step();
    h.poll();
    h.io.time.set(h.now() + ms(51));
    h.step();
    h.poll();
    assert!(!h.display.clear_pending.is_empty());
    assert_eq!(h.display.transport_orphaned_clears, 0);
    h.display.publish(&plan).unwrap();
    assert_ne!(h.actor.endpoint.shared.lock().unwrap().clear_bits, 0);
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
pub(super) struct Fake {
    pub(super) time: Rc<Cell<Instant>>,
    pub(super) opens: Cell<usize>,
    pub(super) live: Cell<usize>,
    pub(super) open_pending: Cell<bool>,
    pub(super) write_pending: Cell<bool>,
    pub(super) close_pending: Cell<bool>,
    pub(super) short_write: Cell<bool>,
    pub(super) write_error: Cell<bool>,
    pub(super) auto_status: Cell<bool>,
    pub(super) status_selector: Cell<(u16, u16)>,
    pub(super) read_step: Cell<Duration>,
    pub(super) failed_writes: Cell<usize>,
    pub(super) write_reports: Cell<usize>,
    pub(super) stop_on_read: RefCell<Option<Endpoint>>,
    pub(super) reads: RefCell<VecDeque<Result<Vec<u8>>>>,
    pub(super) writes: RefCell<Vec<Value>>,
    pub(super) output: RefCell<Vec<u8>>,
    pub(super) context: RefCell<super::test_support::FakeContext>,
}
struct Device(Rc<Fake>);
impl Drop for Device {
    fn drop(&mut self) {
        self.0.live.set(self.0.live.get() - 1);
    }
}
impl ReportIo for Device {
    fn write(&self, report: &[u8]) -> Result<usize> {
        self.0.write_reports.set(self.0.write_reports.get() + 1);
        let mut output = self.0.output.borrow_mut();
        output.extend_from_slice(&report[3..3 + report[2] as usize]);
        if let Ok(v) = serde_json::from_slice::<Value>(&output) {
            if v["method"] == "device.status" && self.0.auto_status.get() {
                let (profile, layer) = self.0.status_selector.get();
                let text =
                    json!({"id":v["id"],"result":{"profile_index":profile,"layer_index":layer}})
                        .to_string();
                for bytes in text.as_bytes().chunks(MAX_CHUNK) {
                    self.0
                        .reads
                        .borrow_mut()
                        .push_back(Ok(raw(std::str::from_utf8(bytes).unwrap())));
                }
            }
            self.0.writes.borrow_mut().push(v);
            output.clear();
        }
        Ok(report.len())
    }
    fn read_timeout(&self, buf: &mut [u8], timeout: i32) -> Result<usize> {
        assert_eq!(timeout, 0);
        self.0.time.set(self.0.time.get() + self.0.read_step.get());
        if let Some(endpoint) = self.0.stop_on_read.borrow_mut().take() {
            endpoint.stop();
        }
        let bytes = self
            .0
            .reads
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(vec![]))?;
        buf[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
    fn actor_write(&self, report: &[u8]) -> Poll<Result<usize>> {
        if self.0.write_pending.get() {
            Poll::Pending
        } else if self.0.write_error.get() {
            self.0.failed_writes.set(self.0.failed_writes.get() + 1);
            Poll::Ready(Err(anyhow::anyhow!("fake output error")))
        } else if self.0.short_write.get() {
            Poll::Ready(Ok(report.len() - 1))
        } else {
            Poll::Ready(self.write(report))
        }
    }
    fn actor_close(&self) -> Poll<()> {
        if self.0.close_pending.get() {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}
fn raw(text: &str) -> Vec<u8> {
    assert!(text.len() <= MAX_CHUNK);
    let mut report = vec![0; REPORT_SIZE];
    report[..3].copy_from_slice(&[REPORT_ID, CHANNEL_RPC, text.len() as u8]);
    report[3..3 + text.len()].copy_from_slice(text.as_bytes());
    report
}
struct FakeFactory(Rc<Fake>, Option<super::test_support::ContextLease>);
impl FakeFactory {
    fn new(io: Rc<Fake>) -> Self {
        Self(io, None)
    }
}
impl Factory for FakeFactory {
    fn open(&mut self, ids: Arc<AtomicI64>) -> Poll<Result<Pad>> {
        if self.1.is_none() {
            self.1 = self.0.context.borrow().try_enter();
            if self.1.is_none() {
                return Poll::Pending;
            }
        }
        self.0.opens.set(self.0.opens.get() + 1);
        if self.0.open_pending.get() {
            return Poll::Pending;
        }
        assert_eq!(self.0.live.get(), 0, "exclusive handle");
        self.0.live.set(1);
        let time = self.0.time.clone();
        self.1.take();
        Poll::Ready(Ok(Pad {
            dev: Box::new(Device(self.0.clone())),
            next_id: 1,
            actor_ids: Some(ids),
            acc: String::new(),
            acc_times: VecDeque::new(),
            pending: VecDeque::new(),
            clock: Rc::new(move || time.get()),
            last_input_success: None,
            last_output_success: None,
            probe_stop: None,
        }))
    }
}
pub(super) struct ManualStarter(pub(super) Endpoint);
impl Starter for ManualStarter {
    fn start(&mut self) -> Result<(TransportStart, Option<Endpoint>)> {
        Ok((TransportStart::Started, Some(self.0.clone())))
    }
}
pub(super) struct Harness {
    pub(super) display: CreatorMicroDisplay,
    pub(super) actor: Actor,
    pub(super) io: Rc<Fake>,
}
impl Harness {
    pub(super) fn new() -> Self {
        let time = Rc::new(Cell::new(Instant::now()));
        let io = Rc::new(Fake {
            time: time.clone(),
            opens: Cell::new(0),
            live: Cell::new(0),
            open_pending: Cell::new(false),
            write_pending: Cell::new(false),
            close_pending: Cell::new(false),
            short_write: Cell::new(false),
            write_error: Cell::new(false),
            auto_status: Cell::new(true),
            status_selector: Cell::new((1, 3)),
            read_step: Cell::new(Duration::ZERO),
            failed_writes: Cell::new(0),
            write_reports: Cell::new(0),
            stop_on_read: RefCell::new(None),
            reads: RefCell::new(VecDeque::new()),
            writes: RefCell::new(vec![]),
            output: RefCell::new(vec![]),
            context: RefCell::new(super::test_support::FakeContext::new()),
        });
        let endpoint = Endpoint::new(7);
        let actor = Actor::new(
            endpoint.clone(),
            Box::new(FakeFactory::new(io.clone())),
            Arc::new(AtomicI64::new(1)),
        );
        let mut display = super::super::tests::configured();
        display.clock = Rc::new(move || time.get());
        display.transport_starter = Box::new(ManualStarter(endpoint));
        Self { display, actor, io }
    }
    fn now(&self) -> Instant {
        self.io.time.get()
    }
    fn step(&mut self) -> bool {
        self.actor.step(self.now())
    }
    fn connect(&mut self) {
        self.display.start_transport().unwrap();
        for _ in 0..20 {
            self.step();
            if self.display.diagnostics().connectivity == Connectivity::Connected {
                return;
            }
        }
        panic!("fake status did not establish connection");
    }
    fn poll(&mut self) -> InputBatch {
        self.display
            .poll_input_batch(
                InputPollMode::DrainOnly,
                InputPollBudget {
                    deadline: self.now() + ms(5),
                    max_raw_frames: 128,
                },
            )
            .unwrap()
    }
}

#[test]
fn creator_transport_blocked_open_is_lazy_and_main_stays_nonwaiting() {
    let mut h = Harness::new();
    assert_eq!(h.io.opens.get(), 0);
    h.io.open_pending.set(true);
    assert_eq!(
        h.display.start_transport().unwrap(),
        TransportStart::Started
    );
    for _ in 0..200 {
        h.step();
        assert_eq!(
            h.display.reconnect_step(h.now() + ms(5)).unwrap(),
            ReconnectOutcome::InProgress
        );
        assert_eq!(h.display.diagnostics().connectivity, Connectivity::Unknown);
        assert!(h.poll().events.is_empty());
        assert_eq!(
            h.display.start_transport().unwrap(),
            TransportStart::Running
        );
    }
    assert_eq!(h.io.live.get(), 0);
    h.io.open_pending.set(false);
    h.connect();
    assert_eq!(h.io.live.get(), 1);
    assert_eq!(h.display.diagnostics().layer.unwrap().position, Some(3));
    assert_eq!(
        h.display.reconnect_step(h.now() + ms(5)).unwrap(),
        ReconnectOutcome::Connected
    );
    assert!(matches!(
        h.poll().events.as_slice(),
        [DeviceEvent::Reset { .. }]
    ));
}

#[test]
fn creator_transport_complete_send_not_enqueue_or_optional_ack() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    let old = h.display.diagnostics().last_output_success;
    h.io.time.set(h.now() + ms(1));
    h.io.write_pending.set(true);
    assert_eq!(h.display.publish(&plan).unwrap(), PublishOutcome::Unchanged);
    for _ in 0..50 {
        h.step();
        h.poll();
        assert_eq!(h.display.diagnostics().last_output_success, old);
    }
    h.io.write_pending.set(false);
    for _ in 0..50 {
        h.step();
    }
    assert_eq!(h.display.diagnostics().last_output_success, Some(h.now()));
    assert_eq!(h.display.publish(&plan).unwrap(), PublishOutcome::Updated);
    let writes = h.io.writes.borrow();
    assert!(writes.iter().any(|v| v["method"] == "v.oai.rgbcfg"));
    assert!(writes.iter().any(|v| v["method"] == "v.oai.thstatus"));
    let ids: Vec<_> = writes.iter().map(|v| v["id"].as_i64().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn creator_transport_blocked_close_and_retirement_require_actual_exit() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    h.io.close_pending.set(true);
    let Harness {
        display,
        mut actor,
        io,
    } = h;
    drop(display);
    let finished = Arc::new(AtomicBool::new(false));
    let mut registry = Registry {
        lease: Some(Box::new(Joined(finished.clone()))),
    };
    for _ in 0..50 {
        assert!(!actor.step(io.time.get()));
        assert!(!registry.available());
        assert_eq!(io.live.get(), 1);
    }
    io.close_pending.set(false);
    assert!(
        actor.step(io.time.get()),
        "Drop must request actor-owned close"
    );
    assert_eq!(io.live.get(), 0);
    assert!(
        !registry.available(),
        "a stopping message is not actual exit"
    );
    drop(actor);
    finished.store(true, Ordering::Release);
    assert!(registry.available());
    let mut h = Harness::new();
    h.io.open_pending.set(true);
    h.display.start_transport().unwrap();
    h.step();
    h.display.stop_transport();
    assert!(!h.step(), "cannot cancel an in-flight native open");
    h.io.open_pending.set(false);
    for _ in 0..4 {
        if h.step() {
            assert_eq!(h.io.live.get(), 0);
            return;
        }
    }
    panic!("stop did not retire after in-flight open returned");
}

#[derive(Clone)]
struct Recorder(Rc<RefCell<Vec<bool>>>);
impl crate::switches::Emitter for Recorder {
    fn emit(&mut self, _: &crate::chord::Chord, down: bool) -> Result<()> {
        self.0.borrow_mut().push(down);
        Ok(())
    }
}
fn apply(engine: &mut crate::switches::Switches<Recorder>, batch: InputBatch, now: Instant) {
    for event in batch.events {
        match event {
            DeviceEvent::Key { address, pressed } => engine.event(&address, pressed, now).unwrap(),
            DeviceEvent::Reset { device } => engine.reset_device(&device).unwrap(),
        }
    }
}
fn key(pressed: bool) -> String {
    json!({"m":"v.oai.hid","p":{"act":u8::from(pressed),"k":"AG16"}}).to_string()
}

#[test]
fn creator_transport_mailbox_flood_preserves_mic_cleanup_and_clear_debt() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let calls = Rc::new(RefCell::new(vec![]));
    let mut engine =
        crate::switches::Switches::new(&[h.display.device.clone()], Recorder(calls.clone()))
            .unwrap();
    h.io.reads.borrow_mut().push_back(Ok(raw(&key(true))));
    h.step();
    apply(&mut engine, h.poll(), h.now());
    assert_eq!(*calls.borrow(), [true]);
    for _ in 0..300 {
        h.io.reads.borrow_mut().push_back(Ok(raw(&key(true))));
    }
    h.io.reads.borrow_mut().push_back(Ok(raw(&key(false))));
    for _ in 0..4 {
        h.step();
    }
    assert!(h.actor.endpoint.shared.lock().unwrap().events.len() <= 256);
    let first = h.poll();
    assert_eq!(
        first.continuity,
        InputContinuity::Lost,
        "overflow visible BEFORE prefix"
    );
    apply(&mut engine, first, h.now());
    for _ in 0..4 {
        h.step();
        let batch = h.poll();
        apply(&mut engine, batch, h.now());
    }
    assert_eq!(
        *calls.borrow(),
        [true, false],
        "Reset/release must clear owned microphone once"
    );
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.display.clear_pending.push(16);
    let endpoint = h.actor.endpoint.clone();
    let guard = endpoint.shared.lock().unwrap();
    assert_eq!(h.display.publish(&plan).unwrap(), PublishOutcome::Unchanged);
    assert_eq!(
        h.display.clear_pending,
        [16],
        "busy metadata must not discard clear debt"
    );
    drop(guard);
    h.display.publish(&plan).unwrap();
    assert!(h.display.clear_pending.is_empty());
    assert_ne!(h.actor.endpoint.shared.lock().unwrap().clear_bits, 0);
}

fn request(h: &mut Harness, seq: u64) -> CaptureRequest {
    CaptureRequest {
        id: CaptureId {
            actor: 7,
            sequence: seq,
        },
        epoch: h.display.transport_epoch().unwrap(),
        requested_at: h.now(),
        not_before: h.now(),
        expires_at: h.now() + ms(10),
    }
}
#[test]
fn creator_transport_partial_129_reset_needs_new_clean_evidence() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let req = request(&mut h, 1);
    assert_eq!(
        h.display.request_capture(req),
        CaptureRequestOutcome::Pending
    );
    h.io.reads
        .borrow_mut()
        .push_back(Ok(raw("{\"m\":\"kb.pl\",")));
    h.step();
    assert_eq!(h.display.capture_evidence(req.id), None);
    h.io.reads
        .borrow_mut()
        .push_back(Ok(raw("\"p\":{\"p\":1,\"l\":3}}")));
    h.step();
    h.poll();
    h.step();
    assert!(h.display.capture_evidence(req.id).is_some());
    h.display.release_capture(crate::display::CaptureToken {
        evidence: h.display.capture_evidence(req.id).unwrap(),
    });
    h.io.reads
        .borrow_mut()
        .extend((0..128).map(|_| Ok(raw("{\"id\":9999,\"result\":{}}"))));
    h.io.reads
        .borrow_mut()
        .push_back(Ok(raw("{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":6}}")));
    h.io.reads.borrow_mut().push_back(Ok(raw(&key(false))));
    let req = request(&mut h, 2);
    assert_eq!(
        h.display.request_capture(req),
        CaptureRequestOutcome::Pending
    );
    h.step();
    assert_eq!(h.io.reads.borrow().len(), 2);
    assert_eq!(h.display.capture_evidence(req.id), None);
    h.step();
    let batch = h.poll();
    assert!(matches!(
        batch.events.as_slice(),
        [
            DeviceEvent::Reset { .. },
            DeviceEvent::Key { pressed: false, .. }
        ]
    ));
    assert_eq!(h.display.capture_evidence(req.id), None);
    let req = request(&mut h, 3);
    assert_eq!(
        h.display.request_capture(req),
        CaptureRequestOutcome::Pending
    );
    h.step();
    let evidence = h.display.capture_evidence(req.id).unwrap();
    assert_eq!(evidence.observed_at, h.now());
    assert!(!evidence.partial_frame);
    assert!(!evidence.backlog);
    assert_eq!(evidence.continuity, InputContinuity::Continuous);
}

#[test]
fn creator_transport_capture_handoff_fires_real_timer_once_and_expires_without_main() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let mut device = h.display.device.clone();
    device.definition.actions.insert(
        "dictate".into(),
        crate::device::Action::TapHoldChords {
            tap_keys: vec!["enter".into()],
            hold_keys: vec!["shift".into(), "enter".into()],
            hold_ms: 300,
        },
    );
    let calls = Rc::new(RefCell::new(vec![]));
    let mut engine = crate::switches::Switches::new(&[device], Recorder(calls.clone())).unwrap();
    h.io.reads.borrow_mut().push_back(Ok(raw(&key(true))));
    h.step();
    apply(&mut engine, h.poll(), h.now());
    let deadline = engine.next_deadline().unwrap();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.display.publish(&plan).unwrap();
    for _ in 0..30 {
        h.io.time.set(h.now() + ms(10));
        h.step();
    }
    assert_eq!(h.now(), deadline);
    for _ in 0..30 {
        h.step();
    } // finish finite feedback; optional reply stays silent
    let req = request(&mut h, 1);
    assert_eq!(
        h.display.request_capture(req),
        CaptureRequestOutcome::Pending
    );
    h.step();
    let evidence = h.display.capture_evidence(req.id).unwrap();
    for _ in 0..100 {
        h.step();
        assert_eq!(
            h.display.capture_evidence(req.id),
            Some(evidence),
            "production Ready parks without a fake scheduling pause"
        );
    }
    let frontier = h.poll().event_frontier.unwrap();
    let CaptureClaim::Claimed(token) = h.display.try_claim_capture(evidence, h.now()) else {
        panic!("clean production handoff must be claimable");
    };
    h.step(); // Claimed also parks HID
    assert_eq!(
        h.display.consume_capture(token, frontier, h.now()),
        CaptureConsume::Consumed
    );
    let trusted = BTreeSet::from(["creator-micro".to_string()]);
    engine.advance(evidence.observed_at, &trusted);
    assert_eq!(
        *calls.borrow(),
        [true, false],
        "actual timer emitted one hold pulse"
    );
    assert!(matches!(
        h.display.consume_capture(token, frontier, h.now()),
        CaptureConsume::Rejected(_)
    ));
    h.display.release_capture(token);
    h.step();
    let req = request(&mut h, 2);
    h.display.request_capture(req);
    h.step();
    let old = h.display.capture_evidence(req.id).unwrap();
    h.io.time.set(h.now() + ms(10));
    h.step();
    assert_eq!(h.display.capture_evidence(req.id), None);
    assert!(matches!(
        h.display.try_claim_capture(old, h.now()),
        CaptureClaim::Rejected(_)
    ));
    let req = request(&mut h, 3);
    assert_eq!(
        h.display.request_capture(req),
        CaptureRequestOutcome::Pending
    );
    h.step();
    let e = h.display.capture_evidence(req.id).unwrap();
    let CaptureClaim::Claimed(token) = h.display.try_claim_capture(e, h.now()) else {
        panic!("claim");
    };
    // New relevant main command invalidates a parked token before native work.
    h.display
        .publish_feedback(
            &plan,
            &BTreeMap::from([(
                Address::try_from("creator-micro:rex/16".to_string()).unwrap(),
                Treatment {
                    color: Rgb {
                        red: 255,
                        green: 0,
                        blue: 0,
                    },
                    brightness: 1.0,
                    motion: Motion::Steady,
                },
            )]),
        )
        .unwrap();
    assert!(matches!(
        h.display.consume_capture(token, frontier, h.now()),
        CaptureConsume::Rejected(_)
    ));
}

#[test]
fn creator_transport_reconnect_identity_ids_and_counter_failure_are_closed() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let old = h.display.transport_epoch().unwrap();
    h.display.clear_pending.push(16);
    h.io.reads
        .borrow_mut()
        .push_back(Err(anyhow::anyhow!("fake unplug")));
    h.step();
    assert!(matches!(
        h.poll().events.as_slice(),
        [DeviceEvent::Reset { .. }]
    ));
    assert!(
        h.display.clear_pending.is_empty(),
        "old-identity unsent clears cannot cross reconnect"
    );
    h.io.time.set(h.now() + Duration::from_secs(2));
    h.connect();
    h.poll();
    let new = h.display.transport_epoch().unwrap();
    assert!(new.connection > old.connection);
    assert_eq!(new.actor, old.actor);
    assert_eq!(h.io.live.get(), 1);
    let ids: Vec<_> =
        h.io.writes
            .borrow()
            .iter()
            .map(|v| v["id"].as_i64().unwrap())
            .collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    let stale = CaptureRequest {
        epoch: old,
        ..request(&mut h, 1)
    };
    assert_eq!(
        h.display.request_capture(stale),
        CaptureRequestOutcome::Rejected(CaptureReject::Stale)
    );
    let mut h = Harness::new();
    h.actor.ids.store(i64::MAX, Ordering::Release);
    h.display.start_transport().unwrap();
    h.step();
    assert!(h.io.writes.borrow().is_empty());
    assert!(h.actor.endpoint.stop.load(Ordering::Acquire));
    let mut h = Harness::new();
    h.connect();
    h.poll();
    h.actor.endpoint.shared.lock().unwrap().frontier = u64::MAX;
    h.io.reads.borrow_mut().push_back(Ok(raw(&key(true))));
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.step())).is_ok(),
        "counter overflow is typed fail-closed, not a panic"
    );
    assert!(h.actor.endpoint.stop.load(Ordering::Acquire));
    assert!(
        h.display
            .capture_evidence(CaptureId {
                actor: 7,
                sequence: 1
            })
            .is_none()
    );
}

#[test]
fn creator_transport_order_coalescing_native_preservation_and_clear_uncertainty() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.display.publish(&plan).unwrap();
    for _ in 0..50 {
        h.step();
    }
    h.display.options.effect_speed = 0.73;
    h.io.reads
        .borrow_mut()
        .push_back(Ok(raw("{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":2}}")));
    h.step();
    h.poll();
    let begin = h.io.writes.borrow().len();
    h.display.publish(&plan).unwrap();
    for _ in 0..50 {
        h.step();
    }
    let writes = h.io.writes.borrow();
    assert!(
        writes[begin..]
            .iter()
            .all(|v| v["method"] == "v.oai.thstatus"),
        "native selector never receives rgbcfg"
    );
    let clear = writes[begin]["params"].as_array().unwrap();
    assert!(!clear.is_empty());
    assert!(
        clear.iter().all(|e| e["e"] == 0
            && e["b"] == 0.0
            && (6..=18).contains(&e["id"].as_u64().unwrap())
            && (e["s"].as_f64().unwrap() - 0.73).abs() < 0.000001),
        "clear payload preserves configured speed/ownership"
    );
    drop(writes);
    h.display
        .options
        .layers
        .get_mut("test")
        .unwrap()
        .zone_lighting = ZoneLighting::Firmware;
    h.io.reads
        .borrow_mut()
        .push_back(Ok(raw("{\"m\":\"kb.pl\",\"p\":{\"p\":1,\"l\":6}}")));
    h.step();
    h.poll();
    let begin = h.io.writes.borrow().len();
    let address = Address::try_from("creator-micro:test/16".to_string()).unwrap();
    for brightness in [0.2, 0.4, 0.8] {
        h.display
            .publish_feedback(
                &plan,
                &BTreeMap::from([(
                    address.clone(),
                    Treatment {
                        color: Rgb {
                            red: 255,
                            green: 0,
                            blue: 0,
                        },
                        brightness,
                        motion: Motion::Steady,
                    },
                )]),
            )
            .unwrap();
    }
    for _ in 0..50 {
        h.step();
    }
    let writes = h.io.writes.borrow();
    assert_eq!(
        writes.len() - begin,
        1,
        "latest desired coalesced, no native palette"
    );
    let entries = writes[begin]["params"].as_array().unwrap();
    assert!(
        (entries.iter().find(|v| v["id"] == 16).unwrap()["b"]
            .as_f64()
            .unwrap()
            - 0.8)
            .abs()
            < 0.000001
    );
    drop(writes);
    // A failed clear is uncertain, not blindly put back into the unsent queue.
    h.display.clear_pending.push(16);
    h.display.publish(&plan).unwrap();
    let old = h.display.diagnostics().last_output_success;
    h.io.short_write.set(true);
    h.step();
    let s = h.actor.endpoint.shared.lock().unwrap();
    assert_eq!(s.clear_bits, 0);
    assert_ne!(s.uncertain_clears & (1 << 10), 0);
    drop(s);
    assert_eq!(h.display.diagnostics().last_output_success, old);
    assert_eq!(
        h.display.diagnostics().last_failure.unwrap().code,
        DiagnosticCode::OutputFailed
    );
}

#[test]
fn creator_transport_never_falls_back_to_legacy_factory_and_stop_is_not_healthy() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    assert!(
        h.display
            .connect_with(|| panic!("legacy open must not coexist with actor"))
            .unwrap()
    );
    h.display.stop_transport();
    assert_ne!(
        h.display.diagnostics().connectivity,
        Connectivity::Connected
    );
    assert_eq!(
        h.display.start_transport().unwrap(),
        TransportStart::Retiring
    );
}

fn feed(h: &Harness, v: Value) {
    for chunk in v.to_string().as_bytes().chunks(MAX_CHUNK) {
        h.io.reads
            .borrow_mut()
            .push_back(Ok(raw(std::str::from_utf8(chunk).unwrap())));
    }
}
#[test]
fn creator_transport_correlated_responses_do_not_confuse_pending_with_success() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.display.publish(&plan).unwrap();
    for _ in 0..50 {
        h.step();
    }
    let feedback = BTreeMap::from([(
        Address::try_from("creator-micro:rex/16".to_string()).unwrap(),
        Treatment {
            color: Rgb {
                red: 255,
                green: 0,
                blue: 0,
            },
            brightness: 1.0,
            motion: Motion::Steady,
        },
    )]);
    assert_eq!(
        h.display.publish_feedback(&plan, &feedback).unwrap(),
        PublishOutcome::Unchanged,
        "previous completion cannot report NEW enqueue as Updated"
    );
    for _ in 0..50 {
        h.step();
    }
    let send_at = h.display.diagnostics().last_output_success;
    let id = h.io.writes.borrow().last().unwrap()["id"].clone();
    h.io.time.set(h.now() + ms(1));
    let reply_at = h.now();
    for v in [
        json!({"id":999999,"result":{"ok":1}}),
        json!({"id":id,"method":"wrong","error":"wrong method"}),
        json!({"id":id}),
        json!({"id":id,"result":{"ok":true}}),
    ] {
        feed(&h, v);
        h.step();
    }
    assert!(h.actor.optional.back().unwrap().confirmed_at.is_none());
    feed(&h, json!({"id":id,"result":{"ok":1}}));
    h.step();
    assert_eq!(
        h.actor.optional.back().unwrap().confirmed_at,
        Some(reply_at)
    );
    assert_eq!(h.display.diagnostics().last_output_success, send_at);
    feed(&h, json!({"id":id,"error":{"code":-1}}));
    h.step();
    assert_eq!(
        h.display.diagnostics().last_failure.unwrap().code,
        DiagnosticCode::OutputFailed
    );
    feed(&h, json!({"id":id,"result":{"ok":1}}));
    h.step();
    assert!(
        h.display.diagnostics().last_failure.unwrap().active,
        "optional reply cannot erase actual rejection"
    );
    // Required status has its own response contract, original time and timeout.
    for mode in [
        "wrong-then-valid",
        "error",
        "missing",
        "invalid",
        "timeout",
        "short",
        "send-error",
    ] {
        let mut h = Harness::new();
        h.io.auto_status.set(false);
        h.io.short_write.set(mode == "short");
        h.io.write_error.set(mode == "send-error");
        h.display.start_transport().unwrap();
        h.step();
        if mode != "short" && mode != "send-error" {
            let id = h.io.writes.borrow()[0]["id"].clone();
            match mode {
                "wrong-then-valid" => {
                    feed(
                        &h,
                        json!({"id":9999,"result":{"profile_index":1,"layer_index":9}}),
                    );
                    feed(
                        &h,
                        json!({"id":id,"result":{"profile_index":1,"layer_index":3}}),
                    );
                }
                "error" => feed(&h, json!({"id":id,"error":"rejected"})),
                "missing" => feed(&h, json!({"id":id})),
                "invalid" => feed(
                    &h,
                    json!({"id":id,"result":{"profile_index":1,"layer_index":0}}),
                ),
                _ => h.io.time.set(h.now() + ms(700)),
            }
            h.step();
        }
        let observation = h.display.diagnostics();
        if mode == "wrong-then-valid" {
            assert_eq!(observation.layer.unwrap().position, Some(3));
        } else {
            assert_eq!(
                observation.last_failure.unwrap().code,
                if mode == "short" || mode == "send-error" {
                    DiagnosticCode::OutputFailed
                } else {
                    DiagnosticCode::InputFailed
                }
            );
        }
    }
}

#[test]
fn creator_transport_capture_counts_raw_work_and_rejects_unapplied_or_forged_frontier() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let req = request(&mut h, 1);
    assert_eq!(
        h.display.request_capture(req),
        CaptureRequestOutcome::Pending
    );
    feed(&h, json!({"id":9999,"result":{}}));
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":0}}));
    h.step();
    let e = h.display.capture_evidence(req.id).unwrap();
    assert_eq!(e.raw_frames, 2, "all raw reports, even ignored ACKs, count");
    let CaptureClaim::Claimed(token) = h.display.try_claim_capture(e, h.now()) else {
        panic!("claim");
    };
    assert_eq!(
        h.display.consume_capture(token, 0, h.now()),
        CaptureConsume::Rejected(CaptureReject::Stale)
    );
    assert_eq!(
        h.display.consume_capture(token, u64::MAX, h.now()),
        CaptureConsume::Rejected(CaptureReject::Stale)
    );
    let frontier = h.poll().event_frontier.unwrap();
    assert_eq!(
        h.display.consume_capture(token, frontier, h.now()),
        CaptureConsume::Consumed
    );
    h.display.release_capture(token);
    let req = request(&mut h, 2);
    h.display.request_capture(req);
    h.step();
    let e = h.display.capture_evidence(req.id).unwrap();
    let forged = CaptureEvidence {
        command_revision: e.command_revision + 1,
        ..e
    };
    assert_eq!(
        h.display.try_claim_capture(forged, h.now()),
        CaptureClaim::Rejected(CaptureReject::Stale)
    );
    let CaptureClaim::Claimed(token) = h.display.try_claim_capture(e, h.now()) else {
        panic!("claim");
    };
    h.io.time.set(h.now() + ms(10));
    h.step();
    assert!(matches!(
        h.display.consume_capture(token, frontier, h.now()),
        CaptureConsume::Rejected(_)
    ));
    let bad = CaptureRequest {
        not_before: h.now() - ms(1),
        ..request(&mut h, 3)
    };
    assert_eq!(
        h.display.request_capture(bad),
        CaptureRequestOutcome::Rejected(CaptureReject::Stale)
    );
}

#[test]
fn creator_transport_continuity_gap_cancels_pending_and_requires_release_before_rearm() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":1}}));
    h.step();
    h.poll();
    h.io.time.set(h.now() + ms(51));
    let req = request(&mut h, 1);
    h.display.request_capture(req);
    h.step();
    h.step();
    assert_eq!(
        h.display.capture_evidence(req.id),
        None,
        "two rapid empty reads cannot hide a >50ms sampling gap"
    );
    assert!(
        h.poll()
            .events
            .iter()
            .any(|e| matches!(e, DeviceEvent::Reset { .. }))
    );
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":0}}));
    h.step();
    h.poll();
    h.step();
    let req = request(&mut h, 2);
    assert_eq!(
        h.display.request_capture(req),
        CaptureRequestOutcome::Pending
    );
    h.step();
    let e = h.display.capture_evidence(req.id).unwrap();
    assert!(matches!(
        h.display.try_claim_capture(e, h.now()),
        CaptureClaim::Claimed(_)
    ));
}

#[test]
fn creator_transport_expired_main_budget_and_actor_read_overrun_never_grant_proof() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":0}}));
    h.step();
    let batch = h
        .display
        .poll_input_batch(
            InputPollMode::DrainOnly,
            InputPollBudget {
                deadline: h.now(),
                max_raw_frames: 128,
            },
        )
        .unwrap();
    assert!(batch.events.is_empty());
    assert_eq!(batch.event_frontier, None);
    assert_eq!(
        h.poll().events.len(),
        1,
        "expired poll cannot consume a queued release"
    );
    let req = request(&mut h, 1);
    h.display.request_capture(req);
    h.io.read_step.set(ms(6));
    h.step();
    assert_eq!(
        h.display.capture_evidence(req.id),
        None,
        "empty observation after actor slice deadline is not a fence"
    );
}

#[test]
fn creator_transport_failed_inflight_write_is_not_replayed_during_close() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.io.write_pending.set(true);
    h.display.clear_pending.push(16);
    h.display.publish(&plan).unwrap();
    h.step();
    h.io.write_pending.set(false);
    h.io.write_error.set(true);
    h.step();
    h.step();
    assert_eq!(
        h.io.failed_writes.get(),
        1,
        "a returned error is not an outstanding native call to retry"
    );
    assert_eq!(h.io.live.get(), 0);
}

#[test]
fn creator_transport_shutdown_services_owed_clear_before_owner_close() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.display.clear_pending.push(16);
    h.display.publish(&plan).unwrap();
    h.display.stop_transport();
    for _ in 0..32 {
        if h.step() {
            break;
        }
    }
    assert_eq!(h.io.live.get(), 0);
    assert!(
        h.io.writes
            .borrow()
            .iter()
            .any(|v| v["method"] == "v.oai.thstatus"
                && v["params"][0]["id"] == 16
                && v["params"][0]["e"] == 0),
        "best-effort retirement clear precedes close, no repaint"
    );
    assert!(
        !h.io
            .writes
            .borrow()
            .iter()
            .any(|v| v["method"] == "v.oai.rgbcfg")
    );
}

#[test]
fn creator_transport_factory_probe_parser_is_bounded_too() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    for _ in 0..1100 {
        h.io.reads
            .borrow_mut()
            .push_back(Ok(raw(&"{".repeat(MAX_CHUNK))));
        if h.actor.pad.as_mut().unwrap().read_step_timeout(0).is_err() {
            assert!(h.actor.pad.as_ref().unwrap().acc.len() <= ACC_CAP + MAX_CHUNK);
            return;
        }
    }
    panic!(
        "factory/probe Pad parser must enforce actor accumulator bound before actor loop starts"
    );
}

#[test]
fn creator_transport_stop_during_open_cannot_start_a_new_status_write() {
    struct StopOnOpen {
        inner: FakeFactory,
        endpoint: Endpoint,
    }
    impl Factory for StopOnOpen {
        fn open(&mut self, ids: Arc<AtomicI64>) -> Poll<Result<Pad>> {
            let result = self.inner.open(ids);
            self.endpoint.stop();
            result
        }
    }
    let mut h = Harness::new();
    h.actor.factory = Box::new(StopOnOpen {
        inner: FakeFactory::new(h.io.clone()),
        endpoint: h.actor.endpoint.clone(),
    });
    h.display.start_transport().unwrap();
    h.step();
    assert!(
        h.io.writes.borrow().is_empty(),
        "stop during native open retires returned handle before a new RPC"
    );
    assert!(h.step());
    assert_eq!(h.io.live.get(), 0);
}

#[test]
fn creator_transport_backlog_and_new_selector_cannot_publish_old_zone_frame() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.display.publish(&plan).unwrap();
    h.io.reads
        .borrow_mut()
        .extend((0..128).map(|_| Ok(raw("{\"id\":9999,\"result\":{}}"))));
    feed(&h, json!({"m":"kb.pl","p":{"p":1,"l":2}}));
    let sent = h.io.write_reports.get();
    h.step();
    assert_eq!(h.io.reads.borrow().len(), 1);
    assert_eq!(
        h.io.write_reports.get(),
        sent,
        "even the first raw output fragment must wait for an exhausted input slice"
    );
    assert_eq!(
        h.io.writes.borrow().len(),
        1,
        "do not start feedback RPC while raw backlog may hide a selector"
    );
    for _ in 0..20 {
        h.step();
    }
    assert_eq!(
        h.io.write_reports.get(),
        sent,
        "no fragment may re-tag an old palette into a native selector"
    );
    assert_eq!(
        h.io.writes.borrow().len(),
        1,
        "new selector cannot re-tag old desired palette as current"
    );
    h.poll();
    h.display.publish(&plan).unwrap();
    for _ in 0..30 {
        h.step();
    }
    assert!(
        h.io.writes
            .borrow()
            .iter()
            .all(|v| v["method"] != "v.oai.rgbcfg")
    );
}

#[test]
fn creator_transport_stop_during_read_prevents_next_feedback_rpc() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    h.display.publish(&plan).unwrap();
    *h.io.stop_on_read.borrow_mut() = Some(h.actor.endpoint.clone());
    let sent = h.io.write_reports.get();
    h.step();
    assert_eq!(
        h.io.write_reports.get(),
        sent,
        "stop observed during a read prevents a new feedback RPC"
    );
}

#[test]
fn creator_transport_reconnect_cannot_rearm_a_known_down_without_release_evidence() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":1}}));
    h.step();
    h.poll();
    h.io.reads
        .borrow_mut()
        .push_back(Err(anyhow::anyhow!("lost while down")));
    h.step();
    h.poll();
    h.io.time.set(h.now() + Duration::from_secs(2));
    h.connect();
    h.poll();
    let req = request(&mut h, 1);
    h.display.request_capture(req);
    h.step();
    assert_eq!(
        h.display.capture_evidence(req.id),
        None,
        "new status alone is not required member-release evidence"
    );
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":0}}));
    h.step();
    h.poll();
    h.step();
    assert!(h.display.capture_evidence(req.id).is_some());
}

#[test]
fn creator_transport_original_observation_cutoff_covers_only_the_first_deadline() {
    let mut h = Harness::new();
    h.connect();
    h.poll();
    let mut device = h.display.device.clone();
    device.definition.actions.insert(
        "dictate".into(),
        crate::device::Action::TapHoldChords {
            tap_keys: vec!["enter".into()],
            hold_keys: vec!["shift".into(), "enter".into()],
            hold_ms: 300,
        },
    );
    let mut second = device.definition.keys["key11"].clone();
    second.members = vec![Address::try_from("creator-micro:rex/17".to_string()).unwrap()];
    device.definition.keys.get_mut("key11").unwrap().members =
        vec![Address::try_from("creator-micro:rex/16".to_string()).unwrap()];
    device.definition.keys.insert("send2".into(), second);
    h.display.device = device.clone();
    let calls = Rc::new(RefCell::new(vec![]));
    let mut engine = crate::switches::Switches::new(&[device], Recorder(calls.clone())).unwrap();
    let start = h.now();
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG16","act":1}}));
    h.step();
    apply(&mut engine, h.poll(), h.now());
    h.io.time.set(start + ms(5));
    feed(&h, json!({"m":"v.oai.hid","p":{"k":"AG17","act":1}}));
    h.step();
    apply(&mut engine, h.poll(), h.now());
    for n in 2..=60 {
        h.io.time.set(start + ms(n * 5));
        h.step();
    }
    let req = request(&mut h, 1);
    h.display.request_capture(req);
    h.step();
    let e = h.display.capture_evidence(req.id).unwrap();
    h.io.time.set(start + ms(306));
    let frontier = h.poll().event_frontier.unwrap();
    let CaptureClaim::Claimed(token) = h.display.try_claim_capture(e, h.now()) else {
        panic!("claim");
    };
    assert_eq!(
        h.display.consume_capture(token, frontier, h.now()),
        CaptureConsume::Consumed
    );
    engine.advance(
        e.observed_at,
        &BTreeSet::from(["creator-micro".to_string()]),
    );
    assert_eq!(
        *calls.borrow(),
        [true, false],
        "consumed original observation at300 must not fire deadline305 at receipt306"
    );
    assert_eq!(engine.next_deadline(), Some(start + ms(305)));
    h.display.release_capture(token);
    h.step();
    let req = request(&mut h, 2);
    h.display.request_capture(req);
    h.step();
    let e = h.display.capture_evidence(req.id).unwrap();
    let CaptureClaim::Claimed(token) = h.display.try_claim_capture(e, h.now()) else {
        panic!("claim");
    };
    assert_eq!(
        h.display.consume_capture(token, frontier, h.now()),
        CaptureConsume::Consumed
    );
    engine.advance(
        e.observed_at,
        &BTreeSet::from(["creator-micro".to_string()]),
    );
    assert_eq!(*calls.borrow(), [true, false, true, false]);
    h.display.release_capture(token);
}

#[test]
fn creator_transport_batches_expose_real_continuity_and_partial_state_without_drain_authority() {
    let mut h = Harness::new();
    h.connect();
    assert_eq!(h.poll().continuity, InputContinuity::Reset);
    h.step();
    let clean = h.poll();
    assert_eq!(clean.continuity, InputContinuity::Continuous);
    assert!(!clean.drained);
    h.io.reads
        .borrow_mut()
        .push_back(Ok(raw("{\"m\":\"kb.pl\",")));
    h.step();
    let partial = h.poll();
    assert!(partial.partial_frame);
    assert!(!partial.drained);
    h.io.reads
        .borrow_mut()
        .push_back(Ok(raw("\"p\":{\"p\":1,\"l\":3}}")));
    h.step();
    assert!(!h.poll().partial_frame);
    h.io.reads.borrow_mut().push_back(Ok(raw(&key(false))));
    h.io.reads.borrow_mut().push_back(Ok(raw("{oops}")));
    h.step();
    let loss = h.poll();
    assert_eq!(loss.continuity, InputContinuity::Lost);
    assert!(matches!(
        loss.events.as_slice(),
        [
            DeviceEvent::Key { pressed: false, .. },
            DeviceEvent::Reset { .. }
        ]
    ));
}
