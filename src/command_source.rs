//! Config-driven agent source. All knowledge of a particular integration
//! (its ids, its state vocabulary, how to focus it) lives in configuration and
//! in the commands that configuration names. Nothing here knows about Rex,
//! Herdr, or any other product.
//!
//! AIDEV-NOTE: `list` is a snapshot command, not a stream. A short TTL keeps
//! the daemon's 50ms render tick from spawning a process on every frame while
//! still reacting to state changes within a second.

use std::{
    cell::RefCell,
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
}

pub struct CommandSource {
    config: AgentSourceConfig,
    cache: RefCell<Option<Cache>>,
}

impl CommandSource {
    pub fn new(config: AgentSourceConfig) -> Self {
        Self {
            config,
            cache: RefCell::new(None),
        }
    }

    fn ttl(&self) -> Duration {
        Duration::from_millis(self.config.refresh_ms)
    }

    fn snapshot(&self) -> Result<Vec<Agent>> {
        if let Some(cache) = self.cache.borrow().as_ref()
            && cache.at.elapsed() < self.ttl()
        {
            return Ok(cache.agents.clone());
        }
        let agents = self.list()?;
        *self.cache.borrow_mut() = Some(Cache {
            at: Instant::now(),
            agents: agents.clone(),
        });
        Ok(agents)
    }

    fn list(&self) -> Result<Vec<Agent>> {
        let output = run(&self.config.list, &BTreeMap::new())
            .with_context(|| format!("sources.{} list command", self.config.id))?;
        let records: Vec<AgentRecord> = serde_json::from_slice(&output).with_context(|| {
            format!(
                "sources.{} list output is not an agent array",
                self.config.id
            )
        })?;
        Ok(records
            .into_iter()
            .map(|record| {
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
            .collect())
    }
}

impl AgentSource for CommandSource {
    fn id(&self) -> &str {
        &self.config.id
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
    let mut command = Command::new(program);
    for (name, value) in vars {
        command.env(name, value);
    }
    for argument in arguments {
        command.arg(expand(argument, vars));
    }
    let output = command.output().with_context(|| format!("run {program}"))?;
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
}
