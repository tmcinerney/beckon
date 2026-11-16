use beckon::{
    chord::Chord,
    device::{Address, DeviceConfig},
    switches::{Emitter, Switches},
};
use std::{
    cell::RefCell,
    collections::BTreeSet,
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
fn address(id: u8) -> Address {
    format!("pad:{id}").try_into().unwrap()
}
fn chord(keys: &[&str]) -> Chord {
    Chord::parse(&keys.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
}
fn device() -> DeviceConfig {
    DeviceConfig {
        id: "pad".into(), path: "pad.toml".into(),
        definition: toml::from_str("adapter='test'\n[keys.send]\nmembers=['pad:18','pad:19']\naction='send'\neffects='cue'\n[actions.send]\nkind='tap-hold-chords'\ntap_keys=['enter']\nhold_keys=['shift','enter']\nhold_ms=300\n[effects.cue]\npending={color='#FFFFFF',brightness=0.12,motion='steady'}\nhold_fired={color='#3388FF',brightness=0.45,motion='steady'}\nerror={treatment={color='#FFB000',brightness=0.5,motion='steady'},duration_ms=650}").unwrap(),
    }
}
fn setup() -> (Switches<Recorder>, Calls) {
    let calls = Calls::default();
    (
        Switches::new(&[device()], Recorder(calls.clone())).unwrap(),
        calls,
    )
}

#[test]
fn control_failure_history_recovers_only_after_relevant_output_not_visual_expiry_or_reset() {
    use std::cell::Cell;
    struct Deny(Rc<Cell<bool>>);
    impl Emitter for Deny {
        fn emit(&mut self, _: &Chord, _: bool) -> anyhow::Result<()> {
            if self.0.get() {
                Err(beckon::chord::PostingError::PermissionDenied.into())
            } else {
                Ok(())
            }
        }
    }
    let denied = Rc::new(Cell::new(true));
    let now = Instant::now();
    let mut engine = Switches::new(&[device()], Deny(denied.clone())).unwrap();
    engine.event(&address(18), true, now).unwrap();
    assert!(
        engine
            .event(&address(18), false, now + Duration::from_millis(1))
            .is_err()
    );
    assert!(engine.control_observations()[0].2.active);
    engine.advance(now + Duration::from_secs(1), &Default::default());
    engine.reset_device("pad").unwrap();
    assert!(
        engine.control_observations()[0].2.active,
        "no output occurred during expiry/reset"
    );
    denied.set(false);
    engine
        .event(&address(18), true, now + Duration::from_secs(2))
        .unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(2001))
        .unwrap();
    assert!(!engine.control_observations()[0].2.active);
    assert_eq!(
        engine.control_observations()[0].2.at,
        now + Duration::from_millis(1)
    );
}

#[test]
fn daemon_turn_actor_barrier_original_observation_covers_each_control_not_receipt_time() {
    let mut cfg = device();
    let mut second = cfg.definition.keys["send"].clone();
    second.members = vec![address(20)];
    cfg.definition.keys.insert("second".into(), second);
    let calls = Calls::default();
    let now = Instant::now();
    let mut engine = Switches::new(&[cfg], Recorder(calls.clone())).unwrap();
    engine.event(&address(18), true, now).unwrap();
    engine
        .event(&address(20), true, now + Duration::from_millis(5))
        .unwrap();
    engine.advance_observed(
        now + Duration::from_millis(308),
        "pad",
        now + Duration::from_millis(300),
    );
    assert_eq!(
        calls.borrow().len(),
        2,
        "the 305ms control is not covered by the original 300ms sample"
    );
    engine.advance_observed(
        now + Duration::from_millis(309),
        "pad",
        now + Duration::from_millis(305),
    );
    assert_eq!(calls.borrow().len(), 4);
}

#[test]
fn short_tap_posts_plain_enter_once_only_on_last_member_release() {
    let (mut engine, calls) = setup();
    let now = Instant::now();
    engine.event(&address(18), true, now).unwrap();
    engine.event(&address(18), true, now).unwrap();
    engine
        .event(&address(19), true, now + Duration::from_millis(10))
        .unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(100))
        .unwrap();
    assert!(calls.borrow().is_empty());
    assert_eq!(
        engine.treatments()[&address(18)].color.to_string(),
        "#FFFFFF"
    );
    engine
        .event(&address(19), false, now + Duration::from_millis(299))
        .unwrap();
    engine
        .event(&address(19), false, now + Duration::from_millis(299))
        .unwrap();
    assert_eq!(
        *calls.borrow(),
        vec![(chord(&["enter"]), true), (chord(&["enter"]), false)]
    );
    assert!(engine.treatments().is_empty());
}

#[test]
fn hold_requires_trusted_timer_advance_and_fires_one_pulse_without_release_enter() {
    let (mut engine, calls) = setup();
    let now = Instant::now();
    let trusted = BTreeSet::from(["pad".to_string()]);
    engine.event(&address(18), true, now).unwrap();
    assert_eq!(
        engine.next_deadline(),
        Some(now + Duration::from_millis(300))
    );
    assert!(
        engine
            .advance(now + Duration::from_millis(299), &trusted)
            .is_empty()
    );
    assert!(calls.borrow().is_empty());
    assert!(
        engine
            .advance(now + Duration::from_millis(300), &BTreeSet::new())
            .is_empty()
    );
    assert!(calls.borrow().is_empty());
    assert!(
        engine
            .advance(now + Duration::from_millis(300), &trusted)
            .is_empty()
    );
    assert_eq!(
        engine.treatments()[&address(18)].color.to_string(),
        "#3388FF"
    );
    assert!(
        engine
            .advance(now + Duration::from_millis(700), &trusted)
            .is_empty()
    );
    engine
        .event(&address(18), false, now + Duration::from_millis(701))
        .unwrap();
    assert_eq!(
        *calls.borrow(),
        vec![
            (chord(&["shift", "enter"]), true),
            (chord(&["shift", "enter"]), false)
        ]
    );
    assert!(engine.treatments().is_empty());
    assert!(!engine.output_down());
}

#[test]
fn ambiguity_cancellation_quarantines_until_release_and_clean_rearm() {
    let (mut engine, calls) = setup();
    let now = Instant::now();
    let trusted = BTreeSet::from(["pad".to_string()]);
    engine.event(&address(18), true, now).unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(300))
        .unwrap();
    assert!(
        calls.borrow().is_empty(),
        "late release must not become hold or tap"
    );
    engine
        .event(&address(18), true, now + Duration::from_millis(310))
        .unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(320))
        .unwrap();
    assert!(
        calls.borrow().is_empty(),
        "ambiguous release requires a new clean fence"
    );
    engine.rearm_device("pad");
    engine
        .event(&address(18), true, now + Duration::from_secs(1))
        .unwrap();
    engine.cancel_pending("pad");
    engine.rearm_device("pad");
    engine
        .event(&address(19), true, now + Duration::from_millis(1100))
        .unwrap();
    engine.advance(now + Duration::from_millis(1400), &trusted);
    engine
        .event(&address(18), false, now + Duration::from_millis(1410))
        .unwrap();
    engine
        .event(&address(19), false, now + Duration::from_millis(1420))
        .unwrap();
    assert!(calls.borrow().is_empty());
    engine.rearm_device("pad");
    engine
        .event(&address(18), true, now + Duration::from_secs(2))
        .unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(2010))
        .unwrap();
    assert_eq!(calls.borrow().len(), 2);
}

#[test]
fn failed_key_up_retries_only_cleanup_and_error_cue_has_explicit_expiry() {
    struct Flaky {
        calls: Calls,
        fail_up: bool,
    }
    impl Emitter for Flaky {
        fn emit(&mut self, chord: &Chord, down: bool) -> anyhow::Result<()> {
            self.calls.borrow_mut().push((chord.clone(), down));
            if !down && self.fail_up {
                self.fail_up = false;
                anyhow::bail!("temporary failure")
            }
            Ok(())
        }
    }
    let calls = Calls::default();
    let mut engine = Switches::new(
        &[device()],
        Flaky {
            calls: calls.clone(),
            fail_up: true,
        },
    )
    .unwrap();
    let now = Instant::now();
    engine.event(&address(18), true, now).unwrap();
    assert_eq!(
        engine.snapshot(now)[0].phase,
        beckon::switches::ControlPhase::Pending
    );
    let failures = engine.advance(
        now + Duration::from_millis(300),
        &BTreeSet::from(["pad".into()]),
    );
    assert_eq!(failures.len(), 1);
    assert!(engine.output_down());
    assert_eq!(
        engine.snapshot(now + Duration::from_millis(300))[0].phase,
        beckon::switches::ControlPhase::Error
    );
    assert_eq!(
        engine.treatments()[&address(18)].color.to_string(),
        "#FFB000"
    );
    engine
        .event(&address(18), false, now + Duration::from_millis(301))
        .unwrap();
    assert!(
        engine
            .advance(now + Duration::from_millis(302), &BTreeSet::new())
            .is_empty()
    );
    assert!(!engine.output_down());
    assert!(!engine.snapshot(now + Duration::from_millis(302))[0].cleanup_pending);
    assert_eq!(
        *calls.borrow(),
        vec![
            (chord(&["shift", "enter"]), true),
            (chord(&["shift", "enter"]), false),
            (chord(&["shift", "enter"]), false)
        ]
    );
    engine.advance(now + Duration::from_millis(949), &BTreeSet::new());
    assert!(!engine.treatments().is_empty());
    engine.advance(now + Duration::from_millis(950), &BTreeSet::new());
    assert!(engine.treatments().is_empty());
}

#[test]
fn backwards_clock_cancels_and_reset_never_emits_a_deferred_tap() {
    let (mut engine, calls) = setup();
    let now = Instant::now();
    let trusted = BTreeSet::from(["pad".into()]);
    engine
        .event(&address(18), true, now + Duration::from_millis(5))
        .unwrap();
    engine.advance(now, &trusted);
    engine.advance(now + Duration::from_secs(1), &trusted);
    assert!(calls.borrow().is_empty());
    engine.reset_device("pad").unwrap();
    engine
        .event(&address(18), false, now + Duration::from_secs(2))
        .unwrap();
    assert!(calls.borrow().is_empty());
    engine
        .event(&address(18), true, now + Duration::from_secs(3))
        .unwrap();
    engine.reset_device("pad").unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(3010))
        .unwrap();
    assert!(calls.borrow().is_empty());
}

#[test]
fn cancelling_two_chord_input_does_not_delay_joined_microphone_release() {
    use beckon::device::{Action, Key, Mode};
    let mut config = device();
    config.definition.actions.insert(
        "record".into(),
        Action::Chord {
            keys: vec!["ctrl".into(), "opt".into(), "d".into()],
            mode: Mode::Hold,
            tap_ms: None,
        },
    );
    config.definition.keys.insert(
        "record".into(),
        Key {
            members: vec![address(20), address(21)],
            binding: None,
            action: Some("record".into()),
            effects: None,
        },
    );
    let calls = Calls::default();
    let mut engine = Switches::new(&[config], Recorder(calls.clone())).unwrap();
    let now = Instant::now();
    engine.event(&address(20), true, now).unwrap();
    engine.event(&address(21), true, now).unwrap();
    engine.event(&address(18), true, now).unwrap();
    engine.cancel_pending("pad");
    engine
        .event(&address(20), false, now + Duration::from_millis(1))
        .unwrap();
    assert!(engine.output_down());
    engine
        .event(&address(21), false, now + Duration::from_millis(2))
        .unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(3))
        .unwrap();
    assert!(!engine.output_down());
    assert_eq!(
        *calls.borrow(),
        vec![
            (chord(&["ctrl", "opt", "d"]), true),
            (chord(&["ctrl", "opt", "d"]), false)
        ]
    );
}

#[test]
fn independent_controls_sharing_an_action_reference_do_not_join_gestures() {
    let mut config = device();
    let mut independent = config.definition.keys["send"].clone();
    independent.members = vec![address(22)];
    config
        .definition
        .keys
        .insert("independent".into(), independent);
    let calls = Calls::default();
    let mut engine = Switches::new(&[config], Recorder(calls.clone())).unwrap();
    let now = Instant::now();
    engine.event(&address(18), true, now).unwrap();
    engine
        .event(&address(22), true, now + Duration::from_millis(10))
        .unwrap();
    engine
        .event(&address(18), false, now + Duration::from_millis(20))
        .unwrap();
    assert_eq!(calls.borrow().len(), 2);
    assert_eq!(
        engine.treatments()[&address(22)].color.to_string(),
        "#FFFFFF"
    );
    engine
        .event(&address(22), false, now + Duration::from_millis(30))
        .unwrap();
    assert_eq!(calls.borrow().iter().filter(|(_, down)| *down).count(), 2);
}

#[test]
fn proven_posting_denial_reports_typed_error_without_cleanup_or_false_success() {
    struct Denied(Rc<RefCell<Vec<bool>>>);
    impl Emitter for Denied {
        fn emit(&mut self, _: &Chord, down: bool) -> anyhow::Result<()> {
            self.0.borrow_mut().push(down);
            Err(beckon::chord::PostingError::PermissionDenied.into())
        }
    }
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut engine = Switches::new(&[device()], Denied(calls.clone())).unwrap();
    let now = Instant::now();
    engine.event(&address(18), true, now).unwrap();
    let failures = engine.advance(
        now + Duration::from_millis(300),
        &BTreeSet::from(["pad".into()]),
    );
    assert_eq!(
        failures[0].code,
        beckon::display::DiagnosticCode::PostingDenied
    );
    assert!(!engine.output_down());
    engine
        .event(&address(18), false, now + Duration::from_millis(301))
        .unwrap();
    engine.advance(now + Duration::from_millis(302), &BTreeSet::new());
    assert_eq!(*calls.borrow(), vec![true]);
    assert_eq!(
        engine.snapshot(now + Duration::from_millis(302))[0].phase,
        beckon::switches::ControlPhase::Error
    );
}
