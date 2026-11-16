//! Config-driven agent source. All knowledge of a particular integration
//! (its ids, its state vocabulary, how to focus it) lives in configuration and
//! in the commands that configuration names. Nothing here knows about Rex,
//! Herdr, or any other product.
//!
//! AIDEV-NOTE: `list` is a snapshot command, not a stream. A short TTL keeps
//! the daemon's 50ms render tick from spawning a process on every frame while
//! still reacting to state changes within a second.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{
    config::AgentSourceConfig,
    core::{Agent, AgentRef, AgentSource, DEFAULT_SESSION},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentRecord {
    /// Source-defined grouping. Omitted records use the default scope.
    #[serde(default = "default_scope")]
    scope: String,
    /// Opaque, source-defined id. Beckon never interprets it.
    id: String,
    /// Beckon's canonical state vocabulary: idle, working, blocked, done,
    /// unknown. Sources translate their own states before emitting this.
    #[serde(default)]
    state: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    scope_title: Option<String>,
    #[serde(default)]
    focused: bool,
    /// Optional binding token the source already stores on the agent.
    #[serde(default)]
    token: Option<String>,
}

fn default_scope() -> String {
    DEFAULT_SESSION.to_string()
}

struct Cache {
    at: Instant,
    agents: Vec<Agent>,
    scope_titles: BTreeMap<AgentRef, String>,
}

pub struct CommandSource {
    config: AgentSourceConfig,
    cache: RefCell<Option<Cache>>,
    invalidated: Cell<bool>,
    last_failure: RefCell<Option<crate::display::TypedFailure>>,
}

impl CommandSource {
    pub fn new(config: AgentSourceConfig) -> Self {
        Self {
            config,
            cache: RefCell::new(None),
            invalidated: Cell::new(false),
            last_failure: RefCell::new(None),
        }
    }

    fn ttl(&self) -> Duration {
        Duration::from_millis(self.config.refresh_ms)
    }

    fn snapshot(&self) -> Result<Vec<Agent>> {
        if let Some(cache) = self.cache.borrow().as_ref()
            && !self.invalidated.get()
            && cache.at.elapsed() < self.ttl()
        {
            return Ok(cache.agents.clone());
        }
        let cache = match self.list() {
            Ok(cache) => cache,
            Err(error) => {
                *self.last_failure.borrow_mut() = Some(crate::display::TypedFailure {
                    code: if error.downcast_ref::<serde_json::Error>().is_some() {
                        crate::display::DiagnosticCode::SourceSnapshotInvalid
                    } else {
                        crate::display::DiagnosticCode::SourceUnavailable
                    },
                    at: Instant::now(),
                    active: true,
                    recovery: crate::display::Recovery::CheckSource,
                });
                self.invalidated.set(true);
                return Err(error);
            }
        };
        let agents = cache.agents.clone();
        *self.cache.borrow_mut() = Some(cache);
        self.invalidated.set(false);
        if let Some(failure) = self.last_failure.borrow_mut().as_mut() {
            failure.active = false;
        }
        Ok(agents)
    }

    fn list(&self) -> Result<Cache> {
        let output = run(&self.config.list, &BTreeMap::new())
            .with_context(|| format!("sources.{} list command", self.config.id))?;
        let records: Vec<AgentRecord> = serde_json::from_slice(&output).with_context(|| {
            format!(
                "sources.{} list output is not an agent array",
                self.config.id
            )
        })?;
        let mut scope_titles = BTreeMap::new();
        let agents = records
            .into_iter()
            .map(|record| {
                if let Some(title) = record.scope_title.filter(|title| !title.trim().is_empty()) {
                    scope_titles.insert(
                        AgentRef::for_source(&self.config.id, &record.scope, &record.id),
                        title,
                    );
                }
                let mut tokens = BTreeMap::new();
                if let Some(token) = record.token {
                    tokens.insert("fkey".to_string(), token);
                }
                Agent {
                    source: self.config.id.clone(),
                    pane_id: record.id,
                    session: record.scope,
                    revision: 0,
                    agent_status: record.state,
                    agent: None,
                    label: (!record.title.is_empty()).then_some(record.title),
                    cwd: None,
                    terminal_title: None,
                    terminal_title_stripped: None,
                    focused: record.focused,
                    tokens,
                }
            })
            .collect();
        Ok(Cache {
            at: Instant::now(),
            agents,
            scope_titles,
        })
    }
}

impl AgentSource for CommandSource {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn observation(&self) -> crate::source_observation::SourceObservation {
        use crate::source_observation::{AgentObservation, SourceAvailability, SourceObservation};
        let failure = self.last_failure.borrow().clone();
        let cache = self.cache.borrow();
        let mut result = SourceObservation::unknown(&self.config.id);
        result.last_failure = failure.clone();
        result.observed_at = failure.as_ref().map(|failure| failure.at);
        let failed = failure.as_ref().is_some_and(|failure| failure.active);
        if failed {
            result.availability = SourceAvailability::Unavailable;
        }
        if let Some(cache) = cache.as_ref() {
            let available = !failed && !self.invalidated.get();
            if available {
                result.availability = SourceAvailability::Available;
            }
            result.observed_at = Some(if failed {
                failure.as_ref().expect("active failure").at
            } else {
                cache.at
            });
            result.last_success = Some(cache.at);
            if available {
                result.observed_sessions = cache
                    .agents
                    .iter()
                    .map(|agent| agent.session.clone())
                    .collect();
            }
            result.agents = cache
                .agents
                .iter()
                .map(|agent| {
                    let scope_title = cache.scope_titles.get(&agent.reference()).cloned();
                    let title = agent.label.clone();
                    AgentObservation {
                        agent: agent.clone(),
                        available,
                        observed_at: cache.at,
                        title_at: title.as_ref().map(|_| cache.at),
                        scope_title_at: scope_title.as_ref().map(|_| cache.at),
                        title,
                        scope_title,
                    }
                })
                .collect();
        }
        result
    }

    fn current_identity(&self) -> Option<AgentRef> {
        let identity = self.config.identity.as_ref()?;
        let scope = std::env::var(&identity.scope_env).ok()?;
        let id = std::env::var(&identity.id_env).ok()?;
        if scope.is_empty() || id.is_empty() {
            return None;
        }
        Some(AgentRef::for_source(self.config.id.clone(), scope, id))
    }

    fn refresh(&self) {
        self.invalidated.set(true);
    }

    fn panes(&self) -> Result<Vec<Agent>> {
        self.snapshot()
    }

    fn observed_sessions(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .snapshot()?
            .into_iter()
            .map(|agent| agent.session)
            .collect())
    }

    fn focus_pane(&self, pane: &AgentRef) -> Result<()> {
        let agent = self
            .snapshot()?
            .into_iter()
            .find(|candidate| candidate.matches(pane));
        let vars = BTreeMap::from([
            ("BECKON_AGENT_SOURCE".to_string(), pane.source.clone()),
            ("BECKON_AGENT_SCOPE".to_string(), pane.session.clone()),
            ("BECKON_AGENT_ID".to_string(), pane.pane_id.clone()),
            (
                "BECKON_AGENT_STATE".to_string(),
                agent
                    .as_ref()
                    .map(|agent| agent.agent_status.clone())
                    .unwrap_or_default(),
            ),
            (
                "BECKON_AGENT_TITLE".to_string(),
                agent.and_then(|agent| agent.label).unwrap_or_default(),
            ),
        ]);
        if !self.config.activate.is_empty() {
            run(&self.config.activate, &vars)
                .with_context(|| format!("sources.{} activate command", self.config.id))?;
        }
        if !self.config.focus.is_empty() {
            run(&self.config.focus, &vars)
                .with_context(|| format!("sources.{} focus command", self.config.id))?;
        }
        Ok(())
    }
}

/// Run an argv directly (never through a shell), substituting `$VAR` and
/// `${VAR}` tokens and exporting the same values to the child environment.
fn run(argv: &[String], vars: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let program = expand(program, vars);
    let mut command = Command::new(&program);
    for (name, value) in vars {
        command.env(name, value);
    }
    for argument in arguments {
        command.arg(expand(argument, vars));
    }
    let output =
        crate::process_job::output(&mut command).with_context(|| format!("run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn expand(argument: &str, vars: &BTreeMap<String, String>) -> String {
    let mut result = argument.to_string();
    for (name, value) in vars {
        result = result.replace(&format!("${{{name}}}"), value);
        result = result.replace(&format!("${name}"), value);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn expands_both_placeholder_forms() {
        let vars = BTreeMap::from([("BECKON_AGENT_ID".to_string(), "block:42".to_string())]);
        assert_eq!(expand("$BECKON_AGENT_ID", &vars), "block:42");
        assert_eq!(expand("${BECKON_AGENT_ID}!", &vars), "block:42!");
        assert_eq!(expand("literal", &vars), "literal");
    }

    #[test]
    fn a_list_command_produces_source_qualified_agents() {
        let config = AgentSourceConfig {
            id: "example".into(),
            list: vec![
                "/bin/echo".into(),
                r#"[{"scope":"s1","id":"a1","state":"working","title":"One","focused":true,"token":"f2"}]"#
                    .into(),
            ],
            focus: Vec::new(),
            activate: Vec::new(),
            identity: None,
            refresh_ms: 1000,
        };
        let source = CommandSource::new(config);
        let agents = source.panes().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].source, "example");
        assert_eq!(agents[0].session, "s1");
        assert_eq!(agents[0].pane_id, "a1");
        assert_eq!(agents[0].agent_status, "working");
        assert_eq!(agents[0].label.as_deref(), Some("One"));
        assert_eq!(agents[0].fkey(), Some("f2"));
        assert!(source.observed_sessions().unwrap().contains("s1"));
    }

    #[test]
    fn parses_a_minimal_agent_record() {
        let record: AgentRecord =
            serde_json::from_str(r#"{"id":"session:abc","state":"working"}"#).unwrap();
        assert_eq!(record.scope, DEFAULT_SESSION);
        assert_eq!(record.id, "session:abc");
        assert_eq!(record.state, "working");
        assert!(record.title.is_empty());
    }

    #[test]
    fn refresh_drops_the_cached_snapshot_so_a_new_agent_is_seen() {
        let path = std::env::temp_dir().join(format!(
            "beckon-command-source-refresh-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, r#"[{"id":"a1","state":"working"}]"#).unwrap();
        let config = AgentSourceConfig {
            id: "example".into(),
            list: vec!["/bin/cat".into(), path.to_string_lossy().into_owned()],
            focus: Vec::new(),
            activate: Vec::new(),
            identity: None,
            refresh_ms: 60_000, // keep the first snapshot cached for the whole test
        };
        let source = CommandSource::new(config);

        assert_eq!(source.panes().unwrap()[0].pane_id, "a1");
        fs::write(&path, r#"[{"id":"a2","state":"working"}]"#).unwrap();
        // Within the TTL the cached snapshot is still served, even though the
        // list command now reports a different (brand-new) agent.
        assert_eq!(source.panes().unwrap()[0].pane_id, "a1");

        source.refresh();
        assert_eq!(source.panes().unwrap()[0].pane_id, "a2");

        fs::remove_file(&path).unwrap();
    }
}
