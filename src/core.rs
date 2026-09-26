use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const KEY_IDS: [&str; 10] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10"];
pub const STATE_VERSION: u32 = 1;

/// Herdr's name for the unnamed session whose socket lives directly under the
/// Herdr configuration directory. This mirrors Herdr's own convention rather
/// than a Beckon invention.
pub const DEFAULT_SESSION: &str = "default";

fn default_session() -> String {
    DEFAULT_SESSION.to_string()
}

/// Session-qualified pane identity.
///
/// Herdr pane IDs are only unique within one session, so every operation that
/// targets a pane carries the session that owns it. The same pane ID may
/// legitimately exist in two sessions; it then refers to two distinct panes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaneRef {
    #[serde(default = "default_session")]
    pub session: String,
    pub pane_id: String,
}

impl PaneRef {
    pub fn new(session: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            pane_id: pane_id.into(),
        }
    }
}

/// Log and diagnostic identity; not a wire format. Pane IDs contain colons of
/// their own, so this is display-only.
impl fmt::Display for PaneRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.session, self.pane_id)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub key: String,
    /// Owning Herdr session. Pre-multi-session state files omit this field and
    /// are read as the default session.
    #[serde(default = "default_session")]
    pub session: String,
    pub pane_id: String,
}

impl Binding {
    pub fn reference(&self) -> PaneRef {
        PaneRef::new(self.session.clone(), self.pane_id.clone())
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BindingState {
    pub state_version: u32,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct Pane {
    pub pane_id: String,
    /// Owning session. Herdr's wire JSON has no session field; the per-session
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

impl Pane {
    pub fn fkey(&self) -> Option<&str> {
        self.tokens.get("fkey").map(String::as_str)
    }

    pub fn reference(&self) -> PaneRef {
        PaneRef::new(self.session.clone(), self.pane_id.clone())
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
pub struct PanePresentation {
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

/// The minimal Herdr surface Beckon's binding policy needs. Implementations
/// are per-session; callers that manage several sessions route through
/// [`crate::session::SessionRouter`].
pub trait PaneDirectory {
    fn panes(&self) -> Result<Vec<Pane>>;
    /// Sessions whose pane snapshot is currently authoritative.
    ///
    /// This is declared, not derived from [`Self::panes`]: an empty snapshot
    /// from a session Beckon can see means its panes are gone, while a session
    /// that is not listed here is simply not observed (for example a stopped
    /// Herdr server) and its bindings are kept dormant rather than released.
    fn observed_sessions(&self) -> Result<std::collections::BTreeSet<String>>;
    fn write_fkey(&self, pane: &PaneRef, key: Option<&str>) -> Result<()>;
    /// Publish Beckon-owned display tokens without changing a pane's title or
    /// other metadata owned by a user or agent integration.
    fn write_presentation_tokens(
        &self,
        pane: &PaneRef,
        binding: &str,
    ) -> Result<PresentationTokenWrite>;
    fn focus_pane(&self, pane: &PaneRef) -> Result<()>;
    fn send_keys(&self, pane: &PaneRef, keys: &[&str]) -> Result<()>;
}

pub struct BindingService<'a> {
    store: &'a dyn BindingStore,
    panes: &'a dyn PaneDirectory,
}

impl<'a> BindingService<'a> {
    pub fn new(store: &'a dyn BindingStore, panes: &'a dyn PaneDirectory) -> Self {
        Self { store, panes }
    }

    pub fn bind(&self, pane: &PaneRef, requested_key: Option<&str>) -> Result<BindResult> {
        let panes = self.panes.panes()?;
        if !panes
            .iter()
            .any(|candidate| candidate.session == pane.session && candidate.pane_id == pane.pane_id)
        {
            bail!("{}", pane_missing_message(pane));
        }
        let mut state = self.reconcile_panes(&panes)?;
        // Implicit registration must be idempotent. A repeated `beckon bind`
        // from the same pane keeps its current key; only an explicit --key can
        // intentionally move it.
        if requested_key.is_none()
            && let Some(existing) = state
                .bindings
                .iter()
                .find(|binding| binding.session == pane.session && binding.pane_id == pane.pane_id)
        {
            return Ok(BindResult {
                key: existing.key.clone(),
                session: pane.session.clone(),
                pane_id: pane.pane_id.clone(),
                changed: false,
            });
        }
        let key = match requested_key {
            Some(key) => valid_key(key)?.to_string(),
            None => first_free_key(&state.bindings)
                .context("no Beckon keys are free")?
                .to_string(),
        };

        if let Some(owner) = state.bindings.iter().find(|binding| binding.key == key)
            && !(owner.session == pane.session && owner.pane_id == pane.pane_id)
        {
            bail!("{key} is already bound to {}", owner.reference());
        }
        if state.bindings.iter().any(|binding| {
            binding.session == pane.session && binding.pane_id == pane.pane_id && binding.key == key
        }) {
            return Ok(BindResult {
                key,
                session: pane.session.clone(),
                pane_id: pane.pane_id.clone(),
                changed: false,
            });
        }

        state.bindings.retain(|binding| {
            !(binding.session == pane.session && binding.pane_id == pane.pane_id)
        });
        state.bindings.push(Binding {
            key: key.clone(),
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

    pub fn release(&self, pane: &PaneRef) -> Result<bool> {
        let panes = self.panes.panes()?;
        if !panes
            .iter()
            .any(|candidate| candidate.session == pane.session && candidate.pane_id == pane.pane_id)
        {
            bail!("{}", pane_missing_message(pane));
        }
        let mut state = self.reconcile_panes(&panes)?;
        let before = state.bindings.len();
        state.bindings.retain(|binding| {
            !(binding.session == pane.session && binding.pane_id == pane.pane_id)
        });
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
        let key = valid_key(key)?;
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

    pub fn status(&self) -> Result<Vec<(Binding, Pane)>> {
        let panes = self.panes.panes()?;
        let state = self.reconcile_panes(&panes)?;
        let mut bindings = state
            .bindings
            .into_iter()
            .filter_map(|binding| {
                panes
                    .iter()
                    .find(|pane| pane.session == binding.session && pane.pane_id == binding.pane_id)
                    .cloned()
                    .map(|pane| (binding, pane))
            })
            .collect::<Vec<_>>();
        bindings.sort_by(|left, right| left.0.key.cmp(&right.0.key));
        Ok(bindings)
    }

    pub fn panes(&self) -> Result<Vec<PanePresentation>> {
        let panes = self.panes.panes()?;
        let state = self.reconcile_panes(&panes)?;
        let mut presentation = panes
            .into_iter()
            .map(|pane| {
                let binding = state
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.session == pane.session && binding.pane_id == pane.pane_id
                    })
                    .map(|binding| binding.key.to_ascii_uppercase())
                    .unwrap_or_else(|| "unbound".into());
                let title = pane.display_title();
                PanePresentation {
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

    pub fn pane_for_key(&self, key: &str) -> Result<PaneRef> {
        let key = valid_key(key)?;
        let panes = self.panes.panes()?;
        let state = self.reconcile_panes(&panes)?;
        state
            .bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(Binding::reference)
            .with_context(|| format!("{key} is not bound"))
    }

    fn reconcile_panes(&self, panes: &[Pane]) -> Result<BindingState> {
        let existing = self.store.load()?;
        let imported_tokens = existing.is_none();
        let mut state = existing.unwrap_or_else(|| BindingState {
            state_version: STATE_VERSION,
            bindings: panes
                .iter()
                .filter_map(|pane| {
                    pane.fkey().map(|key| Binding {
                        key: key.to_string(),
                        session: pane.session.clone(),
                        pane_id: pane.pane_id.clone(),
                    })
                })
                .collect(),
        });
        validate_bindings(&state.bindings)?;
        // Only sessions with an authoritative snapshot can prove that a pane is
        // gone. Bindings in unobserved sessions stay dormant so a stopped (or
        // not yet running) Herdr session does not silently lose its keys.
        let observed = self.panes.observed_sessions()?;
        state.bindings.retain(|binding| {
            !observed.contains(&binding.session)
                || panes
                    .iter()
                    .any(|pane| pane.session == binding.session && pane.pane_id == binding.pane_id)
        });
        self.store.save(&state)?;

        for pane in panes {
            match state
                .bindings
                .iter()
                .find(|binding| binding.session == pane.session && binding.pane_id == pane.pane_id)
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

/// Preserve the original single-session error text for the default session, and
/// name the session otherwise so multi-session mistakes stay diagnosable.
fn pane_missing_message(pane: &PaneRef) -> String {
    if pane.session == DEFAULT_SESSION {
        "pane no longer exists".into()
    } else {
        format!("pane no longer exists in session {}", pane.session)
    }
}

#[derive(Debug, Serialize)]
pub struct BindResult {
    pub key: String,
    pub session: String,
    pub pane_id: String,
    pub changed: bool,
}

pub fn valid_key(key: &str) -> Result<&str> {
    KEY_IDS
        .iter()
        .copied()
        .find(|candidate| *candidate == key)
        .context("key must be f1 through f10")
}

pub fn first_free_key(bindings: &[Binding]) -> Option<&'static str> {
    KEY_IDS
        .into_iter()
        .find(|key| !bindings.iter().any(|binding| binding.key == *key))
}

pub fn validate_bindings(bindings: &[Binding]) -> Result<()> {
    for binding in bindings {
        valid_key(&binding.key)?;
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
                || (other.session == binding.session && other.pane_id == binding.pane_id)
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
        panes: RefCell<Vec<Pane>>,
    }

    impl FakePanes {
        fn new(panes: Vec<Pane>) -> Self {
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

    impl PaneDirectory for FakePanes {
        fn panes(&self) -> Result<Vec<Pane>> {
            Ok(self.panes.borrow().clone())
        }

        fn observed_sessions(&self) -> Result<std::collections::BTreeSet<String>> {
            Ok(self.sessions.iter().cloned().collect())
        }

        fn write_fkey(&self, pane: &PaneRef, key: Option<&str>) -> Result<()> {
            let mut panes = self.panes.borrow_mut();
            let entry = panes
                .iter_mut()
                .find(|candidate| {
                    candidate.session == pane.session && candidate.pane_id == pane.pane_id
                })
                .context("unknown pane")?;
            match key {
                Some(key) => entry.tokens.insert("fkey".into(), key.into()),
                None => entry.tokens.remove("fkey"),
            };
            Ok(())
        }

        fn write_presentation_tokens(
            &self,
            _pane: &PaneRef,
            _binding: &str,
        ) -> Result<PresentationTokenWrite> {
            Ok(PresentationTokenWrite::Written)
        }
        fn focus_pane(&self, _pane: &PaneRef) -> Result<()> {
            Ok(())
        }

        fn send_keys(&self, _pane: &PaneRef, _keys: &[&str]) -> Result<()> {
            Ok(())
        }
    }

    fn pane_in(session: &str, id: &str) -> Pane {
        Pane {
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

    fn pane(id: &str) -> Pane {
        pane_in(DEFAULT_SESSION, id)
    }

    fn reference(id: &str) -> PaneRef {
        PaneRef::new(DEFAULT_SESSION, id)
    }

    #[test]
    fn assigns_the_first_free_key_and_mirrors_it() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1")]);
        let service = BindingService::new(&store, &panes);
        let result = service.bind(&reference("p1"), None).unwrap();
        assert_eq!(result.key, "f1");
        assert_eq!(result.session, DEFAULT_SESSION);
        assert_eq!(panes.panes().unwrap()[0].fkey(), Some("f1"));
    }

    #[test]
    fn implicit_rebind_keeps_the_existing_key() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1"), pane("p2")]);
        let service = BindingService::new(&store, &panes);
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
        let service = BindingService::new(&store, &panes);

        let first = service
            .bind(&PaneRef::new(DEFAULT_SESSION, "w1:p1"), Some("f1"))
            .unwrap();
        assert_eq!(first.session, DEFAULT_SESSION);
        let second = service
            .bind(&PaneRef::new("agent-workspace", "w1:p1"), Some("f2"))
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
    fn a_missing_pane_names_its_session_outside_the_default() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1")]);
        let service = BindingService::new(&store, &panes);

        let default_error = service.bind(&reference("gone"), None).unwrap_err();
        assert_eq!(default_error.to_string(), "pane no longer exists");
        let named_error = service
            .bind(&PaneRef::new("agent-workspace", "gone"), None)
            .unwrap_err();
        assert_eq!(
            named_error.to_string(),
            "pane no longer exists in session agent-workspace"
        );
    }

    #[test]
    fn keeps_bindings_for_unobserved_sessions_dormant() {
        let store = MemoryStore::default();
        let panes = FakePanes::new(vec![pane("p1")]);
        let service = BindingService::new(&store, &panes);
        service.bind(&reference("p1"), Some("f1")).unwrap();
        {
            let mut state = store.0.borrow_mut();
            let state = state.as_mut().unwrap();
            state.bindings.push(Binding {
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
        let service = BindingService::new(&store, &panes);
        service.bind(&reference("p1"), Some("f4")).unwrap();

        assert_eq!(
            service.panes().unwrap(),
            vec![
                PanePresentation {
                    session: DEFAULT_SESSION.into(),
                    pane_id: "p1".into(),
                    title: "Inspect task".into(),
                    binding: "F4".into(),
                    agent_status: "idle".into(),
                    focused: false,
                },
                PanePresentation {
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
                key: "f1".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "p1".into(),
            },
            Binding {
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
                key: "f1".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "w1:p1".into(),
            },
            Binding {
                key: "f2".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "w1:p1".into(),
            },
        ];
        assert!(validate_bindings(&duplicate).is_err());

        let distinct_sessions = vec![
            Binding {
                key: "f1".into(),
                session: DEFAULT_SESSION.into(),
                pane_id: "w1:p1".into(),
            },
            Binding {
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
        let service = BindingService::new(&store, &panes);
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
        let service = BindingService::new(&store, &panes);
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
        let service = BindingService::new(&store, &panes);
        service.bind(&reference("p1"), Some("f2")).unwrap();

        panes.panes.borrow_mut().clear();

        assert!(service.status().unwrap().is_empty());
        assert!(store.load().unwrap().unwrap().bindings.is_empty());
    }
}
