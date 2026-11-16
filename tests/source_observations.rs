use beckon::{
    command_source::CommandSource, config::AgentSourceConfig, core::AgentSource,
    source_observation::SourceAvailability,
};

#[test]
fn source_observation_reads_cached_scope_labels_without_refreshing_or_running_commands() {
    let root = std::env::temp_dir().join(format!("beckon-observe-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("source.json");
    std::fs::write(&file,r#"[{"scope":"opaque","scope_title":"Engineering","id":"block-a","title":"Review engine","state":"working"}]"#).unwrap();
    let source = CommandSource::new(AgentSourceConfig {
        id: "example".into(),
        list: vec!["/bin/cat".into(), file.to_str().unwrap().into()],
        focus: vec![],
        activate: vec![],
        identity: None,
        refresh_ms: 1,
    });
    assert_eq!(
        source.observation().availability,
        SourceAvailability::Unknown
    );
    source.panes().unwrap();
    let observed = source.observation();
    assert_eq!(observed.availability, SourceAvailability::Available);
    assert_eq!(
        observed.agents[0].scope_title.as_deref(),
        Some("Engineering")
    );
    assert_eq!(observed.agents[0].title.as_deref(), Some("Review engine"));
    std::fs::write(&file, "not valid source JSON").unwrap();
    let read = source.observation();
    assert_eq!(read.last_success, observed.last_success);
    assert_eq!(
        read.agents[0].scope_title_at,
        observed.agents[0].scope_title_at
    );
    assert!(
        read.last_failure.is_none(),
        "cache getter must not run now-invalid fake command"
    );
    source.refresh();
    assert_eq!(
        source.observation().availability,
        SourceAvailability::Unknown
    );
    assert!(source.panes().is_err());
    let failed = source.observation();
    assert_eq!(failed.availability, SourceAvailability::Unavailable);
    assert_eq!(failed.agents[0].scope_title.as_deref(), Some("Engineering"));
    assert_eq!(failed.last_success, observed.last_success);
    assert!(!failed.agents[0].available);
    assert!(failed.last_failure.is_some());
    std::fs::write(&file,r#"[{"scope":"opaque","scope_title":"Renamed workspace","id":"block-a","title":"Review engine","state":"working"}]"#).unwrap();
    source.refresh();
    source.panes().unwrap();
    let recovered = source.observation();
    assert_eq!(recovered.availability, SourceAvailability::Available);
    assert!(
        !recovered.last_failure.as_ref().unwrap().active,
        "recovery keeps historical error, not active failure"
    );
    assert_eq!(
        recovered.agents[0].scope_title.as_deref(),
        Some("Renamed workspace")
    );
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn herdr_normal_snapshot_joins_real_workspace_label_without_rekeying_session() {
    use beckon::{herdr::HerdrSocket, pane_cache::PaneCache};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };
    let root = std::env::temp_dir().join(format!("beckon-label-socket-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("fixture.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap()["method"],
            "session.snapshot"
        );
        writeln!(stream,"{}",serde_json::json!({"result":{"snapshot":{"workspaces":[{"workspace_id":"w9","label":"Engineering"}],"panes":[{"pane_id":"p1","workspace_id":"w9","agent_status":"working","label":"Review engine"}]}}})).unwrap();
    });
    let socket = HerdrSocket::new(path.clone());
    let rows = socket.labelled_panes().unwrap();
    server.join().unwrap();
    let cache = PaneCache::new("server-scope", Vec::new());
    cache.replace_labelled(rows);
    let observed = cache.observation();
    assert_eq!(
        observed.agents[0].scope_title.as_deref(),
        Some("Engineering")
    );
    assert_eq!(observed.agents[0].agent.session, "server-scope");
    assert_eq!(observed.agents[0].agent.pane_id, "p1");
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}
#[test]
fn daemon_source_results_apply_only_on_owner_and_metadata_is_staged_without_commands() {
    use beckon::{
        core::AgentSource, source_jobs::CachedSources, source_observation::SourceObservation,
    };
    let mut cache = CachedSources::default();
    assert!(cache.panes().unwrap().is_empty());
    let mut observation = SourceObservation::unknown("rex");
    observation.availability = beckon::source_observation::SourceAvailability::Available;
    let at = std::time::Instant::now();
    observation.observed_at = Some(at);
    observation.last_success = Some(at);
    let agent:beckon::core::Agent=serde_json::from_value(serde_json::json!({"source":"rex","session":"s","pane_id":"p","agent_status":"idle","label":"Real label"})).unwrap();
    observation.observed_sessions.insert("s".into());
    observation
        .agents
        .push(beckon::source_observation::AgentObservation {
            agent: agent.clone(),
            available: true,
            observed_at: at,
            title: Some("Real label".into()),
            title_at: Some(at),
            scope_title: None,
            scope_title_at: None,
        });
    cache.apply(vec![observation]);
    assert_eq!(cache.panes().unwrap(), vec![agent.clone()]);
    cache.write_fkey(&agent.reference(), Some("key1")).unwrap();
    assert_eq!(cache.take_commands().len(), 1);
    assert!(cache.take_commands().is_empty());
    let mut failed = SourceObservation::unknown("rex");
    failed.availability = beckon::source_observation::SourceAvailability::Unavailable;
    cache.apply(vec![failed]);
    assert!(cache.panes().unwrap().is_empty());
    assert_eq!(
        cache.observations()[0].agents[0].title.as_deref(),
        Some("Real label")
    );
    assert!(!cache.observations()[0].agents[0].available);
}

#[test]
fn daemon_partial_source_result_preserves_unrelated_evidence_and_failure_scope() {
    use beckon::{source_jobs::CachedSources, source_observation::SourceObservation};
    let at = std::time::Instant::now();
    let mut first = SourceObservation::unknown("first");
    first.availability = SourceAvailability::Available;
    first.observed_at = Some(at);
    first.last_success = Some(at);
    first.observed_sessions.insert("first-scope".into());
    let mut second = first.clone();
    second.id = "second".into();
    let mut cache = CachedSources::default();
    cache.apply(vec![first.clone(), second]);
    cache.mark_unavailable(["second".into()], at);
    cache.apply(vec![first]);
    let observations = cache.observations();
    assert_eq!(
        observations.len(),
        2,
        "a scoped success must not erase another source's error/history"
    );
    let failed = observations
        .iter()
        .find(|source| source.id == "second")
        .unwrap();
    assert_eq!(failed.availability, SourceAvailability::Unavailable);
    assert!(failed.last_failure.as_ref().unwrap().active);
    let success = observations
        .iter()
        .find(|source| source.id == "first")
        .unwrap();
    assert_eq!(success.last_success, Some(at));
    assert!(success.last_failure.is_none());
}

#[test]
fn explicit_source_refresh_reserves_independent_budget_after_a_stalled_fixture_process() {
    use beckon::source_jobs::{SourceExecutor, SourceResult, SourceWork};
    let root = std::env::temp_dir().join(format!("beckon-source-budget-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("config.toml"),
        include_str!("../templates/config.toml"),
    )
    .unwrap();
    let mut config = beckon::config::load_at(&root.join("config.toml")).unwrap();
    config.herdr.enabled = false;
    config.sources = [
        (
            "blocked",
            vec!["/bin/sh".into(), "-c".into(), "while :; do :; done".into()],
        ),
        ("healthy", vec!["/usr/bin/printf".into(), "[]".into()]),
    ]
    .into_iter()
    .map(|(id, list)| AgentSourceConfig {
        id: id.into(),
        list,
        focus: vec![],
        activate: vec![],
        identity: None,
        refresh_ms: 1000,
    })
    .collect();
    let result = SourceExecutor::default()
        .execute(
            SourceWork::Refresh(config),
            std::time::Instant::now() + std::time::Duration::from_millis(600),
        )
        .unwrap();
    let SourceResult::Refreshed(observations) = result else {
        panic!("expected observations")
    };
    assert_eq!(
        observations[0].availability,
        SourceAvailability::Unavailable
    );
    assert_eq!(
        observations[1].availability,
        SourceAvailability::Available,
        "one timed-out source must not consume the next source's entire budget"
    );
    assert!(observations[1].last_success.is_some());
    std::fs::remove_file(root.join("config.toml")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
