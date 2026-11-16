use beckon::{
    config,
    core::{Agent, Binding, BindingState, STATE_VERSION},
    runtime::{BuildIdentity, LedgerRead, PostingObservation, RuntimeInputs, RuntimeState},
    source_observation::{AgentObservation, SourceAvailability, SourceObservation},
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

fn configuration() -> beckon::config::ConfigurationModel {
    fixture_configuration(false)
}
fn fixture_configuration(latch: bool) -> beckon::config::ConfigurationModel {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "beckon-runtime-fixture-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("config.d")).unwrap();
    let mut value: toml::Value = toml::from_str(include_str!("../templates/config.toml")).unwrap();
    value["bindings"] =
        toml::Value::try_from((1..=10).map(|i| format!("key{i}")).collect::<Vec<_>>()).unwrap();
    value["sources"] = toml::from_str::<toml::Value>(
        "sources=[{id='rex',list=['false'],focus=[],activate=[],refresh_ms=1000}]",
    )
    .unwrap()["sources"]
        .clone();
    value["herdr"]["enabled"] = toml::Value::Boolean(true);
    std::fs::write(root.join("config.toml"), toml::to_string(&value).unwrap()).unwrap();
    let mut device: toml::Value =
        toml::from_str(include_str!("fixtures/creator-micro-enter.toml")).unwrap();
    if latch {
        device["actions"]["dictate"]["mode"] = "tap-hold".into();
        device["actions"]["dictate"]
            .as_table_mut()
            .unwrap()
            .insert("tap_ms".into(), 300.into());
        for (state, motion) in [("idle", "steady"), ("latched", "breathe")] {
            device["effects"]["dictation"]
                .as_table_mut()
                .unwrap()
                .insert(
                    state.into(),
                    toml::from_str::<toml::Value>(&format!(
                        "color='#FF0000'\nbrightness=0.5\nmotion='{motion}'"
                    ))
                    .unwrap(),
                );
        }
    }
    let text = if latch {
        toml::to_string(&device).unwrap()
    } else {
        include_str!("fixtures/creator-micro-enter.toml").to_owned()
    };
    std::fs::write(root.join("config.d/creator-micro.toml"), text).unwrap();
    let mut loaded = config::load_at(&root.join("config.toml")).unwrap();
    loaded.devices[0].path = "/fixture/config.d/creator-micro.toml".into();
    let model = loaded.model().unwrap();
    std::fs::remove_file(root.join("config.d/creator-micro.toml")).unwrap();
    std::fs::remove_dir(root.join("config.d")).unwrap();
    std::fs::remove_file(root.join("config.toml")).unwrap();
    std::fs::remove_dir(root).unwrap();
    model
}

#[test]
fn runtime_diagnostics_are_bounded_typed_and_reply_does_not_freshen_evidence_or_leak_raw_agent_fields()
 {
    use beckon::display::{DiagnosticCode, Recovery, TypedFailure};
    let now = Instant::now();
    let mut runtime = RuntimeState::new("event-fixture".into(), BuildIdentity::compiled());
    for index in 0..70 {
        runtime.record(
            &TypedFailure {
                code: DiagnosticCode::OutputFailed,
                at: now,
                active: true,
                recovery: Recovery::ReconnectDevice,
            },
            Some(format!("device-{index}")),
            None,
            None,
            now,
        );
    }
    let mut data = inputs(now, false);
    data.sources[0].agents[0]
        .agent
        .tokens
        .insert("private".into(), "argv-secret".into());
    data.sources[0].agents[0].agent.cwd = Some("stderr-secret".into());
    let snapshot = runtime.capture(data, now, 1794614400000).unwrap();
    let reply = snapshot
        .reply_line(1, now + Duration::from_millis(500))
        .unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(wire["data"]["events"].as_array().unwrap().len(), 64);
    assert_eq!(wire["data"]["age_ms"], 500);
    assert_eq!(wire["data"]["bindings"][0]["labels_age_ms"], 100);
    let text = String::from_utf8(reply).unwrap();
    assert!(!text.contains("argv-secret"));
    assert!(!text.contains("stderr-secret"));
    let failure = TypedFailure {
        code: DiagnosticCode::PostingDenied,
        at: now,
        active: true,
        recovery: Recovery::GrantPostingPermission,
    };
    let first = runtime.record(&failure, None, None, None, now);
    let duplicate = runtime.record(&failure, None, None, None, now + Duration::from_millis(1));
    assert_eq!(first.id, duplicate.id);
    assert_eq!(duplicate.count, 1);
    let recovered = runtime.record(
        &TypedFailure {
            active: false,
            ..failure
        },
        None,
        None,
        None,
        now + Duration::from_millis(2),
    );
    assert!(!recovered.active);
    assert_eq!(recovered.id, first.id);
}

#[test]
fn runtime_producer_fixtures_follow_real_fake_emitter_pending_fired_denied_and_recovered_states() {
    use beckon::{
        chord::{Chord, PostingError},
        display::{DiagnosticCode, Recovery, TypedFailure},
        runtime::PostingPermission,
        switches::{Emitter, Switches},
    };
    use std::{cell::Cell, collections::BTreeSet, rc::Rc};
    struct Poster(Rc<Cell<bool>>);
    impl Emitter for Poster {
        fn emit(&mut self, _: &Chord, _: bool) -> anyhow::Result<()> {
            if self.0.get() {
                Err(PostingError::PermissionDenied.into())
            } else {
                Ok(())
            }
        }
    }
    fn fixture(name: &str, bytes: Vec<u8>) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        if std::env::var_os("BECKON_UPDATE_FIXTURES").is_some() {
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    let now = Instant::now();
    let denied = Rc::new(Cell::new(false));
    let model = configuration();
    let device_id = model.devices[0].id.clone();
    let address = model.devices[0].definition.keys["send-or-newline"].members[0].clone();
    let mut engine = Switches::new(&model.devices, Poster(denied.clone())).unwrap();
    let mut runtime = RuntimeState::new("phase-fixture".into(), BuildIdentity::compiled());
    engine.event(&address, true, now).unwrap();
    let mut data = inputs(now, false);
    data.controls = engine.snapshot(now);
    let pending = runtime.capture(data, now, 1794614400000).unwrap();
    assert_eq!(
        pending.devices[0]
            .controls
            .iter()
            .find(|c| c.id == "send-or-newline")
            .unwrap()
            .phase,
        beckon::switches::ControlPhase::Pending
    );
    fixture(
        "runtime-v1-pending.json",
        pending.reply_line(1, now).unwrap(),
    );
    let fired_at = now + Duration::from_millis(300);
    engine.advance(fired_at, &BTreeSet::from([device_id.clone()]));
    let mut data = inputs(now, false);
    data.controls = engine.snapshot(fired_at);
    let fired = runtime.capture(data, fired_at, 1794614400300).unwrap();
    assert_eq!(
        fired.devices[0]
            .controls
            .iter()
            .find(|c| c.id == "send-or-newline")
            .unwrap()
            .phase,
        beckon::switches::ControlPhase::HoldFired
    );
    fixture(
        "runtime-v1-fired.json",
        fired.reply_line(1, fired_at).unwrap(),
    );
    engine.event(&address, false, fired_at).unwrap();
    denied.set(true);
    let down = now + Duration::from_millis(400);
    engine.event(&address, true, down).unwrap();
    let error_at = now + Duration::from_millis(700);
    let failures = engine.advance(error_at, &BTreeSet::from([device_id]));
    assert_eq!(failures[0].code, DiagnosticCode::PostingDenied);
    let failure = TypedFailure {
        code: DiagnosticCode::PostingDenied,
        at: error_at,
        active: true,
        recovery: Recovery::GrantPostingPermission,
    };
    let mut data = inputs(now, false);
    data.controls = engine.snapshot(error_at);
    data.posting = PostingObservation {
        permission: PostingPermission::Denied,
        checked_at: Some(error_at),
        last_failure: Some(failure.clone()),
        ..Default::default()
    };
    let error = runtime.capture(data, error_at, 1794614400700).unwrap();
    assert_eq!(
        error.devices[0]
            .controls
            .iter()
            .find(|c| c.id == "send-or-newline")
            .unwrap()
            .phase,
        beckon::switches::ControlPhase::Error
    );
    fixture(
        "runtime-v1-denied.json",
        error.reply_line(1, error_at).unwrap(),
    );
    engine.event(&address, false, error_at).unwrap();
    engine.rearm_device(&address.device);
    denied.set(false);
    let recovered_at = now + Duration::from_millis(1400);
    engine.advance(recovered_at, &BTreeSet::new());
    engine.event(&address, true, recovered_at).unwrap();
    engine
        .event(&address, false, recovered_at + Duration::from_millis(1))
        .unwrap();
    let mut data = inputs(now, false);
    data.controls = engine.snapshot(recovered_at + Duration::from_millis(1));
    data.posting = PostingObservation {
        permission: PostingPermission::Allowed,
        checked_at: Some(recovered_at),
        last_success: Some(recovered_at),
        last_failure: Some(TypedFailure {
            active: false,
            ..failure
        }),
        cleanup_pending: false,
    };
    let recovered = runtime
        .capture(data, recovered_at + Duration::from_millis(1), 1794614401401)
        .unwrap();
    assert_eq!(
        recovered.devices[0]
            .controls
            .iter()
            .find(|c| c.id == "send-or-newline")
            .unwrap()
            .phase,
        beckon::switches::ControlPhase::Idle
    );
    assert!(!recovered.posting.last_error.as_ref().unwrap().active);
    fixture(
        "runtime-v1-recovered.json",
        recovered
            .reply_line(1, recovered_at + Duration::from_millis(1))
            .unwrap(),
    );
    fixture(
        "runtime-v1-stale.json",
        recovered
            .reply_line(1, recovered_at + Duration::from_millis(16000))
            .unwrap(),
    );

    let model = fixture_configuration(true);
    let microphone = model.devices[0].definition.keys["microphone"].members[0].clone();
    let mut engine = Switches::new(&model.devices, Poster(denied)).unwrap();
    engine.event(&microphone, true, now).unwrap();
    let mut data = inputs(now, false);
    data.configuration = fixture_configuration(true);
    data.controls = engine.snapshot(now);
    let held = runtime.capture(data, now, 1794614400000).unwrap();
    assert_eq!(
        held.devices[0]
            .controls
            .iter()
            .find(|c| c.id == "microphone")
            .unwrap()
            .phase,
        beckon::switches::ControlPhase::Held
    );
    fixture("runtime-v1-held.json", held.reply_line(1, now).unwrap());
    let release = now + Duration::from_millis(20);
    engine.event(&microphone, false, release).unwrap();
    let mut data = inputs(now, false);
    data.configuration = model;
    data.controls = engine.snapshot(release);
    let latched = runtime.capture(data, release, 1794614400020).unwrap();
    assert_eq!(
        latched.devices[0]
            .controls
            .iter()
            .find(|c| c.id == "microphone")
            .unwrap()
            .phase,
        beckon::switches::ControlPhase::Latched
    );
    fixture(
        "runtime-v1-latched.json",
        latched.reply_line(1, release).unwrap(),
    );
}
fn row(source: &str, available: bool, at: Instant, scope: &str, title: &str) -> AgentObservation {
    let agent:Agent=serde_json::from_value(serde_json::json!({"source":source,"session":"same-scope","pane_id":"same-pane","agent_status":"working","label":title})).unwrap();
    AgentObservation {
        agent,
        available,
        observed_at: at,
        title: Some(title.into()),
        title_at: Some(at),
        scope_title: Some(scope.into()),
        scope_title_at: Some(at),
    }
}
fn inputs(now: Instant, invalid: bool) -> RuntimeInputs {
    let live = row(
        "rex",
        true,
        now - Duration::from_millis(100),
        "Engineering",
        "Build engine",
    );
    let dormant = row(
        "herdr",
        false,
        now - Duration::from_millis(24000),
        "Historic workspace",
        "Review feedback",
    );
    let mut rex = SourceObservation::unknown("rex");
    rex.availability = SourceAvailability::Available;
    rex.observed_at = Some(live.observed_at);
    rex.last_success = Some(live.observed_at);
    rex.agents.push(live);
    let mut herdr = SourceObservation::unknown("herdr");
    herdr.availability = SourceAvailability::Unavailable;
    herdr.observed_at = Some(now - Duration::from_millis(3000));
    herdr.last_success = Some(dormant.observed_at);
    herdr.agents.push(dormant);
    RuntimeInputs {
        configuration: configuration(),
        ledger: if invalid {
            LedgerRead::Invalid
        } else {
            LedgerRead::Available(BindingState {
                state_version: STATE_VERSION,
                bindings: vec![
                    Binding {
                        key: "key1".into(),
                        source: "rex".into(),
                        session: "same-scope".into(),
                        pane_id: "same-pane".into(),
                    },
                    Binding {
                        key: "key2".into(),
                        source: "herdr".into(),
                        session: "same-scope".into(),
                        pane_id: "same-pane".into(),
                    },
                ],
            })
        },
        sources: vec![rex, herdr],
        devices: BTreeMap::new(),
        controls: Vec::new(),
        posting: PostingObservation::default(),
    }
}

#[test]
fn runtime_snapshot_producer_is_complete_read_only_and_keeps_dormant_owners() {
    let now = Instant::now();
    let mut runtime = RuntimeState::new("fixture-daemon".into(), BuildIdentity::compiled());
    let snapshot = runtime
        .capture(inputs(now, false), now, 1794614400000)
        .unwrap();
    let bytes = snapshot.reply_line(1, now).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        wire["data"]["configuration_revision"],
        wire["data"]["configuration"]["revision"]
    );
    assert_eq!(wire["data"]["bindings"][0]["availability"], "live");
    assert_eq!(wire["data"]["bindings"][1]["availability"], "dormant");
    assert_eq!(wire["data"]["bindings"][1]["owner"]["source"], "herdr");
    assert_eq!(
        wire["data"]["bindings"][1]["scope_title"],
        "Historic workspace"
    );
    assert_eq!(wire["data"]["bindings"][1]["labels_age_ms"], 24000);
    assert_eq!(wire["data"]["devices"][0]["connectivity"], "unknown");
    assert_eq!(wire["data"]["posting"]["permission"], "unknown");
    assert_eq!(
        wire["data"]["configuration"]["capabilities"]["runtime_snapshot_versions"],
        serde_json::json!([1])
    );
    assert_eq!(
        wire["data"]["configuration"]["capabilities"]["status_read_only"],
        true
    );
    let legacy = snapshot.legacy_status_value();
    assert_eq!(legacy["bindings"].as_array().unwrap().len(), 1);
    assert_eq!(legacy["bindings"][0]["pane"]["source"], "rex");
    assert_eq!(legacy["panes"][0]["binding"], "KEY1");
    assert!(wire["data"]["configuration"]["devices"][0]["definition"]["effects"]["enter-cue"]["pending"].is_object());
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-v1.json");
    if std::env::var_os("BECKON_UPDATE_FIXTURES").is_some() {
        std::fs::write(&fixture, &bytes).unwrap();
    }
    assert_eq!(
        std::fs::read(fixture).unwrap(),
        bytes,
        "fixture must be exact real Rust serializer output"
    );
    let invalid = runtime
        .capture(inputs(now, true), now, 1794614400000)
        .unwrap();
    let bytes = invalid.reply_line(1, now).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["data"]["ledger_available"], false);
    assert!(wire["data"]["events"][0]["device"].is_null());
    assert!(
        wire["data"]["events"][0]["control"].is_null(),
        "ledger diagnostics must not invent a device/control id"
    );
    assert!(
        wire["data"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["availability"] == "unknown")
    );
    for (name, bytes) in [
        ("runtime-v1-invalid-ledger.json", bytes),
        (
            "runtime-unsupported.json",
            invalid.reply_line(999, now).unwrap(),
        ),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        if std::env::var_os("BECKON_UPDATE_FIXTURES").is_some() {
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
