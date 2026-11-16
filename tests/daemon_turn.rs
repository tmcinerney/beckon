use beckon::{
    chord::Chord,
    daemon_turn::TurnController,
    device::{Address, DeviceConfig},
    display::{DeviceEvent, DeviceInputBatch, InputBatch, InputContinuity, InputPollBudget},
    switches::{Emitter, Switches},
};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

type Calls = Rc<RefCell<Vec<(Chord, bool)>>>;
struct Recorder(Calls);
impl Emitter for Recorder {
    fn emit(&mut self, chord: &Chord, down: bool) -> anyhow::Result<()> {
        self.0.borrow_mut().push((chord.clone(), down));
        Ok(())
    }
}
fn config() -> DeviceConfig {
    DeviceConfig { id: "pad".into(), path: "pad.toml".into(), definition: toml::from_str(
        "adapter='test'\n[keys.enter]\nmembers=['pad:18']\naction='enter'\n[actions.enter]\nkind='tap-hold-chords'\ntap_keys=['enter']\nhold_keys=['shift','enter']\nhold_ms=300\n[keys.mic]\nmembers=['pad:16','pad:17']\naction='mic'\n[actions.mic]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='hold'").unwrap() }
}
fn edge(id: u8, pressed: bool) -> DeviceEvent {
    DeviceEvent::Key {
        address: addr(id),
        pressed,
    }
}
fn addr(id: u8) -> Address {
    format!("pad:{id}").try_into().unwrap()
}
fn batch(at: Instant, events: Vec<DeviceEvent>) -> DeviceInputBatch {
    DeviceInputBatch {
        device: "pad".into(),
        completed_at: at,
        failure: None,
        batch: InputBatch {
            raw_frames: events.len(),
            events,
            drained: true,
            partial_frame: false,
            continuity: InputContinuity::Continuous,
            ..InputBatch::default()
        },
    }
}
fn budget(at: Instant) -> InputPollBudget {
    InputPollBudget {
        deadline: at + Duration::from_millis(5),
        max_raw_frames: 128,
    }
}

#[test]
fn daemon_turn_stall_cancels_queued_release_but_never_delays_owned_mic_key_up() {
    let cfg = config();
    let calls = Calls::default();
    let mut engine = Switches::new(std::slice::from_ref(&cfg), Recorder(calls.clone())).unwrap();
    let mut turns = TurnController::new(&[cfg]);
    let now = Instant::now();
    turns.service(
        &mut engine,
        vec![batch(now, vec![])],
        budget(now),
        now,
        false,
    );
    let down = now + Duration::from_millis(10);
    turns.service(
        &mut engine,
        vec![batch(down, vec![edge(16, true), edge(18, true)])],
        budget(down),
        down,
        false,
    );
    let stalled = now + Duration::from_millis(1100);
    turns.service(
        &mut engine,
        vec![batch(stalled, vec![edge(18, false), edge(16, false)])],
        budget(stalled),
        stalled,
        false,
    );
    assert_eq!(
        calls.borrow().len(),
        2,
        "only mic down/up, never queued tap or hold"
    );
    assert!(!engine.output_down());
    let backlog = stalled + Duration::from_millis(10);
    turns.service(
        &mut engine,
        vec![batch(backlog, vec![edge(18, true), edge(18, false)])],
        budget(backlog),
        backlog,
        false,
    );
    assert_eq!(
        calls.borrow().len(),
        2,
        "new down+up in one batch is ambiguous too"
    );
}

#[test]
fn daemon_turn_clean_samples_fire_once_and_late_release_never_substitutes_hold() {
    for late_release in [false, true] {
        let cfg = config();
        let calls = Calls::default();
        let now = Instant::now();
        let mut engine =
            Switches::new(std::slice::from_ref(&cfg), Recorder(calls.clone())).unwrap();
        let mut turns = TurnController::new(&[cfg]);
        turns.service(
            &mut engine,
            vec![batch(now, vec![])],
            budget(now),
            now,
            false,
        );
        for ms in (10..=300).step_by(10) {
            let at = now + Duration::from_millis(ms);
            let events = if ms == 10 {
                vec![edge(18, true)]
            } else {
                vec![]
            };
            turns.service(&mut engine, vec![batch(at, events)], budget(at), at, false);
        }
        let at = now + Duration::from_millis(310);
        turns.service(
            &mut engine,
            vec![batch(
                at,
                if late_release {
                    vec![edge(18, false)]
                } else {
                    vec![]
                },
            )],
            budget(at),
            at,
            false,
        );
        let at = now + Duration::from_millis(320);
        turns.service(
            &mut engine,
            vec![batch(at, vec![edge(18, false)])],
            budget(at),
            at,
            false,
        );
        assert_eq!(calls.borrow().len(), if late_release { 0 } else { 2 });
        assert!(!engine.output_down());
    }
}

#[test]
fn daemon_turn_frame_129_reset_partial_progress_and_delayed_poll_cannot_authorize_hold() {
    for cause in ["budget", "partial", "overrun", "navigation"] {
        let cfg = config();
        let calls = Calls::default();
        let now = Instant::now();
        let mut engine =
            Switches::new(std::slice::from_ref(&cfg), Recorder(calls.clone())).unwrap();
        let mut turns = TurnController::new(&[cfg]);
        turns.service(
            &mut engine,
            vec![batch(now, vec![])],
            budget(now),
            now,
            false,
        );
        for ms in (10..=300).step_by(10) {
            let at = now + Duration::from_millis(ms);
            let events = if ms == 10 {
                vec![edge(16, true), edge(18, true)]
            } else {
                vec![]
            };
            turns.service(&mut engine, vec![batch(at, events)], budget(at), at, false);
        }
        let at = now + Duration::from_millis(310);
        let mut input = batch(at, vec![]);
        if cause == "budget" {
            input.batch.raw_frames = 128;
            input.batch.drained = false;
        }
        if cause == "partial" {
            input.batch.raw_frames = 1;
            input.batch.partial_frame = true;
        }
        let serviced_at = if cause == "overrun" {
            at + Duration::from_millis(700)
        } else {
            at
        };
        input.completed_at = serviced_at;
        turns.service(
            &mut engine,
            vec![input],
            budget(at),
            serviced_at,
            cause == "navigation",
        );
        let at = serviced_at + Duration::from_millis(1);
        let mut reset = batch(
            at,
            vec![
                DeviceEvent::Reset {
                    device: "pad".into(),
                },
                edge(18, false),
            ],
        );
        reset.batch.continuity = InputContinuity::Reset;
        turns.service(&mut engine, vec![reset], budget(at), at, false);
        assert_eq!(
            calls.borrow().len(),
            2,
            "{cause}: only existing mic down/reset-up"
        );
        assert!(!engine.output_down());
    }
}
