//! Pure press/release state machine. It estimates toggle state from gestures;
//! it does not claim to observe the receiving application's state.
use crate::{
    chord::Chord,
    device::{Action, Address, DeviceConfig, Mode, Treatment},
};
use anyhow::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub trait Emitter {
    fn emit(&mut self, chord: &Chord, down: bool) -> Result<()>;
}
#[derive(Default)]
pub struct SystemEmitter {
    active: BTreeMap<Chord, usize>,
}
impl Emitter for SystemEmitter {
    fn emit(&mut self, chord: &Chord, down: bool) -> Result<()> {
        let count = self.active.get(chord).copied().unwrap_or(0);
        if down {
            if self
                .active
                .keys()
                .any(|active| active != chord && active.shares_primary_key(chord))
            {
                anyhow::bail!("another chord with this primary key is still held");
            }
            if count == 0 {
                chord.post(true)?;
            }
            self.active.insert(chord.clone(), count + 1);
        } else if count > 1 {
            self.active.insert(chord.clone(), count - 1);
        } else if count == 1 {
            chord.post(false)?;
            self.active.remove(chord);
        }
        Ok(())
    }
}

struct Control {
    device: String,
    name: String,
    members: Vec<Address>,
    chord: Option<Chord>,
    mode: Option<Mode>,
    tap: Option<Duration>,
    effects: crate::device::Effects,
    held: BTreeSet<Address>,
    since: Option<Instant>,
    latched: bool,
    down: bool,
}

pub struct Switches<E: Emitter> {
    controls: Vec<Control>,
    emitter: E,
}

impl<E: Emitter> Switches<E> {
    pub fn refresh_presentation(&mut self, devices: &[DeviceConfig]) {
        for control in &mut self.controls {
            if let Some(device) = devices.iter().find(|device| device.id == control.device)
                && let Some(key) = device.definition.keys.get(&control.name)
            {
                control.effects = key
                    .effects
                    .as_ref()
                    .and_then(|name| device.definition.effects.get(name))
                    .cloned()
                    .unwrap_or_default();
            }
        }
    }
    pub fn new(devices: &[DeviceConfig], emitter: E) -> Result<Self> {
        let mut controls = Vec::new();
        for device in devices {
            for (name, key) in &device.definition.keys {
                let action = key
                    .action
                    .as_ref()
                    .map(|name| &device.definition.actions[name]);
                let (chord, mode, tap) = match action {
                    Some(Action::Chord { keys, mode, tap_ms }) => (
                        Some(Chord::parse(keys)?),
                        Some(*mode),
                        tap_ms.map(Duration::from_millis),
                    ),
                    None => (None, None, None),
                };
                controls.push(Control {
                    device: device.id.clone(),
                    name: name.clone(),
                    members: key.members.clone(),
                    chord,
                    mode,
                    tap,
                    effects: key
                        .effects
                        .as_ref()
                        .map(|name| device.definition.effects[name].clone())
                        .unwrap_or_default(),
                    held: BTreeSet::new(),
                    since: None,
                    latched: false,
                    down: false,
                });
            }
        }
        Ok(Self { controls, emitter })
    }

    /// Duplicate press/release reports and overlapping members are idempotent.
    pub fn event(&mut self, address: &Address, pressed: bool, now: Instant) -> Result<()> {
        for control in &mut self.controls {
            if !control.members.contains(address) {
                continue;
            }
            let was_held = !control.held.is_empty();
            if pressed {
                control.held.insert(address.clone());
            } else {
                control.held.remove(address);
            }
            let held = !control.held.is_empty();
            if was_held == held {
                return Ok(());
            }
            let Some(chord) = &control.chord else {
                return Ok(());
            };
            if held {
                control.since = Some(now);
                if control.down {
                    self.emitter.emit(chord, false)?;
                    control.down = false;
                }
                if let Err(error) = self.emitter.emit(chord, true) {
                    control.held.clear();
                    control.since = None;
                    return Err(error);
                }
                control.down = true;
                if control.mode == Some(Mode::Toggle) {
                    self.emitter.emit(chord, false)?;
                    control.down = false;
                    control.latched = !control.latched;
                }
            } else {
                if control.down {
                    self.emitter.emit(chord, false)?;
                    control.down = false;
                }
                match control.mode {
                    Some(Mode::TapHold) => {
                        let short = control.since.take().is_some_and(|since| {
                            now.saturating_duration_since(since)
                                < control.tap.expect("validated threshold")
                        });
                        control.latched = short && !control.latched;
                    }
                    Some(Mode::Hold) => control.latched = false,
                    _ => {}
                }
            }
            return Ok(());
        }
        Ok(())
    }

    pub fn treatments(&self) -> BTreeMap<Address, Treatment> {
        let mut result = BTreeMap::new();
        for control in &self.controls {
            if let Some(treatment) = control
                .effects
                .for_press(!control.held.is_empty(), control.latched)
            {
                for member in &control.members {
                    result.insert(member.clone(), treatment);
                }
            }
        }
        result
    }

    pub fn held_for(&self, address: &Address) -> bool {
        self.controls
            .iter()
            .find(|control| control.members.contains(address))
            .is_some_and(|control| !control.held.is_empty())
    }

    pub fn output_down(&self) -> bool {
        self.controls.iter().any(|control| control.down)
    }

    /// Layer changes, disconnects and orderly shutdown all release held output.
    /// Failed releases remain marked down so the next reset/drop can retry.
    pub fn reset_device(&mut self, device: &str) -> Result<()> {
        let mut failure = None;
        for control in &mut self.controls {
            if control.device != device {
                continue;
            }
            if control.down
                && let Some(chord) = &control.chord
            {
                match self.emitter.emit(chord, false) {
                    Ok(()) => control.down = false,
                    Err(error) => {
                        failure = Some(
                            error.context(format!("release {}.{}", control.device, control.name)),
                        );
                    }
                }
            }
            control.held.clear();
            control.since = None;
            control.latched = false;
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl<E: Emitter> Drop for Switches<E> {
    fn drop(&mut self) {
        let devices: BTreeSet<_> = self.controls.iter().map(|c| c.device.clone()).collect();
        for device in devices {
            let _ = self.reset_device(&device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    struct Fake(Rc<RefCell<Vec<bool>>>);
    impl Emitter for Fake {
        fn emit(&mut self, _: &Chord, down: bool) -> Result<()> {
            self.0.borrow_mut().push(down);
            Ok(())
        }
    }
    fn address(id: u8) -> Address {
        format!("pad:{id}").try_into().unwrap()
    }
    fn setup(mode: &str) -> (Switches<Fake>, Rc<RefCell<Vec<bool>>>) {
        let timing = if mode == "tap-hold" { "tap_ms=300" } else { "" };
        let definition = toml::from_str(&format!("adapter='test'\n[keys.key11]\nmembers=['pad:16','pad:17']\naction='dictate'\neffects='record'\n[actions.dictate]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='{mode}'\n{timing}\n[effects.record]\nheld={{color='#FF0000',brightness=1.0,motion='steady'}}\nlatched={{color='#FF0000',brightness=1.0,motion='breathe'}}")).unwrap();
        let device = DeviceConfig {
            id: "pad".into(),
            path: "pad.toml".into(),
            definition,
        };
        let calls = Rc::new(RefCell::new(Vec::new()));
        (
            Switches::new(&[device], Fake(calls.clone())).unwrap(),
            calls,
        )
    }
    #[test]
    fn quick_taps_latch_both_keys_then_clear() {
        let (mut switches, calls) = setup("tap-hold");
        let now = Instant::now();
        for expected in [2, 0] {
            switches.event(&address(16), true, now).unwrap();
            switches
                .event(&address(16), false, now + Duration::from_millis(100))
                .unwrap();
            assert_eq!(switches.treatments().len(), expected);
            assert!(
                switches
                    .treatments()
                    .values()
                    .all(|t| t.motion == crate::render::Motion::Breathe)
            );
        }
        assert_eq!(*calls.borrow(), [true, false, true, false]);
    }
    #[test]
    fn hold_roll_and_duplicate_reports_emit_only_one_pair() {
        let (mut switches, calls) = setup("tap-hold");
        let now = Instant::now();
        for (id, pressed) in [(16, true), (16, true), (17, true), (16, false), (16, false)] {
            switches.event(&address(id), pressed, now).unwrap();
        }
        assert_eq!(*calls.borrow(), [true]);
        assert_eq!(switches.treatments().len(), 2);
        switches
            .event(&address(17), false, now + Duration::from_millis(300))
            .unwrap();
        assert_eq!(*calls.borrow(), [true, false]);
        assert!(switches.treatments().is_empty());
    }
    #[test]
    fn hold_and_toggle_are_distinct() {
        for mode in ["hold", "toggle"] {
            let (mut switches, calls) = setup(mode);
            let now = Instant::now();
            switches.event(&address(16), true, now).unwrap();
            assert_eq!(calls.borrow().len(), if mode == "hold" { 1 } else { 2 });
            switches.event(&address(16), false, now).unwrap();
            assert_eq!(switches.treatments().is_empty(), mode == "hold");
        }
    }
    #[test]
    fn disconnect_or_layer_change_releases_a_held_chord() {
        let (mut switches, calls) = setup("hold");
        switches.event(&address(16), true, Instant::now()).unwrap();
        switches.reset_device("pad").unwrap();
        assert_eq!(*calls.borrow(), [true, false]);
        assert!(switches.treatments().is_empty());
    }

    #[test]
    fn long_hold_clears_a_previous_latch_and_drop_releases_output() {
        let (mut switches, calls) = setup("tap-hold");
        let now = Instant::now();
        switches.event(&address(16), true, now).unwrap();
        switches
            .event(&address(16), false, now + Duration::from_millis(50))
            .unwrap();
        switches
            .event(&address(17), true, now + Duration::from_secs(1))
            .unwrap();
        switches
            .event(&address(17), false, now + Duration::from_secs(2))
            .unwrap();
        assert!(switches.treatments().is_empty());
        switches
            .event(&address(16), true, now + Duration::from_secs(3))
            .unwrap();
        drop(switches);
        assert_eq!(*calls.borrow(), [true, false, true, false, true, false]);
    }

    #[test]
    fn denied_key_down_does_not_leave_a_held_or_latched_light() {
        struct Denied;
        impl Emitter for Denied {
            fn emit(&mut self, _: &Chord, _: bool) -> Result<()> {
                anyhow::bail!("denied")
            }
        }
        let (template, _) = setup("tap-hold");
        let mut switches = Switches {
            controls: Vec::new(),
            emitter: Denied,
        };
        // Move the fixture control without cloning active runtime state.
        let mut template = template;
        switches.controls.append(&mut template.controls);
        assert!(switches.event(&address(16), true, Instant::now()).is_err());
        assert!(switches.treatments().is_empty());
    }

    #[test]
    fn a_failed_key_up_remains_retryable() {
        struct Flaky {
            fail: bool,
            calls: Rc<RefCell<Vec<bool>>>,
        }
        impl Emitter for Flaky {
            fn emit(&mut self, _: &Chord, down: bool) -> Result<()> {
                self.calls.borrow_mut().push(down);
                if !down && self.fail {
                    self.fail = false;
                    anyhow::bail!("temporary release failure");
                }
                Ok(())
            }
        }
        let (mut template, _) = setup("hold");
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut switches = Switches {
            controls: std::mem::take(&mut template.controls),
            emitter: Flaky {
                fail: true,
                calls: calls.clone(),
            },
        };
        let now = Instant::now();
        switches.event(&address(16), true, now).unwrap();
        assert!(switches.event(&address(16), false, now).is_err());
        assert!(switches.output_down());
        switches.reset_device("pad").unwrap();
        assert!(!switches.output_down());
        assert_eq!(*calls.borrow(), [true, false, false]);
    }

    #[test]
    fn editing_feedback_preserves_the_held_chord_and_group_state() {
        let (mut switches, calls) = setup("tap-hold");
        let now = Instant::now();
        switches.event(&address(16), true, now).unwrap();
        let treatment = Treatment {
            color: "#123456".parse().unwrap(),
            brightness: 0.5,
            motion: crate::render::Motion::Steady,
        };
        let key = crate::device::Key {
            members: switches.controls[0].members.clone(),
            binding: None,
            action: None,
            effects: Some("record".into()),
        };
        let device = DeviceConfig {
            id: "pad".into(),
            path: "pad.toml".into(),
            definition: crate::device::DeviceFile {
                adapter: "test".into(),
                options: Default::default(),
                keys: BTreeMap::from([("key11".into(), key)]),
                actions: Default::default(),
                effects: BTreeMap::from([(
                    "record".into(),
                    crate::device::Effects {
                        held: Some(treatment),
                        ..Default::default()
                    },
                )]),
            },
        };
        switches.refresh_presentation(&[device]);
        assert!(switches.output_down() && switches.held_for(&address(16)));
        assert_eq!(switches.treatments()[&address(17)], treatment);
        assert_eq!(*calls.borrow(), [true]);
        switches
            .event(&address(16), false, now + Duration::from_secs(1))
            .unwrap();
        assert_eq!(*calls.borrow(), [true, false]);
    }
}
