use beckon::{
    chord::Chord,
    switches::{Emitter, KeyPoster, SystemEmitter},
};
use std::{cell::RefCell, rc::Rc};

type Calls = Rc<RefCell<Vec<(Chord, bool)>>>;
struct Poster(Calls);
impl KeyPoster for Poster {
    fn post(&mut self, chord: &Chord, down: bool) -> anyhow::Result<()> {
        self.0.borrow_mut().push((chord.clone(), down));
        Ok(())
    }
}
fn chord(keys: &[&str]) -> Chord {
    Chord::parse(&keys.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
}

#[test]
fn output_sharing_preserves_exact_chord_flags_and_releases_only_the_last_owner() {
    let calls = Calls::default();
    let mut emitter = SystemEmitter::with_poster(Poster(calls.clone()));
    let mic = chord(&["ctrl", "opt", "d"]);
    let enter = chord(&["enter"]);
    emitter.emit(&mic, true).unwrap();
    emitter.emit(&mic, true).unwrap();
    emitter.pulse_down(&enter).unwrap();
    emitter.emit(&enter, false).unwrap();
    emitter.emit(&mic, false).unwrap();
    assert_eq!(
        *calls.borrow(),
        vec![(mic.clone(), true), (enter.clone(), true), (enter, false)]
    );
    emitter.emit(&mic, false).unwrap();
    assert_eq!(calls.borrow().last(), Some(&(mic, false)));
}

#[test]
fn pulse_cannot_borrow_an_already_held_primary_or_release_its_owner() {
    let calls = Calls::default();
    let mut emitter = SystemEmitter::with_poster(Poster(calls.clone()));
    let enter = chord(&["enter"]);
    emitter.emit(&enter, true).unwrap();
    assert!(emitter.pulse_down(&enter).is_err());
    assert!(emitter.pulse_down(&chord(&["shift", "enter"])).is_err());
    assert_eq!(*calls.borrow(), vec![(enter.clone(), true)]);
    emitter.emit(&enter, false).unwrap();
    assert_eq!(*calls.borrow(), vec![(enter.clone(), true), (enter, false)]);
}

#[test]
fn possibly_posted_legacy_down_failure_retains_cleanup_ownership() {
    use beckon::{device::DeviceConfig, switches::Switches};
    struct Partial(Calls);
    impl KeyPoster for Partial {
        fn post(&mut self, chord: &Chord, down: bool) -> anyhow::Result<()> {
            self.0.borrow_mut().push((chord.clone(), down));
            if down {
                anyhow::bail!("unknown posting outcome")
            }
            Ok(())
        }
    }
    let calls = Calls::default();
    let device = DeviceConfig { id: "pad".into(), path: "pad.toml".into(),
        definition: toml::from_str("adapter='test'\n[keys.mic]\nmembers=['pad:1']\naction='mic'\n[actions.mic]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='hold'").unwrap() };
    let mut engine = Switches::new(
        &[device],
        SystemEmitter::with_poster(Partial(calls.clone())),
    )
    .unwrap();
    let address = "pad:1".to_string().try_into().unwrap();
    assert!(
        engine
            .event(&address, true, std::time::Instant::now())
            .is_err()
    );
    assert!(
        engine.held_for(&address),
        "failed physical gesture remains held until actual release"
    );
    assert!(
        engine.output_down(),
        "unknown down must stay retryable, not silently orphaned"
    );
    assert!(
        engine
            .advance(std::time::Instant::now(), &Default::default())
            .is_empty()
    );
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
fn r1_failed_legacy_gesture_never_replays_from_duplicates_or_group_roll() {
    use beckon::{device::DeviceConfig, switches::Switches};
    struct Once {
        calls: Calls,
        failed: bool,
        denial: bool,
    }
    impl KeyPoster for Once {
        fn post(&mut self, chord: &Chord, down: bool) -> anyhow::Result<()> {
            self.calls.borrow_mut().push((chord.clone(), down));
            if down && !self.failed {
                self.failed = true;
                if self.denial {
                    return Err(beckon::chord::PostingError::PermissionDenied.into());
                }
                anyhow::bail!("possibly accepted down");
            }
            Ok(())
        }
    }
    for denial in [false, true] {
        for cleanup in [false, true] {
            let calls = Calls::default();
            let device = DeviceConfig { id:"pad".into(),path:"pad.toml".into(), definition:toml::from_str("adapter='test'\n[keys.mic]\nmembers=['pad:1','pad:2']\naction='mic'\n[actions.mic]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='hold'").unwrap() };
            let mut engine = Switches::new(
                &[device],
                SystemEmitter::with_poster(Once {
                    calls: calls.clone(),
                    failed: false,
                    denial,
                }),
            )
            .unwrap();
            let one = "pad:1".to_string().try_into().unwrap();
            let two = "pad:2".to_string().try_into().unwrap();
            let now = std::time::Instant::now();
            assert!(engine.event(&one, true, now).is_err());
            if cleanup {
                engine.advance(now, &Default::default());
            }
            engine.event(&one, true, now).unwrap();
            engine.event(&two, true, now).unwrap();
            engine.event(&one, false, now).unwrap();
            engine.event(&two, true, now).unwrap();
            assert_eq!(
                calls.borrow().iter().filter(|(_, down)| *down).count(),
                1,
                "denial={denial}, cleanup={cleanup}"
            );
            engine.event(&two, false, now).unwrap();
            engine.advance(now, &Default::default());
            engine.event(&one, true, now).unwrap();
            engine.event(&one, false, now).unwrap();
            assert_eq!(
                calls.borrow().iter().filter(|(_, down)| *down).count(),
                2,
                "new real gesture should rearm"
            );
        }
    }
}

#[test]
fn r1_toggle_release_debt_is_visible_and_retried_while_pressed_not_like_hold_mic() {
    use beckon::{device::DeviceConfig, switches::Switches};
    struct UpOnce {
        calls: Calls,
        failed: bool,
    }
    impl KeyPoster for UpOnce {
        fn post(&mut self, chord: &Chord, down: bool) -> anyhow::Result<()> {
            self.calls.borrow_mut().push((chord.clone(), down));
            if !down && !self.failed {
                self.failed = true;
                anyhow::bail!("failed up");
            }
            Ok(())
        }
    }
    for mode in ["toggle", "hold"] {
        let calls = Calls::default();
        let device=DeviceConfig {id:"pad".into(),path:"pad.toml".into(),definition:toml::from_str(&format!("adapter='test'\n[keys.mic]\nmembers=['pad:1','pad:2']\naction='mic'\n[actions.mic]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='{mode}'")).unwrap()};
        let mut engine = Switches::new(
            &[device],
            SystemEmitter::with_poster(UpOnce {
                calls: calls.clone(),
                failed: false,
            }),
        )
        .unwrap();
        let one = "pad:1".to_string().try_into().unwrap();
        let two = "pad:2".to_string().try_into().unwrap();
        let now = std::time::Instant::now();
        let result = engine.event(&one, true, now);
        if mode == "toggle" {
            assert!(result.is_err());
            assert!(engine.snapshot(now)[0].cleanup_pending);
            engine.advance(now, &Default::default());
            assert!(!engine.snapshot(now)[0].cleanup_pending);
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .map(|(_, down)| *down)
                    .collect::<Vec<_>>(),
                vec![true, false, false]
            );
            engine.event(&one, true, now).unwrap();
            assert_eq!(calls.borrow().len(), 3);
        } else {
            result.unwrap();
            engine.event(&two, true, now).unwrap();
            engine.advance(now, &Default::default());
            assert!(!engine.snapshot(now)[0].cleanup_pending);
            assert!(engine.output_down());
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .map(|(_, down)| *down)
                    .collect::<Vec<_>>(),
                vec![true]
            );
        }
    }
}
