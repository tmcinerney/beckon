use beckon::{
    config::{self, AgentSourceConfig, Config},
    source_jobs::{CachedSources, SourceExecutor, SourceResult, SourceWork},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    config: Config,
}
impl Fixture {
    fn new(records: serde_json::Value) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ra{:x}-{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        std::fs::write(
            root.join("config.toml"),
            include_str!("../templates/config.toml"),
        )
        .unwrap();
        std::fs::write(
            root.join("rows.json"),
            serde_json::to_vec(&records).unwrap(),
        )
        .unwrap();
        let mut config = config::load_at(&root.join("config.toml")).unwrap();
        config.herdr.enabled = false;
        config.sources = vec![AgentSourceConfig {
            id: "sample-source".into(),
            list: vec![
                "/bin/cat".into(),
                root.join("rows.json").to_str().unwrap().into(),
            ],
            focus: vec![],
            activate: vec![],
            identity: None,
            refresh_ms: 1000,
        }];
        Self { root, config }
    }
    fn observe(&self) -> beckon::source_observation::SourceObservation {
        let SourceResult::Refreshed(mut rows) = SourceExecutor::default()
            .execute(
                SourceWork::Refresh(self.config.clone()),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap()
        else {
            panic!("expected observation")
        };
        let mut cache = CachedSources::default();
        cache.apply(std::mem::take(&mut rows));
        cache.observations().remove(0)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for name in ["config.toml", "rows.json"] {
            std::fs::remove_file(self.root.join(name)).unwrap();
        }
        std::fs::remove_dir(&self.root).unwrap();
    }
}

#[test]
fn ring_full_command_cohort_is_certified_after_executor_validation_including_empty() {
    for records in [
        serde_json::json!([]),
        serde_json::json!([{"scope":"other-session","id":"unbound","state":"blocked"}]),
    ] {
        let fixture = Fixture::new(records);
        let before = Instant::now();
        let observation = fixture.observe();
        let start = observation
            .complete_since
            .expect("a validated full command cohort needs original coverage proof");
        assert!(start >= before && start <= observation.last_success.unwrap());
        assert_eq!(observation.id, "sample-source");
    }
}

#[test]
fn ring_complete_attention_uses_original_cohort_and_expires_without_sampling_renewal() {
    use beckon::render::{
        AgentState,
        attention::{AttentionState, SourceAttention},
    };
    let fixture = Fixture::new(serde_json::json!([{"scope":"session","id":"idle","state":"idle"}]));
    let observation = fixture.observe();
    let start = observation.complete_since.unwrap();
    let now = observation.last_success.unwrap();
    let attention = SourceAttention::from_observation(&observation, now);
    assert_eq!(
        attention.at(start + Duration::from_millis(14999)),
        AttentionState::Observed(AgentState::Idle)
    );
    assert_eq!(
        attention.at(start + Duration::from_millis(15000)),
        AttentionState::Unknown
    );
    assert_eq!(
        attention.at(start - Duration::from_millis(1)),
        AttentionState::Unknown
    );
    let resampled =
        SourceAttention::from_observation(&observation, start + Duration::from_secs(16));
    assert_eq!(
        resampled.at(start + Duration::from_secs(16)),
        AttentionState::Unknown
    );
    let empty = Fixture::new(serde_json::json!([])).observe();
    assert_eq!(
        SourceAttention::from_observation(&empty, empty.last_success.unwrap())
            .at(empty.last_success.unwrap()),
        AttentionState::Empty
    );
}

#[test]
fn ring_command_identity_failures_cannot_certify_or_retain_current_membership() {
    use beckon::source_observation::SourceAvailability;
    for records in [
        serde_json::json!([{"scope":"s","id":"","state":"idle"}]),
        serde_json::json!([{"scope":"","id":"a","state":"idle"}]),
        serde_json::json!([{"scope":"s","id":"a","state":"idle"},{"scope":"s","id":"a","state":"blocked"}]),
        serde_json::json!([{"source":"foreign","scope":"s","id":"a","state":"idle"}]),
    ] {
        let observation = Fixture::new(records).observe();
        assert_eq!(observation.availability, SourceAvailability::Unavailable);
        assert!(observation.complete_since.is_none());
        assert!(observation.agents.is_empty());
    }
    let good =
        Fixture::new(serde_json::json!([{"scope":"s","id":"a","state":"blocked"}])).observe();
    let mut cache = CachedSources::default();
    cache.apply(vec![good.clone()]);
    cache.mark_unavailable(["sample-source".into()], Instant::now());
    let failed = cache.observations().remove(0);
    assert!(failed.complete_since.is_none());
    assert!(!failed.agents[0].available);
    assert_eq!(failed.agents[0].observed_at, good.agents[0].observed_at);
}

#[test]
fn ring_incomplete_evidence_only_fresh_blocked_can_outrank_unknown_and_latest_equal_wins() {
    use beckon::render::{
        AgentState,
        attention::{AttentionState, SourceAttention},
    };
    let mut source=Fixture::new(serde_json::json!([{"scope":"s1","id":"same","state":"working"},{"scope":"s2","id":"same","state":"working"}])).observe();
    let now = source.last_success.unwrap();
    source.complete_since = None;
    assert_eq!(
        SourceAttention::from_observation(&source, now).at(now),
        AttentionState::Unknown
    );
    source.agents[0].agent.agent_status = "blocked".into();
    source.agents[0].observed_at = now - Duration::from_secs(14);
    source.agents[1].agent.agent_status = "blocked".into();
    let attention = SourceAttention::from_observation(&source, now);
    assert_eq!(
        attention.at(now + Duration::from_millis(14999)),
        AttentionState::Observed(AgentState::Blocked)
    );
    assert_eq!(
        attention.at(now + Duration::from_millis(15000)),
        AttentionState::Unknown
    );
    source.agents[1].agent.source = "another-source".into();
    let attention = SourceAttention::from_observation(&source, now);
    assert_eq!(
        attention.at(now + Duration::from_secs(1)),
        AttentionState::Unknown,
        "foreign-source Blocked is not a positive"
    );
    source.agents[0].observed_at = now + Duration::from_secs(1);
    assert_eq!(
        SourceAttention::from_observation(&source, now).at(now + Duration::from_secs(1)),
        AttentionState::Unknown,
        "future evidence is not made fresh by later publication"
    );
    let mut complete =
        Fixture::new(serde_json::json!([{"scope":"s","id":"a","state":"working"}])).observe();
    let now = complete.last_success.unwrap();
    for missing in ["last_success", "observed_at", "complete_since"] {
        let mut incomplete = complete.clone();
        match missing {
            "last_success" => incomplete.last_success = None,
            "observed_at" => incomplete.observed_at = None,
            _ => incomplete.complete_since = None,
        };
        assert_eq!(
            SourceAttention::from_observation(&incomplete, now).at(now),
            AttentionState::Unknown
        );
    }
    complete.agents[0].observed_at = now + Duration::from_millis(1);
    assert_eq!(
        SourceAttention::from_observation(&complete, now).at(now),
        AttentionState::Unknown
    );
}

#[test]
fn ring_executor_rejects_limits_parse_exit_and_deadline_without_false_empty_then_recovers_locally()
{
    use beckon::{
        render::{
            AgentState,
            attention::{AttentionState, SourceAttention},
        },
        source_observation::SourceAvailability,
    };
    let mut invalids = Vec::new();
    invalids.push(Fixture::new(serde_json::json!((0..257).map(|i|serde_json::json!({"id":format!("p{i}"),"state":if i==256 {"blocked"} else {"idle"}})).collect::<Vec<_>>())));
    // Raw output cap and the expanded immutable observation byte cap are separate.
    for size in [1048576, 600000] {
        invalids.push(Fixture::new(
            serde_json::json!([{"id":"p","state":"idle","title":"x".repeat(size)}]),
        ));
    }
    let malformed = Fixture::new(serde_json::json!([]));
    std::fs::write(malformed.root.join("rows.json"), b"not json").unwrap();
    invalids.push(malformed);
    let mut nonzero = Fixture::new(serde_json::json!([]));
    nonzero.config.sources[0].list = vec!["/usr/bin/false".into()];
    invalids.push(nonzero);
    for fixture in invalids {
        let source = fixture.observe();
        let now = Instant::now();
        assert_eq!(source.availability, SourceAvailability::Unavailable);
        assert!(source.complete_since.is_none());
        assert_eq!(
            SourceAttention::from_observation(&source, now).at(now),
            AttentionState::Unknown
        );
    }
    let mut slow = Fixture::new(serde_json::json!([]));
    slow.config.sources[0].list = vec!["/bin/sleep".into(), "1".into()];
    let SourceResult::Refreshed(rows) = SourceExecutor::default()
        .execute(
            SourceWork::Refresh(slow.config.clone()),
            Instant::now() + Duration::from_millis(40),
        )
        .unwrap()
    else {
        panic!("refresh")
    };
    assert_eq!(rows[0].availability, SourceAvailability::Unavailable);
    assert!(rows[0].complete_since.is_none());
    let mut cache = CachedSources::default();
    let blocked =
        Fixture::new(serde_json::json!([{"scope":"old-session","id":"blocked","state":"blocked"}]))
            .observe();
    let mut other = blocked.clone();
    other.id = "other".into();
    for row in &mut other.agents {
        row.agent.source = "other".into();
    }
    cache.apply(vec![blocked, other.clone()]);
    cache.mark_unavailable(["sample-source".into()], Instant::now());
    let empty = Fixture::new(serde_json::json!([])).observe();
    cache.apply(vec![empty]);
    let observations = cache.observations();
    let recovered = observations
        .iter()
        .find(|s| s.id == "sample-source")
        .unwrap();
    let now = Instant::now();
    assert_eq!(
        SourceAttention::from_observation(recovered, now).at(now),
        AttentionState::Empty
    );
    assert!(
        recovered.agents.iter().all(|r| !r.available),
        "history must not resurrect membership"
    );
    let untouched = observations.iter().find(|s| s.id == "other").unwrap();
    assert_eq!(untouched.complete_since, other.complete_since);
    assert_eq!(
        SourceAttention::from_observation(untouched, now).at(now),
        AttentionState::Observed(AgentState::Blocked)
    );
    let full = Fixture::new(serde_json::json!(
        (0..256)
            .map(|i| serde_json::json!({"scope":"new-session","id":format!("p{i}"),"state":"idle"}))
            .collect::<Vec<_>>()
    ))
    .observe();
    cache.apply(vec![full]);
    let observations = cache.observations();
    let current = observations
        .iter()
        .find(|s| s.id == "sample-source")
        .unwrap();
    assert_eq!(current.agents.len(), 256);
    assert!(
        current.agents.iter().all(|row| row.available),
        "only history may be pruned"
    );
    assert!(current.complete_since.is_some());
    let now = Instant::now();
    assert_eq!(
        SourceAttention::from_observation(current, now).at(now),
        AttentionState::Observed(AgentState::Idle)
    );
}
