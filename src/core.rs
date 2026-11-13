use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[cfg(test)]
pub const KEY_IDS: [&str; 10] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10"];
pub const STATE_VERSION: u32 = 1;

/// The source a binding came from before Beckon supported more than one. State
/// files written by older versions omit `source` and are read as Herdr.
pub const DEFAULT_SOURCE: &str = "herdr";

/// Herdr's name for the unnamed session whose socket lives directly under the
/// Herdr configuration directory. This mirrors Herdr's own convention rather
/// than a Beckon invention.
pub const DEFAULT_SESSION: &str = "default";

fn default_source() -> String {
    DEFAULT_SOURCE.to_string()
}

fn default_session() -> String {
    DEFAULT_SESSION.to_string()
}

/// Source-qualified agent identity.
///
/// `source` names the integration that owns the agent (`herdr`, `rex`, ...).
/// `session` is a source-defined scope and `pane_id` a source-defined id within
/// that scope. Herdr pane IDs are only unique within one session; Rex block IDs
/// live inside a Rex session. Both therefore use the same shape, and no source
/// can collide with another even if their ids happen to match.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRef {
    /// Older state files omit this and are read as Herdr.
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_session")]
    pub session: String,
    pub pane_id: String,
}

impl AgentRef {
    /// Herdr identity, kept as the ergonomic default for existing call sites.
    pub fn new(session: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self::for_source(DEFAULT_SOURCE, session, pane_id)
    }

    pub fn for_source(
        source: impl Into<String>,
        session: impl Into<String>,
        pane_id: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            session: session.into(),
            pane_id: pane_id.into(),
        }
    }
}

/// Log and diagnostic identity; not a wire format. Agent IDs contain colons of
/// their own, so this is display-only.
impl fmt::Display for AgentRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}:{}",
            self.source, self.session, self.pane_id
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub key: String,
    /// Which integration owns this agent. Older state files omit it.
    #[serde(default = "default_source")]
    pub source: String,
    /// Owning scope. Pre-multi-session state files omit this field and are read
    /// as the default session.
    #[serde(default = "default_session")]
    pub session: String,
    pub pane_id: String,
}

impl Binding {
    pub fn reference(&self) -> AgentRef {
        AgentRef::for_source(
            self.source.clone(),
            self.session.clone(),
            self.pane_id.clone(),
        )
    }

    /// Identity comparison used everywhere the ledger is matched against a
    /// live agent. Including `source` is what keeps two integrations with the
    /// same opaque id from sharing a binding.
    pub fn matches(&self, agent: &AgentRef) -> bool {
        self.source == agent.source
            && self.session == agent.session
            && self.pane_id == agent.pane_id
    }

    pub fn matches_agent(&self, agent: &Agent) -> bool {
        self.source == agent.source
            && self.session == agent.session
            && self.pane_id == agent.pane_id
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BindingState {
    pub state_version: u32,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct Agent {
    /// Integration that produced this agent. Herdr's wire JSON has no source
    /// field; the adapter stamps it after fetching.
    #[serde(default = "default_source")]
    pub source: String,
    pub pane_id: String,
    /// Owning scope. Herdr's wire JSON has no session field; the per-session
    /// adapter stamps it after fetching.
    #[serde(default = "default_session")]
    pub session: String,
    #[serde(default)]
    pub revision: u64,
    pub agent_status: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

impl Agent {
    pub fn fkey(&self) -> Option<&str> {
        self.tokens.get("fkey").map(String::as_str)
    }

    pub fn reference(&self) -> AgentRef {
        AgentRef::for_source(
            self.source.clone(),
            self.session.clone(),
            self.pane_id.clone(),
        )
    }

    pub fn matches(&self, agent: &AgentRef) -> bool {
        self.source == agent.source
            && self.session == agent.session
            && self.pane_id == agent.pane_id
    }

    /// Resolve a useful title without modifying the pane. Agent TUIs normally
    /// expose their current task through the stripped terminal title; a manual
    /// label remains the next explicit fallback.
    pub fn display_title(&self) -> String {
        self.terminal_title_stripped
            .as_deref()
            .or(self.label.as_deref())
            .or(self.terminal_title.as_deref())
            .filter(|title| !title.trim().is_empty())
            .map(str::trim)
            .map(str::to_owned)
            .or_else(|| {
                self.cwd.as_deref().and_then(|cwd| {
                    Path::new(cwd)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                })
            })
            .unwrap_or_else(|| "untitled pane".into())
    }
}

/// Presentation data for every currently live pane, including unbound panes.
/// This deliberately contains resolved values so clients never need to infer a
/// missing `fkey` token as an unbound state.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentPresentation {
    pub source: String,
    pub session: String,
    pub pane_id: String,
    pub title: String,
    pub binding: String,
    pub agent_status: String,
    pub focused: bool,
}

/// Durable storage for the binding ledger. The ledger, not Herdr metadata, is
/// authoritative because Herdr tokens are intentionally not restart-durable.
pub trait BindingStore {
    fn load(&self) -> Result<Option<BindingState>>;
    fn save(&self, state: &BindingState) -> Result<()>;
}

/// Result of writing Beckon-only pane presentation metadata.
///
/// A pane can close after Beckon's cache observes it but before the metadata
/// command reaches Herdr. That is expected lifecycle churn, not a failed
/// sidebar update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationTokenWrite {
    Written,
    PaneGone,
}

/// The minimal surface Beckon's binding policy needs from an agent
/// integration. Herdr is the first implementation; Rex will be the second.
/// Implementations are per-source and per-scope; callers managing several
/// Herdr sessions route through [`crate::session::SessionRouter`].
///
/// Required: `id`, `panes`, `observed_sessions` and `focus_pane`. The metadata
/// and key-sending operations are Herdr-specific; a source without them takes
/// the default, so adding an integration never requires faking capabilities
/// it does not have.
pub trait AgentSource {
    fn id(&self) -> &str;
    fn panes(&self) -> Result<Vec<Agent>>;
    /// Scopes whose agent snapshot is currently authoritative.
    ///
    /// This is declared, not derived from [`Self::panes`]: an empty snapshot
    /// from a scope Beckon can see means its agents are gone, while a scope
    /// that is not listed here is simply not observed (for example a stopped
    /// Herdr server) and its bindings are kept dormant rather than released.
    fn observed_sessions(&self) -> Result<std::collections::BTreeSet<String>>;
    /// Whether `scope` in `source` is authoritative right now. Registries
    /// override this to route by source instead of comparing their own `id`.
    fn observes(&self, source: &str, scope: &str) -> Result<bool> {
        Ok(self.id() == source && self.observed_sessions()?.contains(scope))
    }
    /// Identity of the agent `beckon bind` is running inside, when the source
    /// can determine it from its environment. Herdr uses `HERDR_PANE_ID`
    /// through the CLI's own resolution and leaves this as `None`.
    fn current_identity(&self) -> Option<AgentRef> {
        None
    }

    /// Drop any cached agent snapshot so the next [`Self::panes`] observes
    /// current state. Sources backed by a short-lived cache (for example a
    /// command source with a snapshot TTL) override this; stateless sources
    /// keep the no-op default.
    fn refresh(&self) {}

    fn focus_pane(&self, pane: &AgentRef) -> Result<()>;

    /// Record the Beckon key on the agent, when the source has writable
    /// metadata. Sources without it keep the ledger authoritative and no-op.
    fn write_fkey(&self, _pane: &AgentRef, _key: Option<&str>) -> Result<()> {
        Ok(())
    }

    /// Publish Beckon-owned display tokens without changing an agent's title
    /// or other metadata owned by a user or agent integration. Sources without
    /// presentation metadata report `PaneGone`, which the daemon treats as a
    /// settled result rather than retrying on every tick.
    fn write_presentation_tokens(
        &self,
        _pane: &AgentRef,
        _binding: &str,
    ) -> Result<PresentationTokenWrite> {
        Ok(PresentationTokenWrite::PaneGone)
    }

    fn send_keys(&self, _pane: &AgentRef, _keys: &[&str]) -> Result<()> {
        bail!("{} cannot send keys to its agents", self.id())
    }
}

pub struct BindingService<'a> {
    store: &'a dyn BindingStore,
    panes: &'a dyn AgentSource,
    keys: Vec<String>,
}

impl<'a> BindingService<'a> {
    pub fn new(store: &'a dyn BindingStore, panes: &'a dyn AgentSource) -> Self {
        Self {
            store,
            panes,
            keys: Vec::new(),
        }
    }

    pub fn with_keys(mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.keys = keys
            .into_iter()
            .map(|key| key.as_ref().to_string())
            .collect();
        self
    }

    fn configured_key<'k>(&self, key: &'k str) -> Result<&'k str> {
        if !self.keys.iter().any(|candidate| candidate == key) {
            bail!(
                "unknown binding key {key:?}; configured: {}",
                self.keys.join(", ")
            );
        }
        Ok(key)
    }

    pub fn bind(&self, pane: &AgentRef, requested_key: Option<&str>) -> Result<BindResult> {
        let panes = self.panes.panes()?;
        if !panes.iter().any(|candidate| candidate.matches(pane)) {
            bail!("{}", pane_missing_message(pane));
        }
        let mut state = self.reconcile_panes(&panes)?;
        // Implicit registration must be idempotent. A repeated `beckon bind`
        // from the same pane keeps its current key; only an explicit --key can
        // intentionally move it.
        if requested_key.is_none()
            && let Some(existing) = state.bindings.iter().find(|binding| binding.matches(pane))
        {
            return Ok(BindResult {
                key: existing.key.clone(),
                session: pane.session.clone(),
                pane_id: pane.pane_id.clone(),
                changed: false,
            });
        }
        let key = match requested_key {
            Some(key) => self.configured_key(key)?.to_string(),
            None => self
                .keys
                .iter()
                .find(|key| !state.bindings.iter().any(|binding| binding.key == **key))
                .context("no Beckon keys are free")?
                .to_string(),
        };

        if let Some(owner) = state.bindings.iter().find(|binding| binding.key == key)
            && !owner.matches(pane)
        {
            bail!("{key} is already bound to {}", owner.reference());
        }
        if state
            .bindings
            .iter()
            .any(|binding| binding.matches(pane) && binding.key == key)
        {
            return Ok(BindResult {
                key,
                session: pane.session.clone(),
                pane_id: pane.pane_id.clone(),
                changed: false,
            });
        }

        state.bindings.retain(|binding| !binding.matches(pane));
        state.bindings.push(Binding {
            key: key.clone(),
            source: pane.source.clone(),
            session: pane.session.clone(),
            pane_id: pane.pane_id.clone(),
        });
        self.store.save(&state)?;
        self.panes.write_fkey(pane, Some(&key))?;
        Ok(BindResult {
            key,
            session: pane.session.clone(),
            pane_id: pane.pane_id.clone(),
            changed: true,
        })
    }

    pub fn release(&self, pane: &AgentRef) -> Result<bool> {
        let panes = self.panes.panes()?;
        if !panes.iter().any(|candidate| candidate.matches(pane)) {
            bail!("{}", pane_missing_message(pane));
        }
        let mut state = self.reconcile_panes(&panes)?;
        let before = state.bindings.len();
        state.bindings.retain(|binding| !binding.matches(pane));
        if state.bindings.len() == before {
            return Ok(false);
        }
        self.store.save(&state)?;
        self.panes.write_fkey(pane, None)?;
        Ok(true)
    }

    /// Release a binding by physical key without requiring the caller to be
    /// inside the target pane.
    pub fn release_key(&self, key: &str) -> Result<Option<Binding>> {
        let key = self.configured_key(key)?;
        let panes = self.panes.panes()?;
        let mut state = self.reconcile_panes(&panes)?;
        let Some(binding) = state
            .bindings
            .iter()
            .find(|binding| binding.key == key)
            .cloned()
        else {
            return Ok(None);
        };
        state
            .bindings
            .retain(|candidate| candidate.key != binding.key);
        self.store.save(&state)?;
        self.panes.write_fkey(&binding.reference(), None)?;
        Ok(Some(binding))
    }

    /// Remove every live Beckon registration. This is intentionally explicit:
    /// callers must choose `beckon release --all`, rather than an omitted pane
    /// accidentally affecting other agents.
    pub fn release_all(&self) -> Result<Vec<Binding>> {
        let panes = self.panes.panes()?;
        let mut state = self.reconcile_panes(&panes)?;
        let released = state.bindings.clone();
        if released.is_empty() {
            return Ok(released);
        }

        // Persist the empty ledger before clearing pane metadata. If a pane
        // closes during this loop, reconciliation will clear any residual
        // fkey token on the next successful directory snapshot.
        state.bindings.clear();
        self.store.save(&state)?;
        for binding in &released {
            self.panes.write_fkey(&binding.reference(), None)?;
        }
        Ok(released)
    }

    pub fn status(&self) -> Result<Vec<(Binding, Agent)>> {
        let panes = self.panes.panes()?;
        let state = self.reconcile_panes(&panes)?;
        let mut bindings = state
            .bindings
            .into_iter()
            .filter_map(|binding| {
                panes
                    .iter()
                    .find(|pane| binding.matches_agent(pane))
                    .cloned()
                    .map(|pane| (binding, pane))
            })
            .collect::<Vec<_>>();
        bindings.sort_by(|left, right| left.0.key.cmp(&right.0.key));
        Ok(bindings)
    }

    pub fn panes(&self) -> Result<Vec<AgentPresentation>> {
        let panes = self.panes.panes()?;
        let state = self.reconcile_panes(&panes)?;
        let mut presentation = panes
            .into_iter()
            .map(|pane| {
                let binding = state
                    .bindings
                    .iter()
                    .find(|binding| binding.matches_agent(&pane))
                    .map(|binding| binding.key.to_ascii_uppercase())
                    .unwrap_or_else(|| "unbound".into());
                let title = pane.display_title();
                AgentPresentation {
                    source: pane.source,
                    session: pane.session,
                    pane_id: pane.pane_id,
                    title,
                    binding,
                    agent_status: pane.agent_status,
                    focused: pane.focused,
                }
            })
            .collect::<Vec<_>>();
        presentation.sort_by(|left, right| {
            left.session
                .cmp(&right.session)
                .then_with(|| left.pane_id.cmp(&right.pane_id))
        });
        Ok(presentation)
    }

    pub fn pane_for_key(&self, key: &str) -> Result<AgentRef> {
        let key = self.configured_key(key)?;
        let panes = self.panes.panes()?;
        let state = self.reconcile_panes(&panes)?;
        state
            .bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(Binding::reference)
            .with_context(|| format!("{key} is not bound"))
    }

    fn reconcile_panes(&self, panes: &[Agent]) -> Result<BindingState> {
        let existing = self.store.load()?;
        let original = existing.clone();
        let imported_tokens = existing.is_none();
        let mut state = existing.unwrap_or_else(|| BindingState {
            state_version: STATE_VERSION,
            bindings: panes
                .iter()
                .filter_map(|pane| {
                    pane.fkey().map(|key| Binding {
                        key: key.to_string(),
                        source: pane.source.clone(),
                        session: pane.session.clone(),
                        pane_id: pane.pane_id.clone(),
                    })
                })
                .collect(),
        });
        validate_bindings(&state.bindings)?;
        for binding in &state.bindings {
            self.configured_key(&binding.key).with_context(|| format!("ledger contains undeclared binding {:?}; migrate the ledger explicitly or restore its declaration", binding.key))?;
        }
        // Only sessions with an authoritative snapshot can prove that a pane is
        // gone. Bindings in unobserved sessions stay dormant so a stopped (or
        // not yet running) Herdr session does not silently lose its keys.
        state.bindings.retain(|binding| {
            // Only the source that owns a binding can prove its agent is gone.
            // A registry routes this by source, so an unrelated source's
            // snapshot never releases another integration's bindings.
            !self
                .panes
                .observes(&binding.source, &binding.session)
                .unwrap_or(false)
                || panes.iter().any(|pane| binding.matches_agent(pane))
        });
        if original.as_ref() != Some(&state) {
            self.store.save(&state)?;
        }

        for pane in panes {
            match state
                .bindings
                .iter()
                .find(|binding| binding.matches_agent(pane))
            {
                Some(expected) if pane.fkey() != Some(expected.key.as_str()) => {
                    self.panes
                        .write_fkey(&pane.reference(), Some(&expected.key))?;
                }
                None if !imported_tokens && pane.fkey().is_some() => {
                    self.panes.write_fkey(&pane.reference(), None)?;
                }
                _ => {}
            }
        }
        Ok(state)
    }
}

/// A resolved identity can be stale (for example a recycled terminal still
/// exporting old identity environment variables). Give an actionable recovery
/// without hardcoding any integration's vocabulary: only the configured source
/// id, the opaque pane id, generic wording, and the CLI command are used.
fn pane_missing_message(pane: &AgentRef) -> String {
    let scope = if pane.session == DEFAULT_SESSION {
        format!("source {}", pane.source)
    } else {
        format!("source {} in session {}", pane.source, pane.session)
    };
    format!(
        "no live agent matches pane {} in {}; the identity may be stale. \
         Retry with `beckon bind --pane <id>` using an explicit pane, or unset \
         the identity environment variables for this source.",
        pane.pane_id, scope
    )
}

#[derive(Debug, Serialize)]
pub struct BindResult {
    pub key: String,
    pub session: String,
    pub pane_id: String,
    pub changed: bool,
}

pub fn valid_key(key: &str) -> Result<&str> {
    if !crate::device::valid_id(key) {
        bail!("invalid binding name {key:?}");
    }
    Ok(key)
}

pub fn validate_bindings(bindings: &[Binding]) -> Result<()> {
    for binding in bindings {
        valid_key(&binding.key)?;
        if binding.source.is_empty() {
            bail!("a binding has an empty source");
        }
        if binding.session.is_empty() {
            bail!("a binding has an empty session");
        }
        if binding.pane_id.is_empty() {
            bail!("a binding has an empty pane_id");
        }
    }
    for (index, binding) in bindings.iter().enumerate() {
        if bindings[index + 1..].iter().any(|other| {
            other.key == binding.key
                || (other.source == binding.source
                    && other.session == binding.session
                    && other.pane_id == binding.pane_id)
        }) {
            bail!("bindings must have unique keys and pane references");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap};

    use super::*;

    #[derive(Default)]
    struct MemoryStore(RefCell<Option<BindingState>>);
    impl BindingStore for MemoryStore {
        fn load(&self) -> Result<Option<BindingState>> {
            Ok(self.0.borrow().clone())
        }
        fn save(&self, state: &BindingState) -> Result<()> {
            *self.0.borrow_mut() = Some(state.clone());
            Ok(())
        }
    }

    struct FakePanes {
        sessions: Vec<String>,
        panes: RefCell<Vec<Agent>>,
    }

    impl FakePanes {
        fn new(panes: Vec<Agent>) -> Self {
            let sessions = panes
                .iter()
                .map(|pane| pane.session.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            Self {
                sessions,
                panes: RefCell::new(panes),
            }
        }
    }

    impl AgentSource for FakePanes {
        fn id(&self) -> &str {
            crate::core::DEFAULT_SOURCE
        }

        fn panes(&self) -> Result<Vec<Agent>> {
            Ok(self.panes.borrow().clone())
        }

        fn observed_sessions(&self) -> Result<std::collections::BTreeSet<String>> {
            Ok(self.sessions.iter().cloned().collect())
        }

        fn write_fkey(&self, pane: &AgentRef, key: Option<&str>) -> Result<()> {
            let mut panes = self.panes.borrow_mut();
            let entry = panes
                .iter_mut()
                .find(|candidate| candidate.matches(pane))
                .context("unknown pane")?;
            match key {
                Some(key) => entry.tokens.insert("fkey".into(), key.into()),
                None => entry.tokens.remove("fkey"),
            };
            Ok(())
        }

        fn write_presentation_tokens(
            &self,
            _pane: &AgentRef,
            _binding: &str,
        ) -> Result<PresentationTokenWrite> {
            Ok(PresentationTokenWrite::Written)
        }
        fn focus_pane(&self, _pane: &AgentRef) -> Result<()> {
            Ok(())
        }

        fn send_keys(&self, _pane: &AgentRef, _keys: &[&str]) -> Result<()> {
            Ok(())
        }
    }

    fn pane_in(session: &str, id: &str) -> Agent {
        Agent {
            source: crate::core::DEFAULT_SOURCE.into(),
            pane_id: id.into(),
            session: session.into(),
            revision: 0,
            agent_status: "idle".into(),
            agent: None,
            label: None,
            cwd: None,
            terminal_title: None,
            terminal_title_stripped: None,
            focused: false,
            tokens: BTreeMap::new(),
        }
    }

    fn pane(id: &str) -> Agent {
        pane_in(DEFAULT_SESSION, id)
    }

    fn reference(id: &str) -> AgentRef {
        AgentRef::new(DEFAULT_SESSION, id)
    }

    #[test]
    fn assigns_the_first_free_key_and_mirrors_it() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);
        let result = service.bind(&reference("p1"), None).unwrap();
        assert_eq!(result.key, "f1");
        assert_eq!(result.session, DEFAULT_SESSION);
        assert_eq!(panes.panes().unwrap()[0].fkey(), Some("f1"));
    }

    #[test]
    fn binding_names_and_capacity_are_configured_not_function_keys() {
        let store = MemoryStore::default();
        let panes = FakePanes::new((1..=13).map(|i| pane(&format!("p{i}"))).collect());
        let keys: Vec<_> = (1..=13).map(|i| format!("key{i}")).collect();
        let service = BindingService::new(&store, &panes).with_keys(&keys);
        for i in 1..=13 {
            assert_eq!(
                service
                    .bind(&reference(&format!("p{i}")), None)
                    .unwrap()
                    .key,
                format!("key{i}")
            );
        }
        assert!(service.bind(&reference("p1"), Some("f1")).is_err());
        assert!(BindingService::new(&store, &panes).panes().is_err());
    }

    #[test]
    fn implicit_rebind_keeps_the_existing_key() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1"), pane("p2")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);
        assert_eq!(service.bind(&reference("p1"), None).unwrap().key, "f1");
        assert_eq!(service.bind(&reference("p2"), None).unwrap().key, "f2");

        let result = service.bind(&reference("p1"), None).unwrap();
        assert_eq!(result.key, "f1");
        assert!(!result.changed);
        assert_eq!(panes.panes().unwrap()[0].fkey(), Some("f1"));
    }

    #[test]
    fn binds_the_same_pane_id_in_two_sessions_independently() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("w1:p1"), pane_in("agent-workspace", "w1:p1")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);

        let first = service
            .bind(&AgentRef::new(DEFAULT_SESSION, "w1:p1"), Some("f1"))
            .unwrap();
        assert_eq!(first.session, DEFAULT_SESSION);
        let second = service
            .bind(&AgentRef::new("agent-workspace", "w1:p1"), Some("f2"))
            .unwrap();
        assert_eq!(second.session, "agent-workspace");

        let status = service.status().unwrap();
        assert_eq!(status.len(), 2);
        assert!(status.iter().any(|(binding, _)| {
            binding.session == DEFAULT_SESSION && binding.pane_id == "w1:p1"
        }));
        assert!(status.iter().any(|(binding, _)| {
            binding.session == "agent-workspace" && binding.pane_id == "w1:p1"
        }));
    }

    #[test]
    fn legacy_binding_state_defaults_to_the_herdr_source() {
        let state: BindingState = serde_json::from_str(
            r#"{"state_version":1,"bindings":[{"key":"f3","pane_id":"w1:p1"}]}"#,
        )
        .unwrap();
        assert_eq!(state.bindings[0].source, DEFAULT_SOURCE);
        assert_eq!(state.bindings[0].session, DEFAULT_SESSION);
        assert_eq!(state.bindings[0].reference().source, DEFAULT_SOURCE);
    }

    #[test]
    fn the_same_opaque_id_in_two_sources_does_not_share_a_binding() {
        let store = MemoryStore::default();
        let mut rex_pane = pane("w1:p1");
        rex_pane.source = "rex".into();
        let panes = FakePanes::new(vec![pane("w1:p1"), rex_pane]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);

        let herdr = service
            .bind(&AgentRef::new(DEFAULT_SESSION, "w1:p1"), None)
            .unwrap();
        assert_eq!(herdr.key, "f1");
        // Without source in the identity this would look like an implicit
        // re-bind of the Herdr agent and return f1 unchanged.
        let rex = service
            .bind(
                &AgentRef::for_source("rex", DEFAULT_SESSION, "w1:p1"),
                Some("f2"),
            )
            .unwrap();
        assert_eq!(rex.key, "f2");
        assert!(rex.changed);

        let state = store.load().unwrap().unwrap();
        assert_eq!(state.bindings.len(), 2);
        assert_eq!(state.bindings[0].source, "herdr");
        assert_eq!(state.bindings[1].source, "rex");
    }

    #[test]
    fn a_missing_pane_flags_a_stale_identity_without_product_vocabulary() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);

        let default_error = service
            .bind(&reference("gone"), None)
            .unwrap_err()
            .to_string();
        assert!(
            default_error.contains("identity may be stale"),
            "{default_error}"
        );
        assert!(
            default_error.contains("beckon bind --pane <id>"),
            "{default_error}"
        );
        assert!(default_error.contains("unset"), "{default_error}");
        assert!(
            default_error.contains("environment variables"),
            "{default_error}"
        );

        let named_error = service
            .bind(&AgentRef::new("agent-workspace", "gone"), None)
            .unwrap_err()
            .to_string();
        assert!(named_error.contains("agent-workspace"), "{named_error}");
        // The message must not hardcode any integration's env var or product name.
        for forbidden in ["REX_SESSION", "REX_BLOCK", "Rex", "Herdr"] {
            assert!(!named_error.contains(forbidden), "{named_error}");
        }
    }

    #[test]
    fn keeps_bindings_for_unobserved_sessions_dormant() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);
        service.bind(&reference("p1"), Some("f1")).unwrap();
        {
            let mut state = store.0.borrow_mut();
            let state = state.as_mut().unwrap();
            state.bindings.push(Binding {
                source: crate::core::DEFAULT_SOURCE.into(),
                key: "f2".into(),
                session: "agent-workspace".into(),
                pane_id: "w2:p1".into(),
            });
        }

        // The stopped session still owns its key even though its panes are not
        // in any observed snapshot.
        assert_eq!(service.status().unwrap().len(), 1);
        assert_eq!(store.load().unwrap().unwrap().bindings.len(), 2);
    }

    #[test]
    fn presents_every_pane_with_title_and_explicit_unbound_state() {
        let store = MemoryStore::default();
        let mut bound = pane("p1");
        bound.terminal_title_stripped = Some(" Inspect task ".into());
        let mut unbound = pane("p2");
        unbound.cwd = Some("/code/feature-pane-presentation".into());
        let panes = FakePanes::new(vec![bound, unbound]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);
        service.bind(&reference("p1"), Some("f4")).unwrap();

        assert_eq!(
            service.panes().unwrap(),
            vec![
                AgentPresentation {
                    source: crate::core::DEFAULT_SOURCE.into(),
                    session: DEFAULT_SESSION.into(),
                    pane_id: "p1".into(),
                    title: "Inspect task".into(),
                    binding: "F4".into(),
                    agent_status: "idle".into(),
                    focused: false,
                },
                AgentPresentation {
                    source: crate::core::DEFAULT_SOURCE.into(),
                    session: DEFAULT_SESSION.into(),
                    pane_id: "p2".into(),
                    title: "feature-pane-presentation".into(),
                    binding: "unbound".into(),
                    agent_status: "idle".into(),
                    focused: false,
                },
            ]
        );
    }

    #[test]
    fn rejects_duplicate_binding_keys() {
        let bindings = vec![
            Binding {
                source: crate::core::DEFAULT_SOURCE.into(),
                key: "f1".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "p1".into(),
            },
            Binding {
                source: crate::core::DEFAULT_SOURCE.into(),
                key: "f1".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "p2".into(),
            },
        ];
        assert!(validate_bindings(&bindings).is_err());
    }

    #[test]
    fn rejects_duplicate_pane_references_but_allows_distinct_sessions() {
        let duplicate = vec![
            Binding {
                source: crate::core::DEFAULT_SOURCE.into(),
                key: "f1".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "w1:p1".into(),
            },
            Binding {
                source: crate::core::DEFAULT_SOURCE.into(),
                key: "f2".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "w1:p1".into(),
            },
        ];
        assert!(validate_bindings(&duplicate).is_err());

        let distinct_sessions = vec![
            Binding {
                source: crate::core::DEFAULT_SOURCE.into(),
                key: "f1".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "w1:p1".into(),
            },
            Binding {
                source: crate::core::DEFAULT_SOURCE.into(),
                key: "f2".into(),
                session: "agent-workspace".into(),
                pane_id: "w1:p1".into(),
            },
        ];
        validate_bindings(&distinct_sessions).unwrap();
    }

    #[test]
    fn releases_a_binding_by_key() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);
        service.bind(&reference("p1"), Some("f2")).unwrap();

        let released = service.release_key("f2").unwrap().unwrap();
        assert_eq!(released.pane_id, "p1");
        assert_eq!(released.session, DEFAULT_SESSION);
        assert_eq!(panes.panes().unwrap()[0].fkey(), None);
        assert!(service.status().unwrap().is_empty());
    }

    #[test]
    fn releases_all_bindings_and_their_tokens() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1"), pane("p2")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);
        service.bind(&reference("p1"), Some("f2")).unwrap();
        service.bind(&reference("p2"), Some("f7")).unwrap();

        let released = service.release_all().unwrap();
        assert_eq!(released.len(), 2);
        assert!(
            panes
                .panes()
                .unwrap()
                .iter()
                .all(|pane| pane.fkey().is_none())
        );
        assert!(store.load().unwrap().unwrap().bindings.is_empty());
    }

    #[test]
    fn closes_only_the_observed_sessions_panes() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1"), pane("p2")]);
        let service = BindingService::new(&store, &panes).with_keys(KEY_IDS);
        service.bind(&reference("p1"), Some("f2")).unwrap();

        panes.panes.borrow_mut().clear();

        assert!(service.status().unwrap().is_empty());
        assert!(store.load().unwrap().unwrap().bindings.is_empty());
    }
}
