use super::*;
use std::{
    cell::{Cell, RefCell},
    io::{Read, Write},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};
struct Store(Cell<usize>);
impl BindingStore for Store {
    fn load(&self) -> Result<Option<beckon::core::BindingState>> {
        self.0.set(self.0.get() + 1);
        Ok(None)
    }
    fn save(&self, _: &beckon::core::BindingState) -> Result<()> {
        panic!("read-only/stale request must never save")
    }
}
struct Poster;
impl Emitter for Poster {
    fn emit(&mut self, _: &beckon::chord::Chord, _: bool) -> Result<()> {
        panic!("read-only/stale request must never post")
    }
}
fn fixture_config(name: &str) -> (std::path::PathBuf, Config) {
    fixture_config_in(&std::env::temp_dir(), name)
}
fn fixture_config_in(parent: &std::path::Path, _name: &str) -> (std::path::PathBuf, Config) {
    use std::os::unix::fs::DirBuilderExt;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    // AIDEV-NOTE: Darwin AF_UNIX limits the entire path, including sandbox TMPDIR.
    // Compact names plus exclusive creation preserve ownership without long labels.
    let root = loop {
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("b{:x}-{sequence:x}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("reserve private fixture root: {error}"),
        }
    };
    std::fs::write(
        root.join("config.toml"),
        include_str!("../../templates/config.toml"),
    )
    .unwrap();
    let config = beckon::config::load_at(&root.join("config.toml")).unwrap();
    (root, config)
}
fn cleanup(root: std::path::PathBuf) {
    std::fs::remove_file(root.join("config.toml")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn fixture_socket_roots_fit_long_private_sandbox_parent_and_remain_unique() {
    use std::os::unix::{ffi::OsStrExt, fs::DirBuilderExt, net::SocketAddr};
    // Reserve a private parent atomically; never adopt/remove an existing path.
    let mut sequence = 0u64;
    let private = loop {
        let candidate =
            std::env::temp_dir().join(format!("n{:x}-{sequence:x}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => sequence += 1,
            Err(error) => panic!("reserve private fixture parent: {error}"),
        }
    };
    let padding = 72usize
        .saturating_sub(private.as_os_str().as_bytes().len() + 1)
        .max(1);
    let parent = private.join("s".repeat(padding));
    std::fs::create_dir(&parent).unwrap();
    let roots = [
        fixture_config_in(&parent, "unknown-ledger-refresh").0,
        fixture_config_in(&parent, "unknown-ledger-refresh").0,
    ];
    let distinct = roots[0] != roots[1];
    let mut failures = Vec::new();
    for root in roots {
        assert_eq!(root.parent(), Some(parent.as_path()));
        let path = root.join("fixture.sock");
        match SocketAddr::from_pathname(&path).and_then(|_| UnixListener::bind(&path)) {
            Ok(listener) => {
                drop(listener);
                std::fs::remove_file(path).unwrap();
            }
            Err(error) => failures.push(format!(
                "{} bytes: {error}",
                path.as_os_str().as_bytes().len()
            )),
        }
        cleanup(root);
    }
    std::fs::remove_dir(parent).unwrap();
    std::fs::remove_dir(private).unwrap();
    assert!(distinct, "each fixture owns a separate root");
    assert!(
        failures.is_empty(),
        "sandbox-style parent must permit real AF_UNIX binds: {failures:?}"
    );
}

#[test]
fn backend_review_r2_only_explicit_navigation_suppresses_gestures_for_entire_worker_lease() {
    let (root, config) = fixture_config("job-navigation-scope");
    let (release, gate) = mpsc::sync_channel(1);
    let (started, seen) = mpsc::sync_channel(1);
    let jobs = SourceJobs::spawn(move || {
        move |_: SourceWork, _| {
            started.send(()).unwrap();
            let _ = gate.recv();
            Ok(SourceResult::Executed)
        }
    })
    .unwrap();
    let mut state = State::new(
        config,
        Switches::new(&[], Poster).unwrap(),
        DisplaySet::default(),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    for kind in ["periodic", "metadata", "mutation", "navigation"] {
        let pending = match kind {
            "periodic" => Pending::Periodic {
                source: "fixture".into(),
            },
            "metadata" => Pending::Metadata,
            "mutation" => Pending::Mutation {
                peer: 999,
                request: Request::Refresh {},
                revision: "fixture".into(),
            },
            _ => Pending::Navigation {
                peer: None,
                arm: None,
            },
        };
        let now = Instant::now();
        state
            .submit(
                SourceWork::Commands(state.config.clone(), vec![]),
                pending,
                now,
            )
            .unwrap();
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            state.navigation_active(),
            kind == "navigation",
            "{kind} is not explicit navigation"
        );
        assert!(
            state
                .submit(
                    SourceWork::Commands(state.config.clone(), vec![]),
                    Pending::Metadata,
                    now
                )
                .is_err()
        );
        state.complete(now + Duration::from_secs(2));
        assert_eq!(
            state.navigation_active(),
            kind == "navigation",
            "timeout does not retire an executing navigation lease"
        );
        release.send(()).unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        while state.jobs.is_busy() {
            state.complete(Instant::now());
            assert!(Instant::now() < limit);
            std::thread::yield_now();
        }
        assert!(!state.navigation_active());
    }
    drop(state);
    cleanup(root);
}

#[test]
fn daemon_periodic_refresh_rotates_sources_and_scopes_failure_without_losing_other_evidence() {
    let (root, mut config) = fixture_config("source-fairness");
    config.herdr.enabled = false;
    config.sources = ["blocked", "healthy"]
        .into_iter()
        .map(|id| beckon::config::AgentSourceConfig {
            id: id.into(),
            list: vec!["not-executed".into()],
            focus: vec![],
            activate: vec![],
            identity: None,
            refresh_ms: 1000,
        })
        .collect();
    let (sent, seen) = mpsc::sync_channel(4);
    let jobs = SourceJobs::spawn(move || {
        move |work: SourceWork, _| {
            let SourceWork::Refresh(config) = work else {
                panic!("unexpected metadata job")
            };
            let ids = config
                .sources
                .iter()
                .map(|source| source.id.clone())
                .collect::<Vec<_>>();
            sent.send(ids.clone()).unwrap();
            if ids == ["healthy"] {
                let mut source = beckon::source_observation::SourceObservation::unknown("healthy");
                source.availability = beckon::source_observation::SourceAvailability::Available;
                source.observed_at = Some(Instant::now());
                source.last_success = source.observed_at;
                Ok(SourceResult::Refreshed(vec![source]))
            } else {
                Err("fake unavailable source".into())
            }
        }
    })
    .unwrap();
    let listener = UnixListener::bind(root.join("fixture.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut state = State::new(
        config,
        Switches::new(&[], Poster).unwrap(),
        DisplaySet::default(),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    state.step(&listener, vec![]);
    assert_eq!(
        seen.recv_timeout(Duration::from_secs(5)).unwrap(),
        ["blocked"],
        "periodic work gets one source's own deadline, never a serial all-source job"
    );
    let limit = Instant::now() + Duration::from_secs(5);
    while state
        .sources
        .observations()
        .iter()
        .all(|source| source.id != "healthy" || source.last_success.is_none())
    {
        state.step(&listener, vec![]);
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    assert_eq!(
        seen.recv_timeout(Duration::from_secs(5)).unwrap(),
        ["healthy"]
    );
    let observations = state.sources.observations();
    assert!(
        observations
            .iter()
            .find(|source| source.id == "blocked")
            .unwrap()
            .last_failure
            .as_ref()
            .unwrap()
            .active
    );
    assert!(
        observations
            .iter()
            .find(|source| source.id == "healthy")
            .unwrap()
            .last_failure
            .is_none()
    );
    drop(state);
    drop(listener);
    std::fs::remove_file(root.join("fixture.sock")).unwrap();
    cleanup(root);
}

#[test]
fn daemon_periodic_source_refresh_does_not_establish_an_authoritative_empty_ledger() {
    let (root, mut config) = fixture_config("unknown-ledger-refresh");
    config.herdr.enabled = false;
    config.sources = vec![beckon::config::AgentSourceConfig {
        id: "fake".into(),
        list: vec!["not-executed".into()],
        focus: vec![],
        activate: vec![],
        identity: None,
        refresh_ms: 1000,
    }];
    let jobs = SourceJobs::spawn(|| {
        move |_: SourceWork, _| {
            let mut observation = beckon::source_observation::SourceObservation::unknown("fake");
            observation.availability = beckon::source_observation::SourceAvailability::Available;
            Ok(SourceResult::Refreshed(vec![observation]))
        }
    })
    .unwrap();
    let listener = UnixListener::bind(root.join("fixture.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut state = State::new(
        config,
        Switches::new(&[], Poster).unwrap(),
        DisplaySet::default(),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    state.step(&listener, vec![]);
    let limit = Instant::now() + Duration::from_secs(5);
    while state.pending.is_some() {
        state.step(&listener, vec![]);
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    state.sample(Instant::now()).unwrap();
    assert!(!state.snapshot.as_ref().unwrap().ledger_available);
    drop(state);
    drop(listener);
    std::fs::remove_file(root.join("fixture.sock")).unwrap();
    cleanup(root);
}

#[test]
fn daemon_start_refuses_a_missing_transport_owner_instead_of_legacy_main_hid() {
    struct Legacy;
    impl beckon::display::DisplaySink for Legacy {
        fn id(&self) -> &str {
            "creator-micro"
        }
        fn publish(
            &mut self,
            _: &beckon::render::RenderPlan,
        ) -> Result<beckon::display::PublishOutcome> {
            panic!("must not fall back to legacy main HID")
        }
    }
    let (root, mut config) = fixture_config("owner-required");
    config.devices.push(beckon::device::DeviceConfig {
        id: "creator-micro".into(),
        path: "candidate.toml".into(),
        definition: toml::from_str(include_str!(
            "../../tests/fixtures/creator-micro-enter.toml"
        ))
        .unwrap(),
    });
    let jobs = SourceJobs::spawn(|| move |_: SourceWork, _| Err("unused fake job".into())).unwrap();
    let engine = Switches::new(&config.devices, Poster).unwrap();
    let mut state = State::new(
        config,
        engine,
        DisplaySet::new(vec![Box::new(Legacy)]),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    assert!(
        state
            .start()
            .unwrap_err()
            .to_string()
            .contains("refusing legacy")
    );
    drop(state);
    cleanup(root);
}

#[test]
fn daemon_turn_actor_barrier_actual_owner_keeps_mic_release_ahead_of_stalled_jobs_and_ipc() {
    struct Record(Rc<RefCell<Vec<bool>>>);
    impl Emitter for Record {
        fn emit(&mut self, _: &beckon::chord::Chord, down: bool) -> Result<()> {
            self.0.borrow_mut().push(down);
            Ok(())
        }
    }
    let (root, mut config) = fixture_config("turn-pressure");
    config.sources.push(beckon::config::AgentSourceConfig {
        id: "blocked-fixture".into(),
        list: vec!["not-executed".into()],
        focus: vec![],
        activate: vec![],
        identity: None,
        refresh_ms: 1000,
    });
    config.display.keys = (1..=10)
        .map(|i| beckon::render::KeyDisplay {
            id: format!("f{i}"),
            color: "#123456".parse().unwrap(),
        })
        .collect();
    config.devices.push(beckon::device::DeviceConfig{id:"mic".into(),path:"mic.toml".into(),definition:toml::from_str("adapter='test'\n[keys.mic]\nmembers=['mic:16']\naction='mic'\n[actions.mic]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='hold'").unwrap()});
    let calls = Rc::new(RefCell::new(Vec::new()));
    let engine = Switches::new(&config.devices, Record(calls.clone())).unwrap();
    let (release, gate) = mpsc::sync_channel(1);
    let (started, seen) = mpsc::sync_channel(1);
    let jobs = SourceJobs::spawn(move || {
        move |_: SourceWork, _| {
            started.send(()).unwrap();
            let _ = gate.recv();
            Ok(SourceResult::Executed)
        }
    })
    .unwrap();
    let listener = UnixListener::bind(root.join("fixture.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    struct BlockedStatus {
        gate: mpsc::Receiver<()>,
        entered: mpsc::SyncSender<()>,
        writes: Arc<std::sync::Mutex<Vec<beckon::hid::StatusSnapshot>>>,
    }
    impl beckon::hid::StatusWriter for BlockedStatus {
        fn write_snapshot(&mut self, snapshot: beckon::hid::StatusSnapshot) -> Result<()> {
            if snapshot.sequence == 0 {
                self.entered.send(()).unwrap();
                let _ = self.gate.recv();
            }
            self.writes.lock().unwrap().push(snapshot);
            Ok(())
        }
    }
    let (status_release, status_gate) = mpsc::sync_channel(1);
    let (status_entered, status_seen) = mpsc::sync_channel(1);
    let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let display = beckon::display::Glove80UsbDisplay::new(BlockedStatus {
        gate: status_gate,
        entered: status_entered,
        writes: writes.clone(),
    });
    let mut state = State::new(
        config,
        engine,
        DisplaySet::new(vec![Box::new(display)]),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    state.start().unwrap();
    state.step(&listener, vec![]);
    seen.recv_timeout(Duration::from_secs(5)).unwrap();
    status_seen.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut clients = Vec::new();
    for _ in 0..8 {
        let (server, client) = UnixStream::pair().unwrap();
        state.peers.admit_unix(server, Instant::now()).unwrap();
        clients.push(client);
    }
    assert_eq!(state.peers.len(), 8);
    for pressed in [true, false] {
        let at = Instant::now();
        let batch = DeviceInputBatch {
            device: "mic".into(),
            completed_at: at,
            failure: None,
            batch: beckon::display::InputBatch {
                events: vec![beckon::display::DeviceEvent::Key {
                    address: "mic:16".to_string().try_into().unwrap(),
                    pressed,
                }],
                drained: true,
                continuity: beckon::display::InputContinuity::Continuous,
                ..Default::default()
            },
        };
        state.step(&listener, vec![batch]);
    }
    assert_eq!(*calls.borrow(), vec![true, false]);
    assert!(!state.switches.output_down());
    assert!(state.jobs.is_busy());
    state.plan.keys[0].state = Some(beckon::render::AgentState::Working);
    state.step(&listener, vec![]);
    state.plan.keys[0].state = Some(beckon::render::AgentState::Done);
    state.step(&listener, vec![]);
    assert!(writes.lock().unwrap().is_empty());
    status_release.send(()).unwrap();
    let limit = Instant::now() + Duration::from_secs(5);
    while writes
        .lock()
        .unwrap()
        .last()
        .is_none_or(|snapshot| snapshot.slots[0] != beckon::hid::Status::Done)
    {
        state.step(&listener, vec![]);
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    assert_eq!(writes.lock().unwrap().len(), 2);
    assert_eq!(*calls.borrow(), vec![true, false]);
    release.send(()).unwrap();
    state.stop();
    drop(state);
    drop(listener);
    drop(clients);
    std::fs::remove_file(root.join("fixture.sock")).unwrap();
    cleanup(root);
}
#[test]
fn daemon_staged_mutation_expired_client_never_saves_or_replays() {
    let (root, config) = fixture_config("expired-mutation");
    let jobs =
        SourceJobs::spawn(|| move |_: SourceWork, _| Ok(SourceResult::Refreshed(vec![]))).unwrap();
    let mut state = State::new(
        config,
        Switches::new(&[], Poster).unwrap(),
        DisplaySet::default(),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    let (server, mut client) = UnixStream::pair().unwrap();
    client.write_all(b"{}\n").unwrap();
    let now = Instant::now();
    let id = state.peers.admit_unix(server, now).unwrap();
    state.peers.poll(now).unwrap();
    state.request(id, Request::ReleaseAll {}, now).unwrap();
    state.complete(now + Duration::from_secs(2));
    assert_eq!(
        state.store.0.get(),
        0,
        "expired staged work must not even load/reconcile the ledger"
    );
    assert!(state.pending.is_none());
    drop(state);
    cleanup(root);
}

#[test]
fn daemon_failed_explicit_refresh_does_not_invent_failures_for_unobserved_sources() {
    let (root, mut config) = fixture_config("refresh-no-invented-failure");
    config.sources.push(beckon::config::AgentSourceConfig {
        id: "healthy".into(),
        list: vec!["not-executed".into()],
        focus: vec![],
        activate: vec![],
        identity: None,
        refresh_ms: 1000,
    });
    let jobs = SourceJobs::spawn(|| {
        move |_: SourceWork, _| Err("aggregate fixture job failed before returning evidence".into())
    })
    .unwrap();
    let mut state = State::new(
        config,
        Switches::new(&[], Poster).unwrap(),
        DisplaySet::default(),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    let at = Instant::now();
    let mut healthy = beckon::source_observation::SourceObservation::unknown("healthy");
    healthy.availability = beckon::source_observation::SourceAvailability::Available;
    healthy.observed_at = Some(at);
    healthy.last_success = Some(at);
    state.sources.apply(vec![healthy]);
    let (server, mut client) = UnixStream::pair().unwrap();
    client.write_all(b"{}\n").unwrap();
    let peer = state.peers.admit_unix(server, at).unwrap();
    state.peers.poll(at).unwrap();
    state.request(peer, Request::Refresh {}, at).unwrap();
    let limit = Instant::now() + Duration::from_secs(5);
    while state.pending.is_some() {
        state.complete(Instant::now());
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    let cached = &state.sources.observations()[0];
    assert_eq!(cached.last_success, Some(at));
    assert!(
        cached.last_failure.is_none(),
        "job failure is not new source-specific unavailability evidence"
    );
    assert_eq!(
        cached.observed_at,
        Some(at),
        "keep original evidence ages; natural staleness still applies"
    );
    drop(state);
    cleanup(root);
}
#[test]
fn actual_daemon_runtime_status_are_cache_only_and_advertise_implemented_capabilities() {
    let (root, config) = fixture_config("read-only-owner");
    let started = Arc::new(AtomicUsize::new(0));
    let worker_count = started.clone();
    let jobs = SourceJobs::spawn(move || {
        move |_: SourceWork, _| {
            worker_count.fetch_add(1, Ordering::SeqCst);
            Err("unexpected command job".into())
        }
    })
    .unwrap();
    let mut state = State::new(
        config,
        Switches::new(&[], Poster).unwrap(),
        DisplaySet::default(),
        jobs,
        Store(Cell::new(0)),
    )
    .unwrap();
    state.sample(Instant::now()).unwrap();
    assert_eq!(state.store.0.get(), 1);
    for request in [
        Request::RuntimeSnapshot { version: 1 },
        Request::Status {},
        Request::RuntimeSnapshot { version: 999 },
    ] {
        let (server, mut client) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        client.write_all(b"{}\n").unwrap();
        let now = Instant::now();
        let id = state.peers.admit_unix(server, now).unwrap();
        assert_eq!(state.peers.poll(now).unwrap().id, id);
        state.request(id, request, now).unwrap();
        state.peers.poll(now);
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 65536];
        for _ in 0..256 {
            if let Ok(count) = client.read(&mut buffer) {
                bytes.extend_from_slice(&buffer[..count]);
            }
            if bytes.ends_with(b"\n") {
                break;
            }
            state.peers.poll(now);
        }
        let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if let Some(capabilities) = wire.pointer("/data/configuration/capabilities") {
            assert_eq!(
                capabilities["runtime_snapshot_versions"],
                serde_json::json!([1])
            );
            assert_eq!(capabilities["status_read_only"], true);
        }
    }
    assert_eq!(state.store.0.get(), 1);
    assert_eq!(started.load(Ordering::SeqCst), 0);
    drop(state);
    cleanup(root);
}
