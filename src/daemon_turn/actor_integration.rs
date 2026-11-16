//! Backend-owned LIB unit tests: the owner fixture steps the production Actor,
//! and every admission goes through the actual CreatorMicroDisplay/controller.
use super::*;
use crate::{
    chord::Chord,
    creator_micro::test_support::{
        CreatorDriver, FakeClock, FakeContext, Gate, creator, device_config,
    },
    display::{Connectivity, DisplaySink, TransportStart},
    render::{DisplayConfig, RenderPlan},
};
use std::{cell::RefCell, rc::Rc};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}
#[derive(Clone)]
struct Poster(Rc<RefCell<Vec<(Chord, bool)>>>);
impl Emitter for Poster {
    fn emit(&mut self, chord: &Chord, down: bool) -> anyhow::Result<()> {
        self.0.borrow_mut().push((chord.clone(), down));
        Ok(())
    }
}
fn timed_config(two: bool) -> DeviceConfig {
    let mut config = device_config();
    let key = config.definition.keys.get_mut("key11").unwrap();
    key.members = vec!["creator-micro:rex/16".to_string().try_into().unwrap()];
    let mut second = key.clone();
    second.members = vec!["creator-micro:rex/17".to_string().try_into().unwrap()];
    if two {
        config.definition.keys.insert("second".into(), second);
    }
    config.definition.actions.insert(
        "dictate".into(),
        Action::TapHoldChords {
            tap_keys: vec!["enter".into()],
            hold_keys: vec!["shift".into(), "enter".into()],
            hold_ms: 300,
        },
    );
    config
}
struct Rig {
    owner: CreatorDriver,
    clock: FakeClock,
    displays: DisplaySet,
    turns: TurnController,
    engine: Switches<Poster>,
    calls: Rc<RefCell<Vec<(Chord, bool)>>>,
    plan: RenderPlan,
}
impl Rig {
    fn new(config: DeviceConfig, at: Instant, context: FakeContext) -> Self {
        Self::with_sinks(config, at, context, vec![])
    }
    fn with_sinks(
        config: DeviceConfig,
        at: Instant,
        context: FakeContext,
        mut extras: Vec<Box<dyn DisplaySink>>,
    ) -> Self {
        let (display, owner) = creator(config.clone(), at, context).unwrap();
        let clock = owner.clock();
        let calls = Rc::new(RefCell::new(Vec::new()));
        extras.insert(0, Box::new(display));
        Self {
            owner,
            clock,
            displays: DisplaySet::new(extras),
            turns: TurnController::new(std::slice::from_ref(&config)),
            engine: Switches::new(&[config], Poster(calls.clone())).unwrap(),
            calls,
            plan: crate::render::render(&DisplayConfig::default(), &[]).unwrap(),
        }
    }
    fn start(&mut self) {
        assert_eq!(self.owner.snapshot().live_handles, 0);
        assert!(matches!(
            self.displays.start_transports()[0].1,
            Ok(TransportStart::Started)
        ));
    }
    fn main(&mut self) -> TurnOutcome {
        self.main_navigation(false)
    }
    fn main_navigation(&mut self, navigation: bool) -> TurnOutcome {
        let now = self.clock.now();
        let result = self.turns.service_transports(
            &mut self.engine,
            &mut self.displays,
            vec![],
            InputPollBudget {
                deadline: now + ms(5),
                max_raw_frames: 128,
            },
            || now,
            navigation,
        );
        self.displays
            .publish_feedback(&self.plan, &self.engine.treatments());
        self.turns
            .request_captures(&mut self.engine, &mut self.displays, now);
        result
    }
    fn cycle(&mut self, at: Instant) {
        self.clock.set(at);
        for _ in 0..6 {
            self.owner.step();
            self.main();
        }
    }
    fn warm(&mut self, zero: Instant) {
        self.start();
        for delta in [20, 15, 10, 5] {
            self.cycle(zero - ms(delta));
        }
        assert_eq!(
            self.displays.diagnostics()["creator-micro"].connectivity,
            Connectivity::Connected
        );
        assert_eq!(self.owner.snapshot().live_handles, 1);
        assert!(self.turns.admitted.contains("creator-micro"));
    }
    fn downs(&self) -> usize {
        self.calls.borrow().iter().filter(|(_, down)| *down).count()
    }
}

#[test]
fn daemon_turn_actor_barrier_real_actor_release_reset_flood_loss_and_watchdog_cleanup() {
    for cause in ["release", "selector", "flood", "loss", "watchdog"] {
        let zero = Instant::now() + ms(20);
        let mut rig = Rig::new(device_config(), zero - ms(20), FakeContext::new());
        rig.warm(zero);
        rig.owner.push_key(16, true);
        rig.cycle(zero);
        assert_eq!(rig.downs(), 1, "positive microphone admission: {cause}");
        assert!(rig.engine.output_down());
        rig.clock.set(zero + ms(5));
        match cause {
            "release" => rig.owner.push_key(16, false),
            "selector" => rig.owner.push_selector(1, 6),
            "loss" => rig.owner.fail_read(),
            "flood" => {
                for _ in 0..300 {
                    rig.owner.push_key(6, true);
                }
                rig.owner.push_key(16, false);
            }
            _ => {
                rig.clock.set(zero + ms(51));
            }
        }
        if cause == "watchdog" {
            rig.main();
        } else {
            for _ in 0..12 {
                rig.owner.step();
            }
            rig.main();
            for _ in 0..12 {
                rig.owner.step();
                rig.main();
            }
        }
        assert!(
            !rig.engine.output_down(),
            "owned cleanup must precede new trust: {cause}"
        );
        assert_eq!(
            rig.calls
                .borrow()
                .iter()
                .map(|(_, down)| *down)
                .collect::<Vec<_>>(),
            [true, false],
            "{cause}"
        );
        // A selector/fault may retire a handle with an uncertain in-flight frame.
        // Cleanup must not depend on keeping that handle alive or reopening it.
        assert!(
            rig.owner.snapshot().live_handles <= 1,
            "{cause}: no duplicate owner"
        );
        if cause == "loss" {
            assert_eq!(rig.owner.snapshot().live_handles, 0);
        }
    }
}

#[test]
fn daemon_turn_actor_barrier_real_actor_expired_capture_cancels_without_replaying_gesture() {
    let zero = Instant::now() + ms(20);
    let mut rig = Rig::new(timed_config(false), zero - ms(20), FakeContext::new());
    rig.warm(zero);
    rig.owner.push_key(16, true);
    rig.cycle(zero);
    for time in (5..=295).step_by(5) {
        rig.cycle(zero + ms(time));
    }
    rig.clock.set(zero + ms(300));
    rig.owner.step();
    let request = rig.turns.requests["creator-micro"];
    let evidence = rig
        .displays
        .capture_evidence("creator-micro", request.id)
        .unwrap();
    rig.clock.set(evidence.lease_until);
    rig.main();
    rig.owner.step();
    for time in (315..=350).step_by(5) {
        rig.cycle(zero + ms(time));
    }
    assert_eq!(
        rig.downs(),
        0,
        "expired capture cannot be retimed or replayed"
    );
    rig.owner.push_key(16, false);
    rig.cycle(zero + ms(355));
    rig.owner.push_key(16, true);
    rig.cycle(zero + ms(360));
    rig.owner.push_key(16, false);
    rig.cycle(zero + ms(365));
    assert_eq!(rig.downs(), 1, "fresh released/rearmed gesture still works");
}

#[test]
fn daemon_turn_actor_barrier_real_actor_threshold_release_reset_and_flood_precede_hold() {
    for cause in ["release", "selector", "flood"] {
        let zero = Instant::now() + ms(20);
        let mut rig = Rig::new(timed_config(false), zero - ms(20), FakeContext::new());
        rig.warm(zero);
        rig.owner.push_key(16, true);
        rig.cycle(zero);
        for time in (5..=295).step_by(5) {
            rig.cycle(zero + ms(time));
        }
        rig.clock.set(zero + ms(300));
        match cause {
            "release" => rig.owner.push_key(16, false),
            "selector" => rig.owner.push_selector(1, 6),
            _ => {
                for _ in 0..300 {
                    rig.owner.push_key(6, true);
                }
                rig.owner.push_key(16, false);
            }
        }
        for _ in 0..12 {
            rig.owner.step();
            rig.main();
        }
        assert_eq!(
            rig.downs(),
            0,
            "observable {cause} wins over threshold; no substituted tap/hold"
        );
    }
}

#[test]
fn daemon_turn_actor_barrier_real_actor_blocked_open_write_close_remains_single_owner() {
    let start = Instant::now();
    let mut rig = Rig::new(device_config(), start, FakeContext::new());
    rig.owner.set_gate(Gate::Open, true);
    rig.start();
    rig.owner.step();
    for time in 0..20 {
        rig.clock.set(start + ms(time));
        rig.main();
    }
    assert_eq!(rig.owner.snapshot().live_handles, 0);
    assert_eq!(
        rig.owner.snapshot().open_polls,
        1,
        "main cannot poll/open native I/O"
    );
    rig.owner.set_gate(Gate::Open, false);
    rig.owner.step();
    rig.owner.step();
    rig.cycle(start + ms(20));
    rig.owner.set_gate(Gate::Write, true);
    rig.plan.keys[0].state = Some(crate::render::AgentState::Working);
    rig.main();
    rig.owner.step();
    let writes = rig.owner.snapshot().write_reports;
    for time in 21..40 {
        rig.clock.set(start + ms(time));
        rig.main();
    }
    assert_eq!(
        rig.owner.snapshot().write_reports,
        writes,
        "main never retries blocked native write"
    );
    assert_eq!(rig.owner.snapshot().live_handles, 1);
    rig.owner.set_gate(Gate::Write, false);
    rig.owner.step();
    rig.main();
    rig.owner.set_gate(Gate::Close, true);
    rig.displays.stop_transports();
    assert!(!rig.owner.step());
    for _ in 0..20 {
        rig.main();
        assert!(!rig.owner.step());
    }
    assert_eq!(rig.owner.snapshot().live_handles, 1);
    assert!(matches!(
        rig.displays.start_transports()[0].1,
        Ok(TransportStart::Retiring)
    ));
    rig.owner.set_gate(Gate::Close, false);
    assert!(rig.owner.step());
    assert_eq!(rig.owner.snapshot().live_handles, 0);
    assert!(rig.owner.snapshot().exited);
}

#[test]
fn daemon_turn_actor_barrier_real_actor_background_job_allows_tap_hold_but_navigation_quarantines()
{
    use crate::slow_job::SourceJobs;
    use std::sync::mpsc;
    for navigation in [false, true] {
        let zero = Instant::now() + ms(20);
        let mut rig = Rig::new(timed_config(false), zero - ms(20), FakeContext::new());
        rig.warm(zero);
        let (release, gate) = mpsc::sync_channel(1);
        let (started, seen) = mpsc::sync_channel(1);
        let mut jobs = SourceJobs::spawn(move || {
            move |_: (), _| {
                started.send(()).unwrap();
                let _ = gate.recv();
            }
        })
        .unwrap();
        jobs.submit((), zero + Duration::from_secs(1)).unwrap();
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        for (time, pressed) in [(0, true), (5, false), (10, true)] {
            rig.clock.set(zero + ms(time));
            rig.owner.push_key(16, pressed);
            for _ in 0..6 {
                rig.owner.step();
                rig.main_navigation(navigation);
            }
        }
        for time in (15..=320).step_by(5) {
            rig.clock.set(zero + ms(time));
            for _ in 0..6 {
                rig.owner.step();
                rig.main_navigation(navigation);
            }
        }
        assert!(jobs.is_busy());
        assert_eq!(
            rig.downs(),
            if navigation { 0 } else { 2 },
            "tap and hold while source job is blocked"
        );
        release.send(()).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        while jobs.is_busy() {
            jobs.poll(Instant::now());
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        rig.cycle(zero + ms(325));
        assert_eq!(
            rig.downs(),
            if navigation { 0 } else { 2 },
            "settlement never replays old gesture"
        );
        rig.owner.push_key(16, false);
        rig.cycle(zero + ms(330));
        rig.owner.push_key(16, true);
        rig.cycle(zero + ms(335));
        rig.owner.push_key(16, false);
        rig.cycle(zero + ms(340));
        assert_eq!(
            rig.downs(),
            if navigation { 1 } else { 3 },
            "released fresh gesture rearms after settlement"
        );
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.owner.set_gate(Gate::Open, false);
        self.owner.set_gate(Gate::Write, false);
        self.owner.set_gate(Gate::Close, false);
        self.displays.stop_transports();
        for _ in 0..8 {
            if self.owner.step() {
                break;
            }
        }
    }
}

#[test]
fn daemon_turn_actor_barrier_real_actor_normal_startup_and_positive_hold_once() {
    let zero = Instant::now() + ms(20);
    let mut rig = Rig::new(timed_config(false), zero - ms(20), FakeContext::new());
    rig.warm(zero);
    rig.owner.push_key(16, true);
    for time in (0..=310).step_by(5) {
        rig.cycle(zero + ms(time));
    }
    assert_eq!(
        rig.downs(),
        1,
        "real parked actor capture must admit a normal hold"
    );
    assert_eq!(
        rig.calls
            .borrow()
            .iter()
            .map(|(_, down)| *down)
            .collect::<Vec<_>>(),
        [true, false]
    );
    rig.owner.push_key(16, false);
    rig.cycle(zero + ms(315));
    assert_eq!(rig.downs(), 1, "hold release never adds Enter");
}

#[test]
fn daemon_turn_actor_barrier_real_actor_original_300_305_sample300_consumed306() {
    let zero = Instant::now() + ms(20);
    let mut rig = Rig::new(timed_config(true), zero - ms(20), FakeContext::new());
    rig.warm(zero);
    rig.owner.push_key(16, true);
    rig.cycle(zero);
    rig.owner.push_key(17, true);
    rig.cycle(zero + ms(5));
    for time in (10..=295).step_by(5) {
        rig.cycle(zero + ms(time));
    }
    assert_eq!(rig.downs(), 0);
    // Finish prior main work and produce a REAL parked capture at original300.
    rig.clock.set(zero + ms(300));
    rig.owner.step();
    let request = rig.turns.requests["creator-micro"];
    let evidence = rig
        .displays
        .capture_evidence("creator-micro", request.id)
        .expect("real actor ready");
    assert_eq!(evidence.observed_at, zero + ms(300));
    for _ in 0..25 {
        rig.owner.step();
    }
    rig.clock.set(zero + ms(306));
    rig.main();
    assert_eq!(
        rig.downs(),
        1,
        "receipt306 must not authorize the305 deadline"
    );
    rig.owner.step();
    rig.main();
    assert_eq!(
        rig.downs(),
        2,
        "new original306 evidence covers the second control"
    );
}

#[test]
fn daemon_turn_glove80_publication_contention_real_actor_shared_context_ipc_source_pressure() {
    use crate::{
        hid::{Status, StatusSnapshot, StatusWriter},
        ipc::{MAX_PEERS, PeerPump},
        slow_job::SourceJobs,
    };
    use std::{
        io::{self, Read, Write},
        sync::{Arc, Mutex, mpsc},
    };
    struct Writer {
        context: FakeContext,
        writes: Arc<Mutex<Vec<StatusSnapshot>>>,
    }
    impl StatusWriter for Writer {
        fn write_snapshot(&mut self, snapshot: StatusSnapshot) -> anyhow::Result<()> {
            let _permit = self.context.enter(); // off-main worker, SAME Creator context
            self.writes.lock().unwrap().push(snapshot);
            Ok(())
        }
    }
    struct Peer {
        request: bool,
        calls: Rc<RefCell<(usize, usize)>>,
    }
    impl Read for Peer {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.calls.borrow_mut().0 += 1;
            if self.request {
                self.request = false;
                bytes[..3].copy_from_slice(b"{}\n");
                Ok(3)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
    }
    impl Write for Peer {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.calls.borrow_mut().1 += 1;
            Err(io::ErrorKind::WouldBlock.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("no blocking flush")
        }
    }
    let context = FakeContext::new();
    let start = Instant::now();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut rig = Rig::with_sinks(
        device_config(),
        start,
        context.clone(),
        vec![Box::new(crate::display::Glove80UsbDisplay::new(Writer {
            context: context.clone(),
            writes: writes.clone(),
        }))],
    );
    let host = DeviceConfig {id:"host".into(),path:"host.toml".into(),definition:toml::from_str("adapter='test'\n[keys.mic]\nmembers=['host:16']\naction='mic'\n[actions.mic]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='hold'").unwrap()};
    let configs = vec![device_config(), host];
    rig.engine = Switches::new(&configs, Poster(rig.calls.clone())).unwrap();
    rig.turns = TurnController::new(&configs);
    rig.owner.set_gate(Gate::Open, true);
    rig.start();
    rig.owner.step();
    assert!(context.snapshot().held);
    rig.main();
    let limit = Instant::now() + Duration::from_secs(5);
    while context.snapshot().waiters != 1 {
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    assert!(writes.lock().unwrap().is_empty());
    let mut peers = PeerPump::default();
    let io_calls = Rc::new(RefCell::new((0, 0)));
    for index in 0..MAX_PEERS {
        peers
            .admit(
                Peer {
                    request: index == 0,
                    calls: io_calls.clone(),
                },
                start,
            )
            .unwrap();
    }
    assert!(
        peers
            .admit(
                Peer {
                    request: false,
                    calls: io_calls.clone()
                },
                start
            )
            .is_err()
    );
    let (release, gate) = mpsc::sync_channel(1);
    let (begun, seen) = mpsc::sync_channel(1);
    let mut jobs = SourceJobs::spawn(move || {
        move |_: (), _| {
            begun.send(()).unwrap();
            let _ = gate.recv();
        }
    })
    .unwrap();
    jobs.submit((), start + Duration::from_secs(1)).unwrap();
    seen.recv_timeout(Duration::from_secs(5)).unwrap();
    for (index, pressed) in [true, false].into_iter().enumerate() {
        rig.clock.set(start + ms(index as u64));
        let now = rig.clock.now();
        let batch = DeviceInputBatch {
            device: "host".into(),
            completed_at: now,
            failure: None,
            batch: InputBatch {
                events: vec![DeviceEvent::Key {
                    address: "host:16".to_string().try_into().unwrap(),
                    pressed,
                }],
                drained: true,
                continuity: InputContinuity::Continuous,
                ..Default::default()
            },
        };
        rig.turns.service_transports(
            &mut rig.engine,
            &mut rig.displays,
            vec![batch],
            InputPollBudget {
                deadline: now + ms(5),
                max_raw_frames: 128,
            },
            || now,
            false,
        );
        if let Some(request) = peers.poll(now) {
            peers.respond(request.id, vec![b'x'; 65536], now).unwrap();
        }
    }
    for index in 2..102 {
        let now = start + ms(index);
        rig.clock.set(now);
        let before = *io_calls.borrow();
        rig.main();
        peers.poll(now);
        assert!(jobs.is_busy());
        let after = *io_calls.borrow();
        assert!(after.0 - before.0 <= MAX_PEERS);
        assert!(after.1 - before.1 <= MAX_PEERS);
        rig.plan.keys[0].state = Some(if index % 2 == 0 {
            crate::render::AgentState::Working
        } else {
            crate::render::AgentState::Done
        });
    }
    rig.plan.keys[0].state = Some(crate::render::AgentState::Done);
    rig.main();
    assert_eq!(
        rig.calls
            .borrow()
            .iter()
            .map(|(_, down)| *down)
            .collect::<Vec<_>>(),
        [true, false]
    );
    assert!(!rig.engine.output_down());
    assert!(writes.lock().unwrap().is_empty());
    assert_eq!(rig.owner.snapshot().open_polls, 1);
    assert_eq!(rig.owner.snapshot().live_handles, 0);
    assert!(
        rig.displays.diagnostics()["glove80"]
            .last_output_success
            .is_none()
    );
    rig.owner.set_gate(Gate::Open, false);
    rig.owner.step();
    rig.owner.step();
    while writes
        .lock()
        .unwrap()
        .last()
        .is_none_or(|snapshot| snapshot.slots[0] != Status::Done)
    {
        rig.main();
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    let snapshots = writes.lock().unwrap().clone();
    assert_eq!(
        snapshots.len(),
        2,
        "one blocked snapshot then coalesced latest, no FIFO"
    );
    assert_eq!(snapshots[0].sequence, 0);
    assert_eq!(snapshots[1].sequence, 1);
    assert_eq!(snapshots[1].slots[0], Status::Done);
    for (wire, desired) in snapshots[1]
        .treatments
        .iter()
        .zip(rig.plan.treatments.ordered())
    {
        assert_eq!(wire.color, desired.color);
        assert_eq!(wire.brightness, (desired.brightness * 255.0).round() as u8);
        assert_eq!(wire.motion, desired.motion);
    }
    while rig.displays.diagnostics()["glove80"]
        .last_output_success
        .is_none()
    {
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    assert_eq!(
        context.snapshot().acquisitions,
        3,
        "one Creator permit, two status sends"
    );
    assert_eq!(rig.owner.snapshot().live_handles, 1);
    release.send(()).unwrap();
    while jobs.is_busy() {
        jobs.poll(Instant::now());
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
}
