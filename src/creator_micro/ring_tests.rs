use super::*;
use crate::{
    core::{AgentSource, Binding},
    render::{self, attention::SourceAttention},
    source_jobs::{CachedSources, SourceExecutor, SourceResult, SourceWork},
};

/// Real private command -> executor -> authoritative cache, never a live source.
fn command_plan() -> (RenderPlan, Instant) {
    use std::{
        os::unix::fs::DirBuilderExt,
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "rc{:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    std::fs::write(
        root.join("config.toml"),
        include_str!("../../templates/config.toml"),
    )
    .unwrap();
    let mut config = crate::config::load_at(&root.join("config.toml")).unwrap();
    let mut rows = (1..=6)
        .map(|i| json!({"scope":"s","id":format!("p{i}"),"state":"idle","focused":true}))
        .collect::<Vec<_>>();
    rows.push(json!({"scope":"elsewhere","id":"unbound","state":"blocked","focused":false}));
    std::fs::write(root.join("rows.json"), serde_json::to_vec(&rows).unwrap()).unwrap();
    config.sources = vec![crate::config::AgentSourceConfig {
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
    let SourceResult::Refreshed(observations) = SourceExecutor::default()
        .execute(
            SourceWork::Refresh(config),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap()
    else {
        panic!("refresh")
    };
    let start = observations[0].last_success.unwrap();
    let mut cache = CachedSources::default();
    cache.apply(observations);
    let agents = cache.panes().unwrap();
    let bound = (1..=6)
        .map(|i| {
            let binding = Binding {
                source: "sample-source".into(),
                session: "s".into(),
                pane_id: format!("p{i}"),
                key: format!("key{i}"),
            };
            let agent = agents
                .iter()
                .find(|a| binding.matches_agent(a))
                .unwrap()
                .clone();
            (binding, agent)
        })
        .collect::<Vec<_>>();
    let mut appearance = render::DisplayConfig::default();
    for (i, key) in appearance.keys.iter_mut().enumerate() {
        key.id = format!("key{}", i + 1);
    }
    let mut plan = render::render(&appearance, &bound).unwrap();
    assert_eq!(
        plan.keys
            .iter()
            .filter(|k| k.state == Some(render::AgentState::Idle))
            .count(),
        6
    );
    let now = Instant::now();
    plan.source_attention = cache
        .observations()
        .iter()
        .map(|s| (s.id.clone(), SourceAttention::from_observation(s, now)))
        .collect();
    for name in ["config.toml", "rows.json"] {
        std::fs::remove_file(root.join(name)).unwrap();
    }
    std::fs::remove_dir(root).unwrap();
    (plan, start)
}

#[test]
fn ring_source_unbound_agent_changes_only_ring_through_actual_actor_then_expires() {
    use test_support::*;
    let (plan, start) = command_plan();
    let now = Instant::now();
    let mut config = device_config();
    config.definition.options["layers"]["rex"]["source"] = "sample-source".into();
    config.definition.options["layers"]["rex"]
        .as_table_mut()
        .unwrap()
        .insert("ring_state_scope".into(), "source".into());
    let (mut sink, mut owner) = creator(config, now, FakeContext::new()).unwrap();
    owner.status_selector(1, 3);
    sink.start_transport().unwrap();
    for _ in 0..20 {
        owner.step();
    }
    sink.poll_input_batch(
        InputPollMode::DrainOnly,
        InputPollBudget {
            deadline: now + Duration::from_millis(5),
            max_raw_frames: 128,
        },
    )
    .unwrap();
    let mut bound = sink.options.clone();
    bound.layers.get_mut("rex").unwrap().ring_state_scope = RingStateScope::Bound;
    let saved = std::mem::replace(&mut sink.options, bound);
    let baseline = sink.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
    sink.options = saved;
    sink.publish(&plan).unwrap();
    for _ in 0..50 {
        owner.step();
    }
    let sent = owner.snapshot();
    let zones = &sent
        .rpcs
        .iter()
        .rev()
        .find(|r| r["method"] == "v.oai.rgbcfg")
        .expect("actual actor zone write")["params"];
    assert_eq!(
        zones["ambient"]["c"],
        rgb_u32(plan.treatments.for_state(render::AgentState::Blocked).color)
    );
    let mut expected = baseline.zones.clone().unwrap();
    expected["ambient"]["c"] = zones["ambient"]["c"].clone();
    assert_eq!(*zones, expected, "only ambient color may change");
    let entries = &sent
        .rpcs
        .iter()
        .rev()
        .find(|r| r["method"] == "v.oai.thstatus")
        .unwrap()["params"];
    assert_eq!(*entries, baseline.entries);
    assert!(
        entries
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["id"].as_u64().unwrap() >= 6)
    );
    let clock = owner.clock();
    clock.set(start + Duration::from_millis(14999));
    assert_eq!(
        sink.lighting(&plan, &BTreeMap::new())
            .unwrap()
            .unwrap()
            .zones
            .unwrap()["ambient"]["c"],
        zones["ambient"]["c"]
    );
    clock.set(start + Duration::from_millis(15000));
    sink.publish(&plan).unwrap();
    for _ in 0..50 {
        owner.step();
    }
    let expired = owner.snapshot();
    assert_eq!(
        expired
            .rpcs
            .iter()
            .rev()
            .find(|r| r["method"] == "v.oai.rgbcfg")
            .unwrap()["params"]["ambient"]["c"],
        rgb_u32(plan.treatments.for_state(render::AgentState::Unknown).color)
    );
    assert_eq!(
        sink.binding_sources.len(),
        6,
        "unbound attention does not become navigation ownership"
    );
    sink.stop_transport();
    owner.step();
}

#[test]
fn ring_scope_source_isolation_empty_fixed_and_firmware_preserve_keys_and_navigation() {
    use crate::{
        render::{AgentState, attention::AttentionState},
        source_observation::{AgentObservation, SourceAvailability, SourceObservation},
    };
    let (mut plan, _) = command_plan();
    let now = Instant::now();
    let mut display = tests::configured();
    display.clock = Rc::new(move || now);
    display.layer(1, 3);
    display.options.layers.get_mut("rex").unwrap().source = Some("sample-source".into());
    let bound = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
    assert_eq!(
        bound.zones.as_ref().unwrap()["ambient"]["c"],
        rgb_u32(plan.treatments.for_state(AgentState::Idle).color)
    );
    display
        .options
        .layers
        .get_mut("rex")
        .unwrap()
        .ring_state_scope = RingStateScope::Source;
    let keys = std::mem::take(&mut plan.keys);
    let unbound = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
    assert_eq!(
        unbound.zones.unwrap()["ambient"]["c"],
        rgb_u32(plan.treatments.for_state(AgentState::Blocked).color)
    );
    assert!(
        unbound
            .entries
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["e"] == 0)
    );
    plan.keys = keys;
    let mut source = SourceObservation::unknown("sample-source");
    source.availability = SourceAvailability::Available;
    source.observed_at = Some(now);
    source.last_success = Some(now);
    source.complete_since = Some(now);
    let empty = SourceAttention::from_observation(&source, now);
    assert_eq!(empty.at(now), AttentionState::Empty);
    source.agents.push(AgentObservation{agent:serde_json::from_value(json!({"source":"sample-source","session":"s","pane_id":"same","agent_status":"idle","focused":false})).unwrap(),available:true,observed_at:now,title:None,title_at:None,scope_title:None,scope_title_at:None});
    let idle = SourceAttention::from_observation(&source, now);
    source.id = "other".into();
    source.agents[0].agent.source = "other".into();
    source.agents[0].agent.agent_status = "blocked".into();
    let other = SourceAttention::from_observation(&source, now);
    plan.source_attention =
        BTreeMap::from([("sample-source".into(), idle), ("other".into(), other)]);
    let isolated = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
    assert_eq!(
        isolated, bound,
        "other-source Blocked cannot change ring or keys"
    );
    plan.source_attention.remove("sample-source");
    let missing = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
    assert_eq!(
        missing.zones.unwrap()["ambient"]["c"],
        rgb_u32(plan.treatments.for_state(AgentState::Unknown).color)
    );
    plan.source_attention.insert("sample-source".into(), empty);
    let empty_frame = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
    assert_eq!(
        empty_frame.zones.as_ref().unwrap()["ambient"]["c"],
        rgb_u32(display.options.ring.color)
    );
    assert_eq!(empty_frame.entries, bound.entries);
    display.options.ring_follows_state_color = false;
    plan.source_attention.insert("sample-source".into(), other);
    assert_eq!(
        display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap(),
        empty_frame,
        "fixed option dominates source mode"
    );
    display.options.layers.get_mut("rex").unwrap().zone_lighting = ZoneLighting::Firmware;
    let firmware = display.lighting(&plan, &BTreeMap::new()).unwrap().unwrap();
    assert!(firmware.zones.is_none());
    assert_eq!(firmware.entries, bound.entries);
    assert!(
        display.binding_sources.is_empty(),
        "lighting never changes navigation authority"
    );
    display.layer(1, 2);
    assert!(display.lighting(&plan, &BTreeMap::new()).unwrap().is_none());
}
