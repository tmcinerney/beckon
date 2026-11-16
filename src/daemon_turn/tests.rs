use super::*;
use crate::{chord::Chord, display::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[test]
fn daemon_turn_actor_barrier_requests_follow_feedback_and_wake_before_lease_expiry() {
    struct Ordered(Rc<RefCell<Vec<&'static str>>>);
    impl DisplaySink for Ordered {
        fn id(&self) -> &str {
            "pad"
        }
        fn publish(&mut self, _: &crate::render::RenderPlan) -> anyhow::Result<PublishOutcome> {
            self.0.borrow_mut().push("publish");
            Ok(PublishOutcome::Unchanged)
        }
        fn transport_epoch(&self) -> Option<TransportEpoch> {
            Some(TransportEpoch {
                actor: 8,
                connection: 1,
                selector: 1,
            })
        }
        fn request_capture(&mut self, _: CaptureRequest) -> CaptureRequestOutcome {
            self.0.borrow_mut().push("request");
            CaptureRequestOutcome::Pending
        }
    }
    let cfg = DeviceConfig {
        id: "pad".into(),
        path: "pad.toml".into(),
        definition: toml::from_str("adapter='test'").unwrap(),
    };
    let at = Instant::now();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut displays = DisplaySet::new(vec![Box::new(Ordered(calls.clone()))]);
    let mut engine = Switches::new(std::slice::from_ref(&cfg), Poster(calls.clone())).unwrap();
    let mut turns = TurnController::new(&[cfg]);
    turns.service_transports(
        &mut engine,
        &mut displays,
        vec![],
        InputPollBudget {
            deadline: at + Duration::from_millis(5),
            max_raw_frames: 128,
        },
        || at,
        false,
    );
    assert!(
        calls.borrow().is_empty(),
        "no request may race the impending feedback frame"
    );
    displays
        .publish(&crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap());
    turns.request_captures(&mut engine, &mut displays, at);
    assert_eq!(*calls.borrow(), vec!["publish", "request"]);
    assert_eq!(
        turns.next_capture_wake(),
        Some(at + Duration::from_millis(5))
    );
}

struct Actor {
    at: Rc<Cell<Instant>>,
    start: Instant,
    request: Option<CaptureRequest>,
    calls: Rc<RefCell<Vec<&'static str>>>,
    deny: bool,
    silent: bool,
    sample_lag: Rc<Cell<Duration>>,
    epoch_busy: Rc<Cell<bool>>,
    claimed: bool,
}
impl DisplaySink for Actor {
    fn id(&self) -> &str {
        "pad"
    }
    fn publish(&mut self, _: &crate::render::RenderPlan) -> anyhow::Result<PublishOutcome> {
        panic!("controller must not publish during admission")
    }
    fn transport_epoch(&self) -> Option<TransportEpoch> {
        if self.epoch_busy.get() {
            return None;
        }
        Some(TransportEpoch {
            actor: 7,
            connection: 1,
            selector: 1,
        })
    }
    fn request_capture(&mut self, request: CaptureRequest) -> CaptureRequestOutcome {
        if self
            .request
            .is_some_and(|old| self.at.get() < old.expires_at)
        {
            return CaptureRequestOutcome::Rejected(CaptureReject::Busy);
        }
        self.request = Some(request);
        CaptureRequestOutcome::Pending
    }
    fn capture_evidence(&self, id: CaptureId) -> Option<CaptureEvidence> {
        if self.silent {
            return None;
        }
        let request = self
            .request
            .filter(|request| request.id == id && self.at.get() >= request.not_before)?;
        Some(CaptureEvidence {
            request,
            command_revision: 1,
            ingress_frontier: 0,
            event_frontier: 0,
            started_at: request.not_before,
            observed_at: self.at.get() - self.sample_lag.get(),
            completed_at: self.at.get() - self.sample_lag.get(),
            lease_until: request.expires_at,
            raw_frames: 0,
            partial_frame: false,
            backlog: false,
            continuity: InputContinuity::Continuous,
        })
    }
    fn try_claim_capture(&mut self, _: CaptureEvidence, _: Instant) -> CaptureClaim {
        self.calls.borrow_mut().push("claim");
        if self.deny {
            return CaptureClaim::Rejected(CaptureReject::Busy);
        }
        self.claimed = true;
        CaptureClaim::Claimed(CaptureToken {
            evidence: self.capture_evidence(self.request.unwrap().id).unwrap(),
        })
    }
    fn consume_capture(
        &mut self,
        token: CaptureToken,
        frontier: u64,
        now: Instant,
    ) -> CaptureConsume {
        assert!(self.claimed);
        assert!(frontier >= token.evidence.event_frontier);
        assert!(now < token.evidence.lease_until);
        self.calls.borrow_mut().push("consume");
        self.claimed = false;
        CaptureConsume::Consumed
    }
    fn release_capture(&mut self, _: CaptureToken) {
        self.calls.borrow_mut().push("release");
        self.request = None;
        self.claimed = false;
    }
    fn poll_input_batch(
        &mut self,
        _: InputPollMode,
        _: InputPollBudget,
    ) -> anyhow::Result<InputBatch> {
        let ms = self.at.get().duration_since(self.start).as_millis();
        let events = match ms {
            15 => vec![DeviceEvent::Key {
                address: "pad:20".to_string().try_into().unwrap(),
                pressed: true,
            }],
            value if value == if self.silent { 5 } else { 10 } => vec![DeviceEvent::Key {
                address: "pad:18".to_string().try_into().unwrap(),
                pressed: true,
            }],
            value if value == if self.silent { 9 } else { 320 } => vec![DeviceEvent::Key {
                address: "pad:18".to_string().try_into().unwrap(),
                pressed: false,
            }],
            _ => vec![],
        };
        Ok(InputBatch {
            events,
            drained: false,
            continuity: InputContinuity::Continuous,
            transport_epoch: self.transport_epoch(),
            event_frontier: Some(0),
            ..Default::default()
        })
    }
}
struct Poster(Rc<RefCell<Vec<&'static str>>>);

#[test]
fn backend_review_r1_busy_epoch_does_not_release_owned_mic_before_physical_release() {
    let cfg = DeviceConfig {id:"pad".into(),path:"pad.toml".into(),definition:toml::from_str("adapter='test'\n[keys.mic]\nmembers=['pad:18']\naction='mic'\n[actions.mic]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='hold'").unwrap()};
    let start = Instant::now();
    let at = Rc::new(Cell::new(start));
    let busy = Rc::new(Cell::new(false));
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut displays = DisplaySet::new(vec![Box::new(Actor {
        at: at.clone(),
        start,
        request: None,
        calls: calls.clone(),
        deny: false,
        silent: false,
        claimed: false,
        sample_lag: Rc::new(Cell::new(Duration::ZERO)),
        epoch_busy: busy.clone(),
    })]);
    let mut engine = Switches::new(std::slice::from_ref(&cfg), Poster(calls.clone())).unwrap();
    let mut turns = TurnController::new(&[cfg]);
    for time in (0..=320).step_by(5) {
        at.set(start + Duration::from_millis(time));
        busy.set(time == 15);
        turns.service_transports(
            &mut engine,
            &mut displays,
            vec![],
            InputPollBudget {
                deadline: at.get() + Duration::from_millis(5),
                max_raw_frames: 128,
            },
            || at.get(),
            false,
        );
        turns.request_captures(&mut engine, &mut displays, at.get());
        if (10..320).contains(&time) {
            assert!(
                engine.output_down(),
                "metadata miss is not a disconnect at {time}"
            );
        }
    }
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|call| **call == "down")
            .count(),
        1
    );
    assert_eq!(
        calls.borrow().iter().filter(|call| **call == "up").count(),
        1
    );
    assert!(!engine.output_down());
}

#[test]
fn daemon_turn_actor_barrier_controller_original_sample_covers_300_not_305_at_receipt_306() {
    // Generic fake-sink proof of the ACTUAL controller, not a claim that this
    // fake implements the production Creator actor's parking/factory protocol.
    let cfg=DeviceConfig{id:"pad".into(),path:"pad.toml".into(),definition:toml::from_str("adapter='test'\n[keys.first]\nmembers=['pad:18']\naction='send'\n[keys.second]\nmembers=['pad:20']\naction='send'\n[actions.send]\nkind='tap-hold-chords'\ntap_keys=['enter']\nhold_keys=['shift','enter']\nhold_ms=290").unwrap()};
    let start = Instant::now();
    let at = Rc::new(Cell::new(start));
    let lag = Rc::new(Cell::new(Duration::ZERO));
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut displays = DisplaySet::new(vec![Box::new(Actor {
        at: at.clone(),
        start,
        request: None,
        calls: calls.clone(),
        deny: false,
        silent: false,
        sample_lag: lag.clone(),
        epoch_busy: Rc::new(Cell::new(false)),
        claimed: false,
    })]);
    let mut engine = Switches::new(std::slice::from_ref(&cfg), Poster(calls.clone())).unwrap();
    let mut turns = TurnController::new(&[cfg]);
    for ms in (0..=295).step_by(5) {
        at.set(start + Duration::from_millis(ms));
        turns.service_transports(
            &mut engine,
            &mut displays,
            vec![],
            InputPollBudget {
                deadline: at.get() + Duration::from_millis(5),
                max_raw_frames: 128,
            },
            || at.get(),
            false,
        );
        turns.request_captures(&mut engine, &mut displays, at.get());
    }
    lag.set(Duration::from_millis(6));
    at.set(start + Duration::from_millis(306));
    turns.service_transports(
        &mut engine,
        &mut displays,
        vec![],
        InputPollBudget {
            deadline: at.get() + Duration::from_millis(5),
            max_raw_frames: 128,
        },
        || at.get(),
        false,
    );
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|call| **call == "down")
            .count(),
        1
    );
    turns.request_captures(&mut engine, &mut displays, at.get());
    lag.set(Duration::ZERO);
    at.set(start + Duration::from_millis(307));
    turns.service_transports(
        &mut engine,
        &mut displays,
        vec![],
        InputPollBudget {
            deadline: at.get() + Duration::from_millis(5),
            max_raw_frames: 128,
        },
        || at.get(),
        false,
    );
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|call| **call == "down")
            .count(),
        2,
        "second control requires its OWN covered deadline in a new consumed capture"
    );
}
impl Emitter for Poster {
    fn emit(&mut self, _: &Chord, down: bool) -> anyhow::Result<()> {
        self.0.borrow_mut().push(if down { "down" } else { "up" });
        Ok(())
    }
}

#[test]
fn daemon_turn_actor_barrier_release_runs_even_when_timer_emitter_unwinds() {
    struct Crash(Rc<Cell<bool>>);
    impl Emitter for Crash {
        fn emit(&mut self, _: &Chord, _: bool) -> anyhow::Result<()> {
            if self.0.get() {
                panic!("fake posting unwind")
            }
            Ok(())
        }
    }
    let cfg=DeviceConfig{id:"pad".into(),path:"pad.toml".into(),definition:toml::from_str("adapter='test'\n[keys.send]\nmembers=['pad:18']\naction='send'\n[actions.send]\nkind='tap-hold-chords'\ntap_keys=['enter']\nhold_keys=['shift','enter']\nhold_ms=300").unwrap()};
    let start = Instant::now();
    let at = Rc::new(Cell::new(start));
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut displays = DisplaySet::new(vec![Box::new(Actor {
        at: at.clone(),
        start,
        request: None,
        calls: calls.clone(),
        deny: false,
        silent: false,
        sample_lag: Rc::new(Cell::new(Duration::ZERO)),
        epoch_busy: Rc::new(Cell::new(false)),
        claimed: false,
    })]);
    let crash = Rc::new(Cell::new(true));
    let mut engine = Switches::new(std::slice::from_ref(&cfg), Crash(crash.clone())).unwrap();
    let mut turns = TurnController::new(&[cfg]);
    for ms in (0..310).step_by(5) {
        at.set(start + Duration::from_millis(ms));
        turns.service_transports(
            &mut engine,
            &mut displays,
            vec![],
            InputPollBudget {
                deadline: at.get() + Duration::from_millis(5),
                max_raw_frames: 128,
            },
            || at.get(),
            false,
        );
        turns.request_captures(&mut engine, &mut displays, at.get());
    }
    at.set(start + Duration::from_millis(310));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        turns.service_transports(
            &mut engine,
            &mut displays,
            vec![],
            InputPollBudget {
                deadline: at.get() + Duration::from_millis(5),
                max_raw_frames: 128,
            },
            || at.get(),
            false,
        )
    }));
    crash.set(false);
    assert!(result.is_err());
    assert_eq!(calls.borrow().last(), Some(&"release"));
}

#[test]
fn daemon_turn_actor_barrier_claim_consume_advance_release_and_busy_never_falls_back() {
    for (deny, silent) in [(false, false), (true, false), (false, true)] {
        let cfg=DeviceConfig{id:"pad".into(),path:"pad.toml".into(),definition:toml::from_str("adapter='test'\n[keys.send]\nmembers=['pad:18']\naction='send'\n[actions.send]\nkind='tap-hold-chords'\ntap_keys=['enter']\nhold_keys=['shift','enter']\nhold_ms=300").unwrap()};
        let start = Instant::now();
        let at = Rc::new(Cell::new(start));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut displays = DisplaySet::new(vec![Box::new(Actor {
            at: at.clone(),
            start,
            request: None,
            calls: calls.clone(),
            deny,
            silent,
            sample_lag: Rc::new(Cell::new(Duration::ZERO)),
            epoch_busy: Rc::new(Cell::new(false)),
            claimed: false,
        })]);
        let mut engine = Switches::new(std::slice::from_ref(&cfg), Poster(calls.clone())).unwrap();
        let mut turns = TurnController::new(&[cfg]);
        let times = if silent {
            vec![0, 5, 9]
        } else {
            (0..=320).step_by(5).collect()
        };
        for ms in times {
            at.set(start + Duration::from_millis(ms));
            turns.service_transports(
                &mut engine,
                &mut displays,
                vec![],
                InputPollBudget {
                    deadline: at.get() + Duration::from_millis(5),
                    max_raw_frames: 128,
                },
                || at.get(),
                false,
            );
            turns.request_captures(&mut engine, &mut displays, at.get());
        }
        let calls = calls.borrow();
        if deny || silent {
            assert!(!calls.contains(&"down"));
        } else {
            assert_eq!(calls.iter().filter(|call| **call == "down").count(), 1);
            let position = calls.iter().position(|call| *call == "down").unwrap();
            assert_eq!(
                &calls[position - 1..position + 3],
                ["consume", "down", "up", "release"]
            );
        }
    }
}
