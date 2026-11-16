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
    fn posting_observation(&self) -> crate::runtime::PostingObservation {
        Default::default()
    }
    fn emit(&mut self, chord: &Chord, down: bool) -> Result<()>;
    fn pulse_down(&mut self, chord: &Chord) -> Result<()> {
        self.emit(chord, true)
    }
}

pub trait KeyPoster {
    fn post(&mut self, chord: &Chord, down: bool) -> Result<()>;
}
#[derive(Default)]
pub struct SystemPoster;
impl KeyPoster for SystemPoster {
    fn post(&mut self, chord: &Chord, down: bool) -> Result<()> {
        chord.post(down)
    }
}

#[derive(Debug)]
pub struct OutputConflict;
impl std::fmt::Display for OutputConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another output owns this primary key; release it before retrying")
    }
}
impl std::error::Error for OutputConflict {}

fn proven_no_output(error: &anyhow::Error) -> bool {
    error.downcast_ref::<OutputConflict>().is_some()
        || error.downcast_ref::<crate::chord::PostingError>().is_some()
}

pub fn output_diagnostic(error: &anyhow::Error) -> crate::display::DiagnosticCode {
    use crate::{chord::PostingError, display::DiagnosticCode};
    match error.downcast_ref::<PostingError>() {
        Some(PostingError::PermissionDenied) => DiagnosticCode::PostingDenied,
        Some(PostingError::EventCreation) => DiagnosticCode::EventCreationFailed,
        None => DiagnosticCode::OutputFailed,
    }
}

#[derive(Default)]
pub struct SystemEmitter<P: KeyPoster = SystemPoster> {
    active: BTreeMap<Chord, usize>,
    poster: P,
    observation: crate::runtime::PostingObservation,
}
impl<P: KeyPoster> SystemEmitter<P> {
    pub fn with_poster(poster: P) -> Self {
        Self {
            active: BTreeMap::new(),
            poster,
            observation: Default::default(),
        }
    }
    fn post(&mut self, chord: &Chord, down: bool) -> Result<()> {
        use crate::{
            display::{Recovery, TypedFailure},
            runtime::PostingPermission,
        };
        let result = self.poster.post(chord, down);
        let at = Instant::now();
        match &result {
            Ok(()) => {
                self.observation.permission = PostingPermission::Allowed;
                self.observation.checked_at = Some(at);
                self.observation.last_success = Some(at);
                if let Some(failure) = &mut self.observation.last_failure {
                    failure.active = false;
                }
            }
            Err(error) => {
                let code = output_diagnostic(error);
                if error.downcast_ref::<crate::chord::PostingError>().is_some() {
                    self.observation.permission =
                        if code == crate::display::DiagnosticCode::PostingDenied {
                            PostingPermission::Denied
                        } else {
                            PostingPermission::Allowed
                        };
                    self.observation.checked_at = Some(at);
                }
                self.observation.last_failure = Some(TypedFailure {
                    code,
                    at,
                    active: true,
                    recovery: if code == crate::display::DiagnosticCode::PostingDenied {
                        Recovery::GrantPostingPermission
                    } else {
                        Recovery::RetryCleanup
                    },
                });
            }
        }
        result
    }
}
impl<P: KeyPoster> Emitter for SystemEmitter<P> {
    fn posting_observation(&self) -> crate::runtime::PostingObservation {
        self.observation.clone()
    }
    fn pulse_down(&mut self, chord: &Chord) -> Result<()> {
        if self
            .active
            .keys()
            .any(|active| active.shares_primary_key(chord))
        {
            return Err(OutputConflict.into());
        }
        self.emit(chord, true)
    }
    fn emit(&mut self, chord: &Chord, down: bool) -> Result<()> {
        let count = self.active.get(chord).copied().unwrap_or(0);
        if down {
            if self
                .active
                .keys()
                .any(|active| active != chord && active.shares_primary_key(chord))
            {
                return Err(OutputConflict.into());
            }
            if count == 0
                && let Err(error) = self.post(chord, true)
            {
                // AIDEV-NOTE: an unknown posting failure may have accepted down.
                // Keep a cleanup claim; a proven preflight/creation failure cannot.
                if !proven_no_output(&error) {
                    self.active.insert(chord.clone(), 1);
                }
                return Err(error);
            }
            self.active.insert(chord.clone(), count + 1);
        } else if count > 1 {
            self.active.insert(chord.clone(), count - 1);
        } else if count == 1 {
            self.post(chord, false)?;
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
    hold_chord: Option<Chord>,
    hold_fired: bool,
    quarantined: bool,
    pulse_cleanup: Option<Chord>,
    release_debt: bool,
    last_failure: Option<crate::display::TypedFailure>,
    error_until: Option<Instant>,
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

pub type TrustedDevices = BTreeSet<String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlFailure {
    pub device: String,
    pub control: String,
    pub code: crate::display::DiagnosticCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPhase {
    Idle,
    Held,
    Latched,
    Pending,
    HoldFired,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlSnapshot {
    pub device: String,
    pub id: String,
    pub phase: ControlPhase,
    pub pressed: bool,
    pub cleanup_pending: bool,
}

impl<E: Emitter> Switches<E> {
    pub fn control_observations(&self) -> Vec<(String, String, crate::display::TypedFailure)> {
        self.controls
            .iter()
            .filter_map(|control| {
                control
                    .last_failure
                    .clone()
                    .map(|failure| (control.device.clone(), control.name.clone(), failure))
            })
            .collect()
    }
    pub fn posting_observation(&self) -> crate::runtime::PostingObservation {
        let mut result = self.emitter.posting_observation();
        result.cleanup_pending = self
            .controls
            .iter()
            .any(|control| control.release_debt || control.pulse_cleanup.is_some());
        result
    }
    pub fn slow_work_blocked(&self) -> bool {
        self.output_down()
            || self
                .controls
                .iter()
                .any(|control| control.hold_chord.is_none() && !control.held.is_empty())
    }
    pub fn pending_deadline(&self, address: &Address) -> Option<Instant> {
        self.controls
            .iter()
            .find(|control| control.members.contains(address))
            .filter(|control| control.hold_chord.is_some() && !control.quarantined)
            .and_then(|control| control.since.zip(control.tap))
            .and_then(|(since, threshold)| since.checked_add(threshold))
    }

    pub fn input_active(&self) -> bool {
        self.controls.iter().any(|control| !control.held.is_empty()) || self.output_down()
    }
    pub fn snapshot(&self, now: Instant) -> Vec<ControlSnapshot> {
        self.controls
            .iter()
            .map(|control| {
                let pressed = !control.held.is_empty();
                let phase = if control.error_until.is_some_and(|until| now < until) {
                    ControlPhase::Error
                } else if control.quarantined {
                    ControlPhase::Idle
                } else if control.hold_chord.is_some() && pressed {
                    if control.hold_fired {
                        ControlPhase::HoldFired
                    } else {
                        ControlPhase::Pending
                    }
                } else if pressed {
                    ControlPhase::Held
                } else if control.latched {
                    ControlPhase::Latched
                } else {
                    ControlPhase::Idle
                };
                ControlSnapshot {
                    device: control.device.clone(),
                    id: control.name.clone(),
                    phase,
                    pressed,
                    cleanup_pending: control.pulse_cleanup.is_some() || control.release_debt,
                }
            })
            .collect()
    }
    /// Ambiguity suppresses new two-chord output, never a legacy mic key-up.
    pub fn quarantine_down(&mut self, address: &Address) {
        for control in &mut self.controls {
            if control.members.contains(address) {
                control.held.insert(address.clone());
                control.quarantined = true;
                control.since = None;
            }
        }
    }

    /// Ambiguity suppresses new two-chord output, never a legacy mic key-up.
    pub fn cancel_pending(&mut self, device: &str) {
        for control in &mut self.controls {
            if control.device == device && control.hold_chord.is_some() {
                control.quarantined = true;
                control.since = None;
                control.hold_fired = false;
            }
        }
    }

    /// The turn controller calls this only after an exhausted clean fence.
    pub fn rearm_device(&mut self, device: &str) {
        for control in &mut self.controls {
            if control.device == device
                && control.held.is_empty()
                && control.pulse_cleanup.is_none()
                && !control.release_debt
            {
                control.quarantined = false;
            }
        }
    }
    /// Only the turn controller's clean/exhausted devices can authorize a hold.
    /// Cleanup and visual expiry are independent of gesture trust.
    pub fn advance(
        &mut self,
        now: Instant,
        trusted_devices: &TrustedDevices,
    ) -> Vec<ControlFailure> {
        self.advance_with(now, |device| {
            trusted_devices.contains(device).then_some(now)
        })
    }

    /// An actor's consumed lease authorizes only deadlines covered by its
    /// ORIGINAL empty-read observation, never a later main-thread receipt.
    pub fn advance_observed(
        &mut self,
        now: Instant,
        device: &str,
        observed_at: Instant,
    ) -> Vec<ControlFailure> {
        self.advance_with(now, |candidate| {
            (candidate == device && observed_at <= now).then_some(observed_at)
        })
    }

    pub fn next_hold_deadline(&self, device: &str) -> Option<Instant> {
        self.controls
            .iter()
            .filter(|control| {
                control.device == device
                    && control.hold_chord.is_some()
                    && !control.quarantined
                    && !control.hold_fired
                    && !control.held.is_empty()
            })
            .filter_map(|control| {
                control
                    .since
                    .zip(control.tap)
                    .and_then(|(since, threshold)| since.checked_add(threshold))
            })
            .min()
    }

    fn advance_with(
        &mut self,
        now: Instant,
        observed: impl Fn(&str) -> Option<Instant>,
    ) -> Vec<ControlFailure> {
        let mut failures = Vec::new();
        for control in &mut self.controls {
            if control.hold_chord.is_some() && control.since.is_some_and(|since| now < since) {
                control.quarantined = true;
                control.since = None;
            }
            if control.error_until.is_some_and(|until| now >= until) {
                control.error_until = None;
            }
            if control.release_debt {
                match Self::release_legacy(control, &mut self.emitter) {
                    Ok(()) => {
                        if control.held.is_empty() {
                            control.quarantined = false;
                        }
                    }
                    Err(_) => failures.push(ControlFailure {
                        device: control.device.clone(),
                        control: control.name.clone(),
                        code: crate::display::DiagnosticCode::CleanupPending,
                    }),
                }
                continue;
            }
            if let Some(chord) = control.pulse_cleanup.as_ref() {
                if self.emitter.emit(chord, false).is_ok() {
                    control.pulse_cleanup = None;
                    Self::recovered(control);
                } else {
                    failures.push(ControlFailure {
                        device: control.device.clone(),
                        control: control.name.clone(),
                        code: crate::display::DiagnosticCode::CleanupPending,
                    });
                }
                continue;
            }
            if control.hold_chord.is_none()
                || control.quarantined
                || control.hold_fired
                || control.held.is_empty()
                || observed(&control.device).is_none()
            {
                continue;
            }
            if control
                .since
                .zip(control.tap)
                .is_some_and(|(since, threshold)| {
                    observed(&control.device)
                        .is_some_and(|at| at >= since && at.duration_since(since) >= threshold)
                })
            {
                control.since = None;
                control.hold_fired = true;
                let chord = control.hold_chord.clone().expect("validated hold chord");
                if let Err(error) = Self::pulse(control, &mut self.emitter, &chord, now) {
                    failures.push(ControlFailure {
                        device: control.device.clone(),
                        control: control.name.clone(),
                        code: output_diagnostic(&error),
                    });
                }
            }
        }
        failures
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.controls
            .iter()
            .flat_map(|control| {
                let hold = if control.hold_chord.is_some()
                    && !control.quarantined
                    && !control.held.is_empty()
                {
                    control
                        .since
                        .zip(control.tap)
                        .and_then(|(since, threshold)| since.checked_add(threshold))
                } else {
                    None
                };
                [hold, control.error_until].into_iter().flatten()
            })
            .min()
    }
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
                let (chord, mode, tap, hold_chord) = match action {
                    Some(Action::Chord { keys, mode, tap_ms }) => (
                        Some(Chord::parse(keys)?),
                        Some(*mode),
                        tap_ms.map(Duration::from_millis),
                        None,
                    ),
                    Some(Action::TapHoldChords {
                        tap_keys,
                        hold_keys,
                        hold_ms,
                    }) => (
                        Some(Chord::parse(tap_keys)?),
                        None,
                        Some(Duration::from_millis(*hold_ms)),
                        Some(Chord::parse(hold_keys)?),
                    ),
                    None => (None, None, None, None),
                };
                controls.push(Control {
                    device: device.id.clone(),
                    name: name.clone(),
                    members: key.members.clone(),
                    chord,
                    mode,
                    tap,
                    hold_chord,
                    hold_fired: false,
                    quarantined: false,
                    pulse_cleanup: None,
                    release_debt: false,
                    last_failure: None,
                    error_until: None,
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
        let result = self.event_inner(address, pressed, now);
        if let Err(error) = &result {
            for control in &mut self.controls {
                if control.members.contains(address) {
                    Self::failed(control, error, now);
                }
            }
        }
        result
    }
    fn event_inner(&mut self, address: &Address, pressed: bool, now: Instant) -> Result<()> {
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
            if control.hold_chord.is_some() {
                if held {
                    if !control.quarantined && control.pulse_cleanup.is_none() {
                        control.since = Some(now);
                        control.hold_fired = false;
                    }
                } else {
                    let since = control.since.take();
                    let short = !control.quarantined
                        && !control.hold_fired
                        && since.is_some_and(|since| {
                            now >= since
                                && now.duration_since(since)
                                    < control.tap.expect("validated timing")
                        });
                    if since.is_some() && !short && !control.hold_fired {
                        // AIDEV-NOTE: processing time cannot classify a queued
                        // threshold-crossing release. Require a fresh clean fence.
                        control.quarantined = true;
                    }
                    control.hold_fired = false;
                    if short {
                        let chord = control.chord.clone().expect("validated tap chord");
                        return Self::pulse(control, &mut self.emitter, &chord, now);
                    }
                }
                return Ok(());
            }
            let Some(chord) = control.chord.clone() else {
                return Ok(());
            };
            if control.quarantined {
                if !held {
                    control.since = None;
                    control.latched = false;
                    if control.release_debt {
                        Self::release_legacy(control, &mut self.emitter)?;
                    }
                    control.quarantined = false;
                }
                return Ok(());
            }
            if held {
                control.since = Some(now);
                if control.down {
                    Self::release_legacy(control, &mut self.emitter)?;
                }
                if let Err(error) = self.emitter.emit(&chord, true) {
                    // AIDEV-NOTE: failed activation consumes the physical gesture.
                    // Membership outlives cleanup, including proven no-output denial.
                    control.since = None;
                    control.down = !proven_no_output(&error);
                    control.release_debt = control.down;
                    control.quarantined = true;
                    control.latched = false;
                    return Err(error);
                }
                control.down = true;
                Self::recovered(control);
                if control.mode == Some(Mode::Toggle) {
                    Self::release_legacy(control, &mut self.emitter)?;
                    control.latched = !control.latched;
                }
            } else {
                if control.down {
                    Self::release_legacy(control, &mut self.emitter)?;
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
            let treatment = if control.error_until.is_some() {
                control.effects.error.map(|effect| effect.treatment)
            } else if control.hold_chord.is_some()
                && !control.held.is_empty()
                && !control.quarantined
            {
                if control.hold_fired {
                    control.effects.hold_fired
                } else {
                    control.effects.pending
                }
            } else {
                control.effects.for_press(
                    !control.held.is_empty() && !control.quarantined,
                    control.latched,
                )
            };
            if let Some(treatment) = treatment {
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
        self.controls
            .iter()
            .any(|control| control.down || control.pulse_cleanup.is_some())
    }

    fn release_legacy(control: &mut Control, emitter: &mut E) -> Result<()> {
        control.release_debt = true;
        let chord = control.chord.as_ref().expect("owned legacy chord");
        match emitter.emit(chord, false) {
            Ok(()) => {
                control.down = false;
                control.release_debt = false;
                Self::recovered(control);
                Ok(())
            }
            Err(error) => {
                control.quarantined = true;
                control.since = None;
                Err(error)
            }
        }
    }

    fn pulse(control: &mut Control, emitter: &mut E, chord: &Chord, now: Instant) -> Result<()> {
        control.pulse_cleanup = Some(chord.clone());
        let result = match emitter.pulse_down(chord) {
            Ok(()) => emitter.emit(chord, false),
            Err(error) => {
                if proven_no_output(&error) {
                    control.pulse_cleanup = None;
                }
                Err(error)
            }
        };
        match result {
            Ok(()) => {
                control.pulse_cleanup = None;
                Self::recovered(control);
                Ok(())
            }
            Err(error) => {
                Self::failed(control, &error, now);
                control.quarantined = true;
                control.since = None;
                control.error_until = control
                    .effects
                    .error
                    .and_then(|effect| now.checked_add(Duration::from_millis(effect.duration_ms)));
                Err(error)
            }
        }
    }

    fn failed(control: &mut Control, error: &anyhow::Error, at: Instant) {
        let code = output_diagnostic(error);
        control.last_failure = Some(crate::display::TypedFailure {
            code,
            at,
            active: true,
            recovery: if code == crate::display::DiagnosticCode::PostingDenied {
                crate::display::Recovery::GrantPostingPermission
            } else {
                crate::display::Recovery::RetryCleanup
            },
        });
    }
    fn recovered(control: &mut Control) {
        if let Some(failure) = &mut control.last_failure {
            failure.active = false;
        }
    }

    /// Layer changes, disconnects and orderly shutdown all release held output.
    /// Failed releases remain marked down so the next reset/drop can retry.
    pub fn reset_device(&mut self, device: &str) -> Result<()> {
        let mut failure = None;
        for control in &mut self.controls {
            if control.device != device {
                continue;
            }
            if let Some(chord) = control.pulse_cleanup.as_ref() {
                match self.emitter.emit(chord, false) {
                    Ok(()) => {
                        control.pulse_cleanup = None;
                        Self::recovered(control);
                    }
                    Err(error) => failure = Some(error),
                }
            }
            if control.down
                && let Some(chord) = &control.chord
            {
                match self.emitter.emit(chord, false) {
                    Ok(()) => {
                        control.down = false;
                        control.release_debt = false;
                        Self::recovered(control);
                    }
                    Err(error) => {
                        control.release_debt = true;
                        Self::failed(control, &error, Instant::now());
                        failure = Some(
                            error.context(format!("release {}.{}", control.device, control.name)),
                        );
                    }
                }
            }
            control.held.clear();
            control.since = None;
            control.latched = false;
            control.hold_fired = false;
            control.quarantined = control.release_debt || control.pulse_cleanup.is_some();
            control.error_until = None;
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
