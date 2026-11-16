use super::*;
use std::time::{Duration, Instant};
use std::{cell::RefCell, rc::Rc};

fn evidence(at: Instant) -> CaptureEvidence {
    CaptureEvidence {
        request: CaptureRequest {
            id: CaptureId {
                actor: 7,
                sequence: 3,
            },
            epoch: TransportEpoch {
                actor: 7,
                connection: 2,
                selector: 4,
            },
            requested_at: at,
            not_before: at + Duration::from_millis(1),
            expires_at: at + Duration::from_millis(20),
        },
        command_revision: 9,
        ingress_frontier: 11,
        event_frontier: 10,
        started_at: at,
        observed_at: at + Duration::from_millis(2),
        completed_at: at + Duration::from_millis(3),
        lease_until: at + Duration::from_millis(12),
        raw_frames: 5,
        partial_frame: false,
        backlog: false,
        continuity: InputContinuity::Continuous,
    }
}

struct LegacyOnly;
impl DisplaySink for LegacyOnly {
    fn id(&self) -> &str {
        "legacy"
    }
    fn publish(&mut self, _: &RenderPlan) -> Result<PublishOutcome> {
        panic!("transport defaults must not publish")
    }
    fn poll_input(&mut self) -> Result<Vec<DeviceEvent>> {
        panic!("transport defaults must not poll legacy input")
    }
    fn poll_input_batch(&mut self, _: InputPollMode, _: InputPollBudget) -> Result<InputBatch> {
        panic!("capture defaults must not manufacture admission from a drain")
    }
    fn reconnect_step(&mut self, _: Instant) -> Result<ReconnectOutcome> {
        panic!("transport defaults must not reconnect")
    }
}

#[test]
fn transport_defaults_are_object_safe_unsupported_and_never_admit_or_do_io() {
    let mut sink: Box<dyn DisplaySink> = Box::new(LegacyOnly);
    let at = Instant::now();
    let evidence = evidence(at);
    let token = CaptureToken { evidence };
    let batch = InputBatch::default();
    assert_eq!(batch.transport_epoch, None);
    assert_eq!(batch.event_frontier, None);
    assert_eq!(batch.continuity, InputContinuity::Unknown);
    assert_eq!(sink.start_transport().unwrap(), TransportStart::Unsupported);
    assert_eq!(sink.transport_epoch(), None);
    assert_eq!(
        sink.request_capture(evidence.request),
        CaptureRequestOutcome::Rejected(CaptureReject::Unsupported)
    );
    assert_eq!(sink.capture_evidence(evidence.request.id), None);
    assert_eq!(
        sink.try_claim_capture(evidence, at),
        CaptureClaim::Rejected(CaptureReject::Unsupported)
    );
    assert_eq!(
        sink.consume_capture(token, 10, at),
        CaptureConsume::Rejected(CaptureReject::Unsupported)
    );
    sink.release_capture(token);
    sink.stop_transport();
    assert_eq!(sink.diagnostics(), DeviceObservation::default());
}

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Epoch,
    Request(CaptureRequest),
    Evidence(CaptureId),
    Claim(CaptureEvidence, Instant),
    Consume(CaptureToken, u64, Instant),
    Release(CaptureToken),
    Start,
    Stop,
}

struct RoutedSink {
    id: &'static str,
    calls: Rc<RefCell<Vec<Call>>>,
    evidence: CaptureEvidence,
    reject: Option<CaptureReject>,
}
impl DisplaySink for RoutedSink {
    fn id(&self) -> &str {
        self.id
    }
    fn publish(&mut self, _: &RenderPlan) -> Result<PublishOutcome> {
        Ok(PublishOutcome::Unchanged)
    }
    fn poll_input(&mut self) -> Result<Vec<DeviceEvent>> {
        panic!("no legacy fallback")
    }
    fn transport_epoch(&self) -> Option<TransportEpoch> {
        self.calls.borrow_mut().push(Call::Epoch);
        Some(self.evidence.request.epoch)
    }
    fn request_capture(&mut self, request: CaptureRequest) -> CaptureRequestOutcome {
        self.calls.borrow_mut().push(Call::Request(request));
        self.reject.map_or(
            CaptureRequestOutcome::Pending,
            CaptureRequestOutcome::Rejected,
        )
    }
    fn capture_evidence(&self, id: CaptureId) -> Option<CaptureEvidence> {
        self.calls.borrow_mut().push(Call::Evidence(id));
        (id == self.evidence.request.id).then_some(self.evidence)
    }
    fn try_claim_capture(&mut self, evidence: CaptureEvidence, now: Instant) -> CaptureClaim {
        self.calls.borrow_mut().push(Call::Claim(evidence, now));
        self.reject.map_or(
            CaptureClaim::Claimed(CaptureToken {
                evidence: self.evidence,
            }),
            CaptureClaim::Rejected,
        )
    }
    fn consume_capture(
        &mut self,
        token: CaptureToken,
        applied_through: u64,
        now: Instant,
    ) -> CaptureConsume {
        self.calls
            .borrow_mut()
            .push(Call::Consume(token, applied_through, now));
        self.reject
            .map_or(CaptureConsume::Consumed, CaptureConsume::Rejected)
    }
    fn release_capture(&mut self, token: CaptureToken) {
        self.calls.borrow_mut().push(Call::Release(token));
    }
}

#[test]
fn transport_forwarders_route_exact_arguments_and_original_evidence_only_to_named_sink() {
    let at = Instant::now();
    let evidence = evidence(at);
    let token = CaptureToken { evidence };
    let calls = Rc::new(RefCell::new(Vec::new()));
    let other_calls = Rc::new(RefCell::new(Vec::new()));
    let mut displays = DisplaySet::new(vec![
        Box::new(RoutedSink {
            id: "other",
            calls: other_calls.clone(),
            evidence,
            reject: None,
        }),
        Box::new(RoutedSink {
            id: "target",
            calls: calls.clone(),
            evidence,
            reject: None,
        }),
        Box::new(LegacyOnly),
    ]);
    assert_eq!(
        displays.transport_epoch("target"),
        Some(evidence.request.epoch)
    );
    assert_eq!(
        displays.request_capture("target", evidence.request),
        CaptureRequestOutcome::Pending
    );
    assert_eq!(
        displays.capture_evidence("target", evidence.request.id),
        Some(evidence)
    );
    let unknown = CaptureId {
        sequence: 100,
        ..evidence.request.id
    };
    assert_eq!(displays.capture_evidence("target", unknown), None);
    let receipt = at + Duration::from_millis(8);
    assert_eq!(
        displays.try_claim_capture("target", evidence, receipt),
        CaptureClaim::Claimed(token)
    );
    assert_eq!(
        displays.consume_capture("target", token, 77, receipt),
        CaptureConsume::Consumed
    );
    displays.release_capture("target", token);
    assert_eq!(
        *calls.borrow(),
        vec![
            Call::Epoch,
            Call::Request(evidence.request),
            Call::Evidence(evidence.request.id),
            Call::Evidence(unknown),
            Call::Claim(evidence, receipt),
            Call::Consume(token, 77, receipt),
            Call::Release(token)
        ]
    );
    assert!(other_calls.borrow().is_empty());
    assert_eq!(
        displays.request_capture("legacy", evidence.request),
        CaptureRequestOutcome::Rejected(CaptureReject::Unsupported)
    );
    assert_eq!(
        displays.try_claim_capture("legacy", evidence, receipt),
        CaptureClaim::Rejected(CaptureReject::Unsupported)
    );
}

#[test]
fn transport_missing_device_rejects_not_started_without_touching_any_sink() {
    let at = Instant::now();
    let evidence = evidence(at);
    let token = CaptureToken { evidence };
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut displays = DisplaySet::new(vec![Box::new(RoutedSink {
        id: "present",
        calls: calls.clone(),
        evidence,
        reject: None,
    })]);
    assert_eq!(displays.transport_epoch("missing"), None);
    assert_eq!(
        displays.capture_evidence("missing", evidence.request.id),
        None
    );
    assert_eq!(
        displays.request_capture("missing", evidence.request),
        CaptureRequestOutcome::Rejected(CaptureReject::NotStarted)
    );
    assert_eq!(
        displays.try_claim_capture("missing", evidence, at),
        CaptureClaim::Rejected(CaptureReject::NotStarted)
    );
    assert_eq!(
        displays.consume_capture("missing", token, 10, at),
        CaptureConsume::Rejected(CaptureReject::NotStarted)
    );
    displays.release_capture("missing", token);
    assert!(calls.borrow().is_empty());
    let mut empty = DisplaySet::default();
    assert_eq!(
        empty.request_capture("missing", evidence.request),
        CaptureRequestOutcome::Rejected(CaptureReject::NotStarted)
    );
}

#[test]
fn transport_forwarders_preserve_actor_rejections_without_retry_or_admission() {
    let at = Instant::now();
    let evidence = evidence(at);
    let token = CaptureToken { evidence };
    for reject in [
        CaptureReject::Unsupported,
        CaptureReject::NotStarted,
        CaptureReject::Busy,
        CaptureReject::Stale,
        CaptureReject::Expired,
        CaptureReject::Faulted,
    ] {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut displays = DisplaySet::new(vec![Box::new(RoutedSink {
            id: "target",
            calls: calls.clone(),
            evidence,
            reject: Some(reject),
        })]);
        assert_eq!(
            displays.request_capture("target", evidence.request),
            CaptureRequestOutcome::Rejected(reject)
        );
        assert_eq!(
            displays.try_claim_capture("target", evidence, at),
            CaptureClaim::Rejected(reject)
        );
        assert_eq!(
            displays.consume_capture("target", token, 10, at),
            CaptureConsume::Rejected(reject)
        );
        assert_eq!(
            *calls.borrow(),
            vec![
                Call::Request(evidence.request),
                Call::Claim(evidence, at),
                Call::Consume(token, 10, at)
            ]
        );
    }
}

#[test]
fn transport_lifecycle_runs_only_explicitly_and_reports_each_sink_in_order_despite_error() {
    struct LifecycleSink {
        id: &'static str,
        start: Option<TransportStart>,
        calls: Rc<RefCell<Vec<Call>>>,
    }
    impl DisplaySink for LifecycleSink {
        fn id(&self) -> &str {
            self.id
        }
        fn publish(&mut self, _: &RenderPlan) -> Result<PublishOutcome> {
            Ok(PublishOutcome::Unchanged)
        }
        fn start_transport(&mut self) -> Result<TransportStart> {
            self.calls.borrow_mut().push(Call::Start);
            self.start
                .ok_or_else(|| anyhow::anyhow!("fake startup failed"))
        }
        fn stop_transport(&mut self) {
            self.calls.borrow_mut().push(Call::Stop);
        }
    }
    let cases = [
        ("unsupported", Some(TransportStart::Unsupported)),
        ("failed", None),
        ("started", Some(TransportStart::Started)),
        ("running", Some(TransportStart::Running)),
        ("retiring", Some(TransportStart::Retiring)),
    ];
    let calls: Vec<_> = cases
        .iter()
        .map(|_| Rc::new(RefCell::new(Vec::new())))
        .collect();
    let mut displays = DisplaySet::new(
        cases
            .iter()
            .zip(&calls)
            .map(|((id, start), calls)| {
                Box::new(LifecycleSink {
                    id,
                    start: *start,
                    calls: calls.clone(),
                }) as Box<dyn DisplaySink>
            })
            .collect(),
    );
    let at = Instant::now();
    let token = CaptureToken {
        evidence: evidence(at),
    };
    let plan = crate::render::render(&crate::render::DisplayConfig::default(), &[]).unwrap();
    displays.diagnostics();
    displays.publish(&plan);
    displays.poll_input_batches(
        InputPollMode::DrainOnly,
        InputPollBudget {
            deadline: at + Duration::from_millis(5),
            max_raw_frames: 128,
        },
        || at,
    );
    displays.request_capture("started", token.evidence.request);
    displays.release_capture("started", token);
    assert!(calls.iter().all(|calls| calls.borrow().is_empty()));
    let results = displays.start_transports();
    assert_eq!(results.len(), cases.len());
    for ((id, result), (expected_id, expected)) in results.iter().zip(cases) {
        assert_eq!(id, expected_id);
        match expected {
            Some(start) => assert_eq!(*result.as_ref().unwrap(), start),
            None => assert_eq!(
                result.as_ref().unwrap_err().to_string(),
                "fake startup failed"
            ),
        }
    }
    assert!(
        calls
            .iter()
            .all(|calls| *calls.borrow() == vec![Call::Start])
    );
    displays.stop_transports();
    assert!(
        calls
            .iter()
            .all(|calls| *calls.borrow() == vec![Call::Start, Call::Stop])
    );
    drop(displays);
    assert!(
        calls
            .iter()
            .all(|calls| *calls.borrow() == vec![Call::Start, Call::Stop])
    );
    let mut empty = DisplaySet::default();
    assert!(empty.start_transports().is_empty());
    empty.stop_transports();
}
