//! Main-thread input continuity policy. Neither this module nor a timer invents
//! physical key timestamps. Only a fresh, exhausted host sample can grant trust.
#[cfg(test)]
mod actor_integration;
#[cfg(test)]
mod tests;
use crate::{
    device::{Action, Address, DeviceConfig},
    display::{
        CaptureClaim, CaptureConsume, CaptureId, CaptureRequest, CaptureRequestOutcome,
        CaptureToken, DeviceEvent, DeviceInputBatch, DisplaySet, InputBatch, InputContinuity,
        InputPollBudget, InputPollMode, TransportEpoch, TypedFailure,
    },
    switches::{ControlFailure, Emitter, Switches, output_diagnostic},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

struct Control {
    id: String,
    binding: bool,
    two_chord: bool,
}

struct CaptureLeases<'a> {
    displays: &'a mut DisplaySet,
    tokens: Vec<(String, CaptureToken)>,
}
impl Drop for CaptureLeases<'_> {
    fn drop(&mut self) {
        for (device, token) in &self.tokens {
            self.displays.release_capture(device, *token);
        }
    }
}

struct ActorTurn<'a> {
    devices: &'a BTreeSet<String>,
    prefixes: &'a BTreeMap<String, u64>,
    samples: &'a BTreeMap<String, Instant>,
}
// Ordered prefix validation is NOT empty-transport or capture admission.
// Actor batches deliberately never set a cached drained=true timing token.
fn ordered_prefix(batch: &InputBatch, budget: InputPollBudget, now: Instant) -> bool {
    !batch.partial_frame
        && batch.continuity == InputContinuity::Continuous
        && batch.raw_frames < budget.max_raw_frames
        && now < budget.deadline
        && !batch
            .events
            .iter()
            .any(|event| matches!(event, DeviceEvent::Reset { .. }))
}

#[derive(Default, Debug)]
pub struct TurnOutcome {
    pub needs_service: bool,
    pub activations: Vec<Address>,
    pub control_failures: Vec<ControlFailure>,
    pub input_failures: Vec<(String, TypedFailure)>,
}

pub struct TurnController {
    devices: BTreeSet<String>,
    controls: BTreeMap<Address, Control>,
    clean_at: BTreeMap<String, Instant>,
    epochs: BTreeMap<String, TransportEpoch>,
    requests: BTreeMap<String, CaptureRequest>,
    applied: BTreeMap<String, u64>,
    observed_at: BTreeMap<String, Instant>,
    capture_sequence: u64,
    admitted: BTreeSet<String>,
}

impl TurnController {
    pub fn next_capture_wake(&self) -> Option<Instant> {
        self.requests
            .values()
            .map(|request| request.not_before + Duration::from_millis(5))
            .min()
    }
    pub fn new(devices: &[DeviceConfig]) -> Self {
        let mut controls = BTreeMap::new();
        for device in devices {
            for (id, key) in &device.definition.keys {
                let two_chord = key
                    .action
                    .as_ref()
                    .and_then(|id| device.definition.actions.get(id))
                    .is_some_and(|action| matches!(action, Action::TapHoldChords { .. }));
                for member in &key.members {
                    controls.insert(
                        member.clone(),
                        Control {
                            id: id.clone(),
                            binding: key.binding.is_some(),
                            two_chord,
                        },
                    );
                }
            }
        }
        Self {
            devices: devices.iter().map(|device| device.id.clone()).collect(),
            controls,
            clean_at: BTreeMap::new(),
            epochs: BTreeMap::new(),
            requests: BTreeMap::new(),
            applied: BTreeMap::new(),
            observed_at: BTreeMap::new(),
            capture_sequence: 0,
            admitted: BTreeSet::new(),
        }
    }

    pub fn service<E: Emitter>(
        &mut self,
        engine: &mut Switches<E>,
        batches: Vec<DeviceInputBatch>,
        budget: InputPollBudget,
        now: Instant,
        navigation_active: bool,
    ) -> TurnOutcome {
        self.service_input(
            engine,
            batches,
            budget,
            now,
            navigation_active,
            ActorTurn {
                devices: &BTreeSet::new(),
                prefixes: &BTreeMap::new(),
                samples: &BTreeMap::new(),
            },
        )
    }

    /// Sole daemon entry: polling/prefix application, actor admission and ONE
    /// observation-cutoff advance. No publication/IPC/job work spans admission.
    pub fn service_transports<E: Emitter>(
        &mut self,
        engine: &mut Switches<E>,
        displays: &mut DisplaySet,
        mut host_batches: Vec<DeviceInputBatch>,
        budget: InputPollBudget,
        mut clock: impl FnMut() -> Instant,
        navigation_active: bool,
    ) -> TurnOutcome {
        let now = clock();
        let mut early = engine.advance(now, &BTreeSet::new());
        let mut batches = displays.poll_input_batches(InputPollMode::DrainOnly, budget, &mut clock);
        batches.retain(|record| !host_batches.iter().any(|host| host.device == record.device));
        batches.append(&mut host_batches);
        for device in &self.devices {
            // AIDEV-NOTE: None can mean a contended metadata try-lock, not loss.
            // It grants no capture authority; Reset/epoch change/watchdog own cleanup.
            if let Some(epoch) = displays.transport_epoch(device)
                && self.epochs.get(device) != Some(&epoch)
            {
                let _ = engine.reset_device(device);
                self.clean_at.remove(device);
                self.requests.remove(device);
                self.applied.remove(device);
                self.admitted.remove(device);
                self.observed_at.insert(device.clone(), now);
                self.epochs.insert(device.clone(), epoch);
            }
        }
        let actors: BTreeSet<_> = self.epochs.keys().cloned().collect();
        let now = clock();
        let mut valid = BTreeMap::new();
        for record in &mut batches {
            if let Some(epoch) = self.epochs.get(&record.device) {
                let Some(frontier) = record.batch.event_frontier else {
                    record.batch.drained = false;
                    continue;
                };
                if record.batch.transport_epoch != Some(*epoch) || {
                    self.applied
                        .get(&record.device)
                        .is_some_and(|old| frontier < *old)
                } {
                    record.batch.drained = false;
                } else if ordered_prefix(&record.batch, budget, now) {
                    valid.insert(record.device.clone(), frontier);
                }
            }
        }
        let mut claimed = Vec::new();
        for device in &actors {
            let previous = self.observed_at.get(device).copied();
            if previous
                .is_some_and(|at| now < at || now.duration_since(at) > Duration::from_millis(50))
            {
                let _ = engine.reset_device(device);
                engine.cancel_pending(device);
                self.clean_at.remove(device);
                self.observed_at.remove(device);
                self.admitted.remove(device);
                valid.remove(device);
                for record in &mut batches {
                    if &record.device == device {
                        record.batch.drained = false;
                    }
                }
            }
            if let Some(request) = self.requests.get(device).copied() {
                if now >= request.expires_at || now < request.requested_at {
                    engine.cancel_pending(device);
                    self.admitted.remove(device);
                    self.requests.remove(device);
                } else if let Some(evidence) = displays.capture_evidence(device, request.id) {
                    let clean = evidence.request == request
                        && Some(request.epoch) == displays.transport_epoch(device)
                        && request.requested_at <= request.not_before
                        && request.not_before <= evidence.observed_at
                        && request.not_before <= evidence.started_at
                        && evidence.started_at <= evidence.observed_at
                        && evidence.observed_at <= evidence.completed_at
                        && evidence.completed_at <= now
                        && now < evidence.lease_until
                        && evidence.lease_until <= request.expires_at
                        && previous.is_none_or(|old| {
                            evidence.observed_at >= old
                                && evidence.observed_at.duration_since(old)
                                    <= Duration::from_millis(50)
                        })
                        && now.duration_since(evidence.observed_at) <= Duration::from_millis(10)
                        && evidence.raw_frames < 128
                        && !evidence.partial_frame
                        && !evidence.backlog
                        && evidence.continuity == InputContinuity::Continuous
                        && valid
                            .get(device)
                            .is_some_and(|frontier| *frontier >= evidence.event_frontier);
                    if clean {
                        if let CaptureClaim::Claimed(token) =
                            displays.try_claim_capture(device, evidence, clock())
                        {
                            if token.evidence == evidence {
                                claimed.push((device.clone(), token));
                            } else {
                                displays.release_capture(device, token);
                                engine.cancel_pending(device);
                                self.admitted.remove(device);
                            }
                        } else {
                            engine.cancel_pending(device);
                            self.admitted.remove(device);
                        }
                    } else {
                        engine.cancel_pending(device);
                        self.admitted.remove(device);
                    }
                }
            }
        }
        let mut leases = CaptureLeases {
            displays,
            tokens: claimed,
        };
        let samples = leases
            .tokens
            .iter()
            .map(|(device, token)| (device.clone(), token.evidence.observed_at))
            .collect();
        let mut outcome = self.service_input(
            engine,
            batches,
            budget,
            clock(),
            navigation_active,
            ActorTurn {
                devices: &actors,
                prefixes: &valid,
                samples: &samples,
            },
        );
        outcome.control_failures.append(&mut early);
        for (device, frontier) in &valid {
            self.applied.insert(device.clone(), *frontier);
        }
        for (device, token) in leases.tokens.clone() {
            let now = clock();
            let frontier = self.applied.get(&device).copied().unwrap_or(0);
            if !navigation_active
                && valid.contains_key(&device)
                && leases
                    .displays
                    .consume_capture(&device, token, frontier, now)
                    == CaptureConsume::Consumed
            {
                // No calls or other work between final admission and this advance.
                outcome.control_failures.extend(engine.advance_observed(
                    now,
                    &device,
                    token.evidence.observed_at,
                ));
                self.observed_at
                    .insert(device.clone(), token.evidence.observed_at);
                self.admitted.insert(device.clone());
            } else {
                engine.cancel_pending(&device);
                self.clean_at.remove(&device);
                self.admitted.remove(&device);
            }
            leases.displays.release_capture(&device, token);
            leases.tokens.retain(|(id, _)| id != &device);
            self.requests.remove(&device);
        }
        drop(leases);
        outcome
    }

    /// Call AFTER feedback publication. Submitting a new desired frame may
    /// invalidate a Ready lease; never immediately invalidate our own request.
    pub fn request_captures<E: Emitter>(
        &mut self,
        engine: &mut Switches<E>,
        displays: &mut DisplaySet,
        now: Instant,
    ) {
        for device in self.epochs.keys() {
            if self.requests.contains_key(device) {
                continue;
            }
            let Some(epoch) = displays.transport_epoch(device) else {
                continue;
            };
            let Some(sequence) = self.capture_sequence.checked_add(1) else {
                engine.cancel_pending(device);
                self.admitted.remove(device);
                continue;
            };
            self.capture_sequence = sequence;
            let not_before = engine
                .next_hold_deadline(device)
                .filter(|at| *at >= now && *at <= now + Duration::from_millis(10))
                .unwrap_or(now);
            let request = CaptureRequest {
                id: CaptureId {
                    actor: epoch.actor,
                    sequence,
                },
                epoch,
                requested_at: now,
                not_before,
                expires_at: not_before + Duration::from_millis(10),
            };
            if displays.request_capture(device, request) == CaptureRequestOutcome::Pending {
                self.requests.insert(device.clone(), request);
            }
        }
    }

    fn service_input<E: Emitter>(
        &mut self,
        engine: &mut Switches<E>,
        batches: Vec<DeviceInputBatch>,
        budget: InputPollBudget,
        now: Instant,
        navigation_active: bool,
        actor_turn: ActorTurn<'_>,
    ) -> TurnOutcome {
        let actors = actor_turn.devices;
        let mut outcome = TurnOutcome {
            needs_service: batches.iter().any(|record| {
                record.batch.raw_frames >= budget.max_raw_frames
                    || record.batch.events.len() >= 128
                    || record.batch.partial_frame
            }),
            control_failures: engine.advance(now, &BTreeSet::new()),
            ..Default::default()
        };
        let mut trusted = BTreeSet::new();
        let mut seen = BTreeSet::new();
        for record in batches {
            if !self.devices.contains(&record.device) || !seen.insert(record.device.clone()) {
                continue;
            }
            let previous = self.clean_at.get(&record.device).copied();
            let actor = actors.contains(&record.device);
            let clean = (if actor {
                actor_turn.prefixes.contains_key(&record.device)
            } else {
                record.batch.is_trusted_drain(budget, now)
            }) && record.completed_at <= now
                && previous.is_none_or(|previous| record.completed_at >= previous);
            let continuous = previous.is_some_and(|previous| {
                now >= previous && now.duration_since(previous) <= Duration::from_millis(50)
            });
            let mut edges: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
            let mut threshold_release = false;
            for event in &record.batch.events {
                if let DeviceEvent::Key { address, pressed } = event {
                    if address.device != record.device {
                        continue;
                    }
                    if let Some(control) = self.controls.get(address)
                        && control.two_chord
                        && !engine.held_for(address)
                    {
                        let flags = edges.entry(&control.id).or_default();
                        if *pressed {
                            flags.0 = true;
                        } else {
                            flags.1 = true;
                        }
                    }
                    if !pressed
                        && engine.pending_deadline(address).is_some_and(|deadline| {
                            now >= deadline && previous.is_none_or(|at| at < deadline)
                        })
                    {
                        threshold_release = true;
                    }
                }
            }
            let ambiguous = !clean
                || !continuous
                || (actors.contains(&record.device) && !self.admitted.contains(&record.device))
                || (actor && !actor_turn.samples.contains_key(&record.device) && record.batch.events.iter().any(|event|matches!(event,DeviceEvent::Key{address,pressed:false} if self.controls.get(address).is_some_and(|control|control.two_chord))))
                || navigation_active
                || threshold_release
                || edges.values().any(|(down, up)| *down && *up);
            if ambiguous {
                engine.cancel_pending(&record.device);
            } else {
                engine.rearm_device(&record.device);
                trusted.insert(record.device.clone());
            }
            for event in record.batch.events {
                match event {
                    DeviceEvent::Reset { device } if device == record.device => {
                        if let Err(error) = engine.reset_device(&device) {
                            outcome.control_failures.push(ControlFailure {
                                device: device.clone(),
                                control: String::new(),
                                code: output_diagnostic(&error),
                            });
                        }
                        engine.cancel_pending(&device);
                        trusted.remove(&device);
                    }
                    DeviceEvent::Key { address, pressed } if address.device == record.device => {
                        let held = engine.held_for(&address);
                        let Some(control) = self.controls.get(&address) else {
                            continue;
                        };
                        // Releases of owned output always win, including lost timing.
                        // Clean legacy mic taps retain their ordinary same-batch behavior.
                        if pressed
                            && !held
                            && !control.two_chord
                            && (!clean
                                || (actor && !actor_turn.samples.contains_key(&record.device)))
                        {
                            engine.quarantine_down(&address);
                            continue;
                        }
                        if let Err(error) = engine.event(&address, pressed, now) {
                            outcome.control_failures.push(ControlFailure {
                                device: record.device.clone(),
                                control: control.id.clone(),
                                code: output_diagnostic(&error),
                            });
                        } else if pressed && !held && control.binding && !ambiguous {
                            outcome.activations.push(address);
                        }
                    }
                    _ => {}
                }
            }
            if clean {
                if !actor {
                    self.clean_at.insert(record.device.clone(), now);
                } else if let Some(observed) = actor_turn.samples.get(&record.device) {
                    self.clean_at.insert(record.device.clone(), *observed);
                }
            } else {
                self.clean_at.remove(&record.device);
            }
            if let Some(failure) = record.failure {
                outcome.input_failures.push((record.device, failure));
            }
        }
        for device in self.devices.difference(&seen) {
            engine.cancel_pending(device);
            self.clean_at.remove(device);
        }
        outcome
            .control_failures
            .extend(engine.advance(now, &trusted.difference(actors).cloned().collect()));
        outcome
    }
}
