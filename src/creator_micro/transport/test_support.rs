//! Crate-only fixtures for backend LIB unit tests. Never compiled for production.

use super::super::{
    CHANNEL_RPC, Clock, CreatorMicroDisplay, DeviceConfig, MAX_CHUNK, REPORT_ID, REPORT_SIZE,
};
use super::{
    Actor,
    tests::{Fake, Harness, ManualStarter},
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContextSnapshot {
    pub held: bool,
    pub waiters: usize,
    pub acquisitions: u64,
}
#[derive(Default)]
struct ContextState {
    state: Mutex<ContextSnapshot>,
    released: Condvar,
}
/// Send+Sync gate shared with a backend fake StatusWriter. No HID/global state.
#[derive(Clone, Default)]
pub(crate) struct FakeContext(Arc<ContextState>);
pub(crate) struct ContextLease(FakeContext);
impl FakeContext {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn try_enter(&self) -> Option<ContextLease> {
        let mut state = self.0.state.try_lock().ok()?;
        if state.held {
            return None;
        }
        state.held = true;
        state.acquisitions += 1;
        Some(ContextLease(self.clone()))
    }
    /// Blocking adapter for an OFF-MAIN backend fake worker only. Creator uses try_enter.
    pub(crate) fn enter(&self) -> ContextLease {
        let mut state = self.0.state.lock().unwrap();
        state.waiters += 1;
        while state.held {
            state = self.0.released.wait(state).unwrap();
        }
        state.waiters -= 1;
        state.held = true;
        state.acquisitions += 1;
        ContextLease(self.clone())
    }
    pub(crate) fn snapshot(&self) -> ContextSnapshot {
        *self.0.state.lock().unwrap()
    }
}
impl Drop for ContextLease {
    fn drop(&mut self) {
        self.0.0.state.lock().unwrap().held = false;
        self.0.0.released.notify_all();
    }
}

/// Main-thread fake monotonic clock; clones read/write the SAME clock as owner I/O.
#[derive(Clone)]
pub(crate) struct FakeClock(Rc<Cell<Instant>>);
impl FakeClock {
    pub(crate) fn now(&self) -> Instant {
        self.0.get()
    }
    pub(crate) fn set(&self, at: Instant) {
        self.0.set(at);
    }
    pub(crate) fn advance(&self, by: Duration) {
        self.set(self.now() + by);
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Gate {
    Open,
    Write,
    Close,
}
#[derive(Clone, Debug)]
pub(crate) struct OwnerSnapshot {
    pub open_polls: usize,
    pub live_handles: usize,
    pub write_reports: usize,
    pub failed_writes: usize,
    pub queued_raw_reports: usize,
    pub rpcs: Vec<Value>,
    pub exited: bool,
}
/// Owns and steps the unchanged PRODUCTION Actor. No transport or capture logic is mocked.
pub(crate) struct CreatorDriver {
    actor: Actor,
    io: Rc<Fake>,
    exited: bool,
}

pub(crate) fn device_config() -> DeviceConfig {
    super::super::tests::configured().device.clone()
}

/// Does not start transport, consume a Reset, pre-rearm controls or create a real thread.
pub(crate) fn creator(
    config: DeviceConfig,
    at: Instant,
    context: FakeContext,
) -> anyhow::Result<(CreatorMicroDisplay, CreatorDriver)> {
    let Harness {
        display: _,
        actor,
        io,
    } = Harness::new();
    io.time.set(at);
    *io.context.borrow_mut() = context;
    let mut display = CreatorMicroDisplay::new(config)?;
    let time = io.time.clone();
    display.clock = Rc::new(move || time.get()) as Clock;
    display.transport_starter = Box::new(ManualStarter(actor.endpoint.clone()));
    Ok((
        display,
        CreatorDriver {
            actor,
            io,
            exited: false,
        },
    ))
}
impl CreatorDriver {
    pub(crate) fn clock(&self) -> FakeClock {
        FakeClock(self.io.time.clone())
    }
    /// One real Actor::step, with no implicit time advance or main/controller work.
    pub(crate) fn step(&mut self) -> bool {
        if !self.exited {
            self.exited = self.actor.step(self.io.time.get());
        }
        self.exited
    }
    pub(crate) fn set_gate(&self, gate: Gate, blocked: bool) {
        match gate {
            Gate::Open => self.io.open_pending.set(blocked),
            Gate::Write => self.io.write_pending.set(blocked),
            Gate::Close => self.io.close_pending.set(blocked),
        }
    }
    pub(crate) fn push_raw(&self, report: Vec<u8>) {
        assert!(report.len() <= REPORT_SIZE);
        self.io.reads.borrow_mut().push_back(Ok(report));
    }
    /// Raw wire envelopes including sys.version/status replies and arbitrary notifications.
    pub(crate) fn push_rpc(&self, value: Value) {
        for chunk in value.to_string().as_bytes().chunks(MAX_CHUNK) {
            let mut report = vec![0; REPORT_SIZE];
            report[..3].copy_from_slice(&[REPORT_ID, CHANNEL_RPC, chunk.len() as u8]);
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            self.push_raw(report);
        }
    }
    pub(crate) fn push_key(&self, id: u8, pressed: bool) {
        self.push_rpc(
            json!({"m":"v.oai.hid","p":{"k":format!("AG{id:02}"),"act":u8::from(pressed)}}),
        );
    }
    pub(crate) fn push_selector(&self, profile: u16, layer: u16) {
        self.push_rpc(json!({"m":"kb.pl","p":{"p":profile,"l":layer}}));
    }
    pub(crate) fn fail_read(&self) {
        self.io
            .reads
            .borrow_mut()
            .push_back(Err(anyhow::anyhow!("injected Creator read failure")));
    }
    pub(crate) fn auto_status(&self, enabled: bool) {
        self.io.auto_status.set(enabled);
    }
    pub(crate) fn status_selector(&self, profile: u16, layer: u16) {
        self.io.status_selector.set((profile, layer));
    }
    pub(crate) fn stop(&self) {
        self.actor.endpoint.stop();
    }
    pub(crate) fn snapshot(&self) -> OwnerSnapshot {
        OwnerSnapshot {
            open_polls: self.io.opens.get(),
            live_handles: self.io.live.get(),
            write_reports: self.io.write_reports.get(),
            failed_writes: self.io.failed_writes.get(),
            queued_raw_reports: self.io.reads.borrow().len(),
            rpcs: self.io.writes.borrow().clone(),
            exited: self.exited,
        }
    }
}

#[cfg(test)]
mod smoke {
    use crate::{creator_micro::test_support::*, display::*};
    use std::time::{Duration, Instant};

    #[test]
    fn creator_test_harness_real_sink_and_production_capture_are_usable_from_crate_scope() {
        let start = Instant::now();
        let (display, mut owner) = creator(device_config(), start, FakeContext::new()).unwrap();
        let clock = owner.clock();
        let mut sink: Box<dyn DisplaySink> = Box::new(display);
        assert_eq!(owner.snapshot().live_handles, 0);
        assert_eq!(sink.start_transport().unwrap(), TransportStart::Started);
        owner.step();
        owner.step();
        assert_eq!(sink.diagnostics().connectivity, Connectivity::Connected);
        let budget = || InputPollBudget {
            deadline: clock.now() + Duration::from_millis(5),
            max_raw_frames: 128,
        };
        sink.poll_input_batch(InputPollMode::DrainOnly, budget())
            .unwrap();
        owner.step();
        owner.push_key(16, true);
        owner.step();
        let batch = sink
            .poll_input_batch(InputPollMode::DrainOnly, budget())
            .unwrap();
        assert!(matches!(
            batch.events.as_slice(),
            [DeviceEvent::Key { pressed: true, .. }]
        ));
        let epoch = sink.transport_epoch().unwrap();
        let req = CaptureRequest {
            id: CaptureId {
                actor: epoch.actor,
                sequence: 1,
            },
            epoch,
            requested_at: clock.now(),
            not_before: clock.now(),
            expires_at: clock.now() + Duration::from_millis(10),
        };
        assert_eq!(sink.request_capture(req), CaptureRequestOutcome::Pending);
        owner.step();
        let evidence = sink.capture_evidence(req.id).unwrap();
        for _ in 0..25 {
            assert!(!owner.step());
            assert_eq!(sink.capture_evidence(req.id), Some(evidence));
        }
        let CaptureClaim::Claimed(token) = sink.try_claim_capture(evidence, clock.now()) else {
            panic!("real owner claim");
        };
        assert_eq!(
            sink.consume_capture(token, batch.event_frontier.unwrap(), clock.now()),
            CaptureConsume::Consumed
        );
        assert!(matches!(
            sink.consume_capture(token, batch.event_frontier.unwrap(), clock.now()),
            CaptureConsume::Rejected(_)
        ));
        sink.release_capture(token);
        clock.advance(Duration::from_millis(1));
        clock.set(start + Duration::from_millis(1));
        owner.push_key(16, false);
        owner.step();
        assert!(matches!(
            sink.poll_input_batch(InputPollMode::DrainOnly, budget())
                .unwrap()
                .events
                .as_slice(),
            [DeviceEvent::Key { pressed: false, .. }]
        ));
        sink.stop_transport();
        assert!(owner.step());
        assert!(owner.snapshot().exited);
    }

    #[test]
    fn creator_test_harness_shared_context_and_io_gates_have_positive_release() {
        let context = FakeContext::new();
        let (mut sink, mut owner) =
            creator(device_config(), Instant::now(), context.clone()).unwrap();
        owner.set_gate(Gate::Open, true);
        sink.start_transport().unwrap();
        assert!(!owner.step());
        assert!(context.snapshot().held);
        assert!(context.try_enter().is_none());
        assert_eq!(context.snapshot().waiters, 0);
        owner.set_gate(Gate::Open, false);
        owner.set_gate(Gate::Write, true);
        owner.step();
        let external = context.enter(); // uncontended here; backend writer may use this on its OFF-MAIN fake worker
        assert!(context.snapshot().held);
        drop(external);
        assert!(context.snapshot().acquisitions >= 2);
        owner.set_gate(Gate::Write, false);
        owner.step();
        owner.step();
        assert_eq!(sink.diagnostics().connectivity, Connectivity::Connected);
        assert!(owner.snapshot().open_polls >= 2);
        assert!(owner.snapshot().write_reports > 0);
        assert_eq!(owner.snapshot().failed_writes, 0);
        assert_eq!(owner.snapshot().queued_raw_reports, 0);
        assert!(!owner.snapshot().rpcs.is_empty());
        owner.set_gate(Gate::Close, true);
        owner.stop();
        assert!(!owner.step());
        assert_eq!(owner.snapshot().live_handles, 1);
        owner.set_gate(Gate::Close, false);
        assert!(owner.step());
        assert_eq!(owner.snapshot().live_handles, 0);
    }

    #[test]
    fn creator_test_harness_raw_version_status_selector_and_loss_controls_are_real_input() {
        let (mut sink, mut owner) =
            creator(device_config(), Instant::now(), FakeContext::new()).unwrap();
        owner.auto_status(false);
        sink.start_transport().unwrap();
        owner.step();
        let id = owner.snapshot().rpcs[0]["id"].clone();
        owner.push_rpc(serde_json::json!({"id":99999,"result":{"version":"fake version"}}));
        owner.push_rpc(serde_json::json!({"id":id,"result":{"profile_index":1,"layer_index":6}}));
        owner.step();
        assert_eq!(sink.diagnostics().layer.unwrap().position, Some(6));
        let clock = owner.clock();
        let budget = || InputPollBudget {
            deadline: clock.now() + Duration::from_millis(5),
            max_raw_frames: 128,
        };
        sink.poll_input_batch(InputPollMode::DrainOnly, budget())
            .unwrap();
        owner.push_selector(1, 3);
        owner.step();
        assert!(
            sink.poll_input_batch(InputPollMode::DrainOnly, budget())
                .unwrap()
                .events
                .iter()
                .any(|e| matches!(e, DeviceEvent::Reset { .. }))
        );
        owner.push_raw(vec![]);
        owner.fail_read();
        owner.step();
        owner.step();
        assert_eq!(
            sink.poll_input_batch(InputPollMode::DrainOnly, budget())
                .unwrap()
                .continuity,
            InputContinuity::Lost
        );
        owner.stop();
        assert!(owner.step());
        let (mut sink, mut owner) =
            creator(device_config(), clock.now(), FakeContext::new()).unwrap();
        owner.status_selector(1, 6);
        sink.start_transport().unwrap();
        owner.step();
        owner.step();
        assert_eq!(sink.diagnostics().layer.unwrap().position, Some(6));
        owner.stop();
        owner.step();
    }
}
