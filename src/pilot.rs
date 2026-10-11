//! Read-only, expiring observations for the bounded Creator Micro pilot.
use crate::core::{BindingStore, Pane, PaneDirectory};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeSession {
    pub agent: String,
    pub kind: String,
    pub value: String,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Observation {
    #[serde(flatten)]
    pub pane: Pane,
    pub terminal_id: String,
    #[serde(default)]
    pub agent_session: Option<NativeSession>,
}
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub socket: String,
    pub generation: u64,
    pub refresh: u64,
    pub at: Instant,
    pub observations: Vec<Observation>,
}

pub fn decode_pair(
    panes: serde_json::Value,
    agents: serde_json::Value,
) -> Result<Vec<Observation>> {
    let panes: Vec<Observation> = serde_json::from_value(panes)?;
    let agents: Vec<Observation> = serde_json::from_value(agents)?;
    let mut agent_native_ids = std::collections::BTreeSet::new();
    for agent in &agents {
        if let Some(native) = &agent.agent_session
            && !agent_native_ids.insert((native.agent.clone(), native.value.clone()))
        {
            bail!("duplicate native agent identity in agent observations");
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut terminals = std::collections::BTreeSet::new();
    let mut native_ids = std::collections::BTreeSet::new();
    let had_panes = !panes.is_empty();
    let mut verified = Vec::new();
    for pane in panes {
        if pane.pane.pane_id.is_empty()
            || pane.terminal_id.is_empty()
            || !ids.insert(pane.pane.pane_id.clone())
            || !terminals.insert(pane.terminal_id.clone())
        {
            bail!("missing or duplicate native pane/terminal identity");
        }
        if let Some(native) = &pane.agent_session
            && !native_ids.insert((native.agent.clone(), native.value.clone()))
        {
            bail!("duplicate native agent identity");
        }
        let matches: Vec<_> = agents
            .iter()
            .filter(|agent| agent.pane.pane_id == pane.pane.pane_id)
            .collect();
        // AIDEV-NOTE: Quarantine incomplete per-pane joins; unrelated panes must not hide a verified target.
        // Global duplicate identities still invalidate the snapshot rather than choosing an arbitrary owner.
        if pane.pane.agent.is_some() {
            if matches.len() != 1 {
                continue;
            }
            let agent = matches[0];
            let Some(native) = &pane.agent_session else {
                continue;
            };
            if agent.terminal_id != pane.terminal_id
                || agent.pane.agent != pane.pane.agent
                || agent.agent_session != pane.agent_session
                || agent.pane.agent_status != pane.pane.agent_status
                || native.kind != "id"
                || native.value.is_empty()
                || Some(&native.agent) != pane.pane.agent.as_ref()
            {
                continue;
            }
        } else if pane.agent_session.is_some()
            || matches.len() > 1
            || matches.iter().any(|agent| {
                agent.terminal_id != pane.terminal_id
                    || agent.pane.agent.is_some()
                    || agent.agent_session.is_some()
            })
        {
            continue;
        }
        // Preserve normal cache status; shell metadata is normalized only in the pilot projection.
        verified.push(pane);
    }
    if agents
        .iter()
        .any(|agent| !ids.contains(&agent.pane.pane_id))
    {
        bail!("orphan agent observation");
    }
    if had_panes && verified.is_empty() {
        bail!("no verified pane observations");
    }
    Ok(verified)
}
pub fn project(
    store: &dyn BindingStore,
    directory: &dyn PaneDirectory,
    display: &crate::render::DisplayConfig,
    now: Instant,
) -> Result<serde_json::Value> {
    use serde_json::json;
    static EPOCH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!("{}-{:?}", std::process::id(), std::time::SystemTime::now())
    });
    let state = store.load()?.unwrap_or_default();
    crate::core::validate_bindings(&state.bindings)?;
    let binding = state.bindings.iter().find(|binding| binding.key == "f4");
    let mut frame =
        json!({"version":"beckon.pilot.v1","epoch":&*EPOCH,"available":false,"binding":binding});
    let Some(binding) = binding else {
        return Ok(frame);
    };
    let Some(snapshot) = directory.pilot_snapshot(&binding.session) else {
        return Ok(frame);
    };
    let age = now
        .checked_duration_since(snapshot.at)
        .unwrap_or(Duration::MAX);
    frame["source"] = json!({"session":binding.session,"socket":snapshot.socket,"generation":snapshot.generation,
        "refresh":snapshot.refresh,"age_ms":age.as_millis().min(u64::MAX as u128) as u64});
    if age >= Duration::from_secs(10) {
        return Ok(frame);
    }
    let matches: Vec<_> = snapshot
        .observations
        .iter()
        .filter(|item| item.pane.pane_id == binding.pane_id)
        .collect();
    if matches.len() != 1 {
        return Ok(frame);
    }
    let observed = matches[0];
    let mut pane = observed.pane.clone();
    if pane.agent.is_none() {
        pane.agent_status = "unknown".into();
    }
    let render = crate::render::render(display, &[(binding.clone(), pane.clone())])?;
    let key = render
        .keys
        .iter()
        .find(|key| key.key == "f4")
        .ok_or_else(|| anyhow::anyhow!("f4 render absent"))?;
    frame["available"] = json!(true);
    frame["terminal_id"] = json!(observed.terminal_id);
    frame["agent_session"] = json!(observed.agent_session);
    frame["raw_status"] = json!(pane.agent_status);
    frame["render"] = serde_json::to_value(key)?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Binding, BindingState, PaneRef, PresentationTokenWrite};
    use serde_json::json;
    use std::collections::BTreeSet;
    struct ReadOnly {
        state: BindingState,
        snapshot: Option<Snapshot>,
    }
    impl BindingStore for ReadOnly {
        fn load(&self) -> Result<Option<BindingState>> {
            Ok(Some(self.state.clone()))
        }
        fn save(&self, _: &BindingState) -> Result<()> {
            panic!("pilot wrote the ledger")
        }
    }
    impl PaneDirectory for ReadOnly {
        fn panes(&self) -> Result<Vec<Pane>> {
            panic!("pilot used mutable status path")
        }
        fn pilot_snapshot(&self, _: &str) -> Option<Snapshot> {
            self.snapshot.clone()
        }
        fn observed_sessions(&self) -> Result<BTreeSet<String>> {
            panic!("pilot pruned")
        }
        fn write_fkey(&self, _: &PaneRef, _: Option<&str>) -> Result<()> {
            panic!("write")
        }
        fn write_presentation_tokens(
            &self,
            _: &PaneRef,
            _: &str,
        ) -> Result<PresentationTokenWrite> {
            panic!("write")
        }
        fn focus_pane(&self, _: &PaneRef) -> Result<()> {
            panic!("focus")
        }
        fn send_keys(&self, _: &PaneRef, _: &[&str]) -> Result<()> {
            panic!("keys")
        }
    }
    fn wire() -> serde_json::Value {
        json!({"pane_id":"w1:t1:p1","terminal_id":"terminal-1","revision":3,"agent_status":"idle","agent":null})
    }
    #[test]
    fn shell_keeps_terminal_identity_but_has_unknown_status() {
        let pair = decode_pair(json!([wire()]), json!([])).unwrap();
        assert!(pair[0].pane.agent.is_none());
        assert_eq!(pair[0].terminal_id, "terminal-1");
    }
    #[test]
    fn rejects_duplicate_or_inconsistent_native_observations() {
        assert!(decode_pair(json!([wire(), wire()]), json!([])).is_err());
        let mut agent = wire();
        agent["agent"] = json!("codex");
        assert!(decode_pair(json!([agent]), json!([])).is_err());
        let mut missing = wire();
        missing.as_object_mut().unwrap().remove("terminal_id");
        assert!(decode_pair(json!([missing]), json!([])).is_err());
    }
    #[test]
    fn read_only_projection_expires_and_retains_dormant_binding() {
        let now = Instant::now();
        let mut source = ReadOnly {
            state: BindingState {
                state_version: 1,
                bindings: vec![Binding {
                    key: "f4".into(),
                    session: "test".into(),
                    pane_id: "w1:t1:p1".into(),
                }],
            },
            snapshot: Some(Snapshot {
                socket: "/test.sock".into(),
                generation: 2,
                refresh: 1,
                at: now,
                observations: decode_pair(json!([wire()]), json!([])).unwrap(),
            }),
        };
        let display = crate::render::DisplayConfig::default();
        let frame = project(&source, &source, &display, now).unwrap();
        assert_eq!(frame["available"], true);
        assert_eq!(frame["raw_status"], "unknown");
        assert_eq!(frame["terminal_id"], "terminal-1");
        assert_eq!(
            project(&source, &source, &display, now + Duration::from_secs(11)).unwrap()["available"],
            false
        );
        source.snapshot = None;
        let frame = project(&source, &source, &display, now).unwrap();
        assert_eq!(frame["available"], false);
        assert_eq!(frame["binding"]["pane_id"], "w1:t1:p1");
    }
    #[test]
    fn incomplete_unrelated_agents_do_not_hide_verified_target() {
        let native = json!({"pane_id":"chosen","terminal_id":"chosen-terminal","agent":"codex",
            "agent_status":"idle","agent_session":{"agent":"codex","kind":"id","value":"chosen-native"}});
        let incomplete = json!({"pane_id":"incomplete","terminal_id":"other-terminal","agent":"codex",
            "agent_status":"idle"});
        let shell = json!({"pane_id":"shell","terminal_id":"shell-terminal","agent":null,
            "agent_status":"idle"});
        let observations = decode_pair(
            json!([native.clone(), incomplete.clone(), shell.clone()]),
            json!([native.clone(), incomplete, shell]),
        )
        .unwrap();
        assert_eq!(observations.len(), 2);
        let now = Instant::now();
        let mut source = ReadOnly {
            state: BindingState {
                state_version: 1,
                bindings: vec![Binding {
                    key: "f4".into(),
                    session: "test".into(),
                    pane_id: "chosen".into(),
                }],
            },
            snapshot: Some(Snapshot {
                socket: "/test.sock".into(),
                generation: 1,
                refresh: 1,
                at: now,
                observations,
            }),
        };
        let display = crate::render::DisplayConfig::default();
        let frame = project(&source, &source, &display, now).unwrap();
        assert_eq!(frame["available"], true);
        assert_eq!(frame["agent_session"]["value"], "chosen-native");
        source.state.bindings[0].pane_id = "incomplete".into();
        assert_eq!(
            project(&source, &source, &display, now).unwrap()["available"],
            false
        );
    }

    #[test]
    fn duplicate_native_identity_in_agent_list_invalidates_snapshot() {
        let chosen = json!({"pane_id":"chosen","terminal_id":"chosen-terminal","agent":"codex",
            "agent_status":"idle","agent_session":{"agent":"codex","kind":"id","value":"same-native"}});
        let incomplete = json!({"pane_id":"incomplete","terminal_id":"other-terminal","agent":"codex",
            "agent_status":"idle"});
        let mut duplicate = incomplete.clone();
        duplicate["agent_session"] = chosen["agent_session"].clone();
        assert!(
            decode_pair(
                json!([chosen.clone(), incomplete]),
                json!([chosen, duplicate])
            )
            .is_err()
        );
    }

    #[test]
    fn requires_matching_real_native_agent_pair() {
        let agent = serde_json::json!({"pane_id":"p1","terminal_id":"t1","agent":"codex","agent_status":"blocked","agent_session":{"agent":"codex","kind":"id","value":"native-1"}});
        let pair = decode_pair(
            serde_json::json!([agent.clone()]),
            serde_json::json!([agent.clone()]),
        )
        .unwrap();
        assert_eq!(pair[0].pane.agent_status, "blocked");
        let mut changed = agent.clone();
        changed["agent_session"]["value"] = serde_json::json!("native-2");
        assert!(decode_pair(serde_json::json!([agent]), serde_json::json!([changed])).is_err());
    }
}
