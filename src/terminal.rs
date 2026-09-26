//! Terminal surface control: enumerate surfaces, raise one by opaque handle.
//!
//! This is the control plane described in
//! `docs/terminal-backend-architecture.md`, and it separates three concerns on
//! purpose:
//!
//! - [`TerminalBackend`] adapters enumerate and focus surfaces. They know
//!   nothing about Herdr, sessions, bindings, or layouts.
//! - [`SurfaceStore`] records which surface currently displays which session.
//!   Only the explicit `adopt` and `forget` commands write it.
//! - [`TerminalLink`] joins the two for the navigation path: it raises the
//!   adopted surface for a session, or does nothing when none is recorded.
//!
//! Nothing in this module infers a session-to-surface mapping, repairs a stale
//! one, or creates surfaces. Those are deliberate omissions: different people
//! arrange different desktops, and Beckon ships primitives rather than a layout
//! model.

use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{TerminalBackendKind, TerminalConfig};
use crate::state::{save_private_json, state_directory};

pub mod ghostty;

/// Opaque per-backend surface identity. The string is meaningful only to the
/// backend that produced it; nothing above this boundary interprets one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct SurfaceHandle(String);

impl SurfaceHandle {
    pub fn new(handle: impl Into<String>) -> Self {
        Self(handle.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SurfaceHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One live terminal surface. Titles and window/tab names are human-facing
/// descriptions; only `handle` is used for actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    pub handle: SurfaceHandle,
    pub title: String,
    pub window: String,
    pub tab: String,
}

/// The pluggable terminal control plane. Implementations are selected by
/// `[terminal] backend` and must stay session-blind.
pub trait TerminalBackend {
    /// Stable registry name, recorded alongside adopted handles.
    fn id(&self) -> &str;
    fn list_surfaces(&self) -> Result<Vec<Surface>>;
    fn focus_surface(&self, handle: &SurfaceHandle) -> Result<()>;
}

/// Build the configured backend. `None` means the pre-backend behavior: the
/// user's focus command is the only surface mechanism.
pub fn from_config(config: &TerminalConfig) -> Result<Option<TerminalLink>> {
    match config.backend {
        TerminalBackendKind::None => Ok(None),
        TerminalBackendKind::GhosttyAppleScript => Ok(Some(TerminalLink::new(
            Box::new(ghostty::GhosttyAppleScript::default()),
            SurfaceStore::from_environment(),
        ))),
    }
}

/// One explicit session-to-surface record. `backend` names the backend that
/// produced the handle so a configuration change invalidates the record
/// honestly instead of silently mis-firing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceRecord {
    pub backend: String,
    pub session: String,
    pub handle: SurfaceHandle,
}

pub const SURFACE_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceState {
    pub version: u32,
    #[serde(default)]
    pub surfaces: Vec<SurfaceRecord>,
}

impl Default for SurfaceState {
    fn default() -> Self {
        Self {
            version: SURFACE_STATE_VERSION,
            surfaces: Vec::new(),
        }
    }
}

/// The adopted-handle store. Machine-local observation, not declarative
/// configuration: it lives in the state directory because configuration is
/// often rendered by Home Manager and would clobber CLI writes.
#[derive(Clone, Debug)]
pub struct SurfaceStore {
    path: PathBuf,
}

impl SurfaceStore {
    pub fn from_environment() -> Self {
        Self {
            path: state_directory().join("surfaces.json"),
        }
    }

    pub fn load(&self) -> Result<SurfaceState> {
        if !self.path.exists() {
            return Ok(SurfaceState::default());
        }
        let contents = std::fs::read_to_string(&self.path)
            .with_context(|| format!("read {}", self.path.display()))?;
        let state: SurfaceState = serde_json::from_str(&contents)
            .with_context(|| format!("parse {}", self.path.display()))?;
        if state.version != SURFACE_STATE_VERSION {
            bail!(
                "{} has version {}; this Beckon version supports {}",
                self.path.display(),
                state.version,
                SURFACE_STATE_VERSION
            );
        }
        validate_surfaces(&state.surfaces)?;
        Ok(state)
    }

    pub fn get(&self, session: &str) -> Result<Option<SurfaceRecord>> {
        Ok(self
            .load()?
            .surfaces
            .into_iter()
            .find(|record| record.session == session))
    }

    /// Record (or replace) the surface for one session.
    pub fn record(&self, record: SurfaceRecord) -> Result<()> {
        let mut state = self.load()?;
        state
            .surfaces
            .retain(|existing| existing.session != record.session);
        state.surfaces.push(record);
        state
            .surfaces
            .sort_by(|left, right| left.session.cmp(&right.session));
        validate_surfaces(&state.surfaces)?;
        save_private_json(&self.path, &state)
    }

    /// Remove the surface record for one session. Returns whether one existed.
    pub fn forget(&self, session: &str) -> Result<bool> {
        let mut state = self.load()?;
        let before = state.surfaces.len();
        state.surfaces.retain(|record| record.session != session);
        if state.surfaces.len() == before {
            return Ok(false);
        }
        save_private_json(&self.path, &state)?;
        Ok(true)
    }
}

fn validate_surfaces(surfaces: &[SurfaceRecord]) -> Result<()> {
    for record in surfaces {
        if record.backend.trim().is_empty() {
            bail!("an adopted surface has an empty backend");
        }
        if record.session.trim().is_empty() {
            bail!("an adopted surface has an empty session");
        }
        if record.handle.as_str().trim().is_empty() {
            bail!("an adopted surface has an empty handle");
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for record in surfaces {
        if !seen.insert(record.session.as_str()) {
            bail!("adopted surfaces must have unique sessions");
        }
    }
    Ok(())
}

/// The navigation-time join of a backend and the adopted handle store.
pub struct TerminalLink {
    backend: Box<dyn TerminalBackend>,
    store: SurfaceStore,
}

impl TerminalLink {
    pub fn new(backend: Box<dyn TerminalBackend>, store: SurfaceStore) -> Self {
        Self { backend, store }
    }

    pub fn backend(&self) -> &dyn TerminalBackend {
        &*self.backend
    }

    pub fn store(&self) -> &SurfaceStore {
        &self.store
    }

    /// Raise the surface adopted for `session`.
    ///
    /// - `Ok(None)`: nothing adopted; navigation proceeds unchanged.
    /// - `Ok(Some(handle))`: the surface was raised.
    /// - `Err`: the record exists but could not be honored (backend changed,
    ///   stale handle, focus failure). Callers warn and continue; they never
    ///   guess or re-adopt.
    pub fn raise_for_session(&self, session: &str) -> Result<Option<SurfaceHandle>> {
        let Some(record) = self.store.get(session)? else {
            return Ok(None);
        };
        if record.backend != self.backend.id() {
            bail!(
                "session {session} is adopted for backend {} but {} is configured; re-run `beckon adopt`",
                record.backend,
                self.backend.id()
            );
        }
        self.backend
            .focus_surface(&record.handle)
            .with_context(|| {
                format!(
                    "raise adopted surface {} for session {session}",
                    record.handle
                )
            })?;
        Ok(Some(record.handle))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct FakeBackend {
        id: &'static str,
        surfaces: Vec<Surface>,
        focused: RefCell<Vec<SurfaceHandle>>,
        fail_focus: bool,
    }

    impl FakeBackend {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                surfaces: vec![Surface {
                    handle: SurfaceHandle::new("handle-1"),
                    title: "session view".into(),
                    window: "w1".into(),
                    tab: "t1".into(),
                }],
                focused: RefCell::new(Vec::new()),
                fail_focus: false,
            }
        }
    }

    impl TerminalBackend for FakeBackend {
        fn id(&self) -> &str {
            self.id
        }

        fn list_surfaces(&self) -> Result<Vec<Surface>> {
            Ok(self.surfaces.clone())
        }

        fn focus_surface(&self, handle: &SurfaceHandle) -> Result<()> {
            if self.fail_focus {
                bail!("no such surface");
            }
            self.focused.borrow_mut().push(handle.clone());
            Ok(())
        }
    }

    struct TestStore(SurfaceStore);

    impl TestStore {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "beckon-surfaces-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            Self(SurfaceStore {
                path: directory.join("surfaces.json"),
            })
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let directory = self.0.path.parent().unwrap();
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    fn record(backend: &str, session: &str) -> SurfaceRecord {
        SurfaceRecord {
            backend: backend.into(),
            session: session.into(),
            handle: SurfaceHandle::new(format!("{session}-handle")),
        }
    }

    #[test]
    fn records_round_trip_and_replace_per_session() {
        let store = TestStore::new();
        assert_eq!(store.0.load().unwrap().surfaces.len(), 0);

        store
            .0
            .record(record("ghostty-applescript", "agent-workspace"))
            .unwrap();
        store
            .0
            .record(record("ghostty-applescript", "default"))
            .unwrap();
        store
            .0
            .record(record("ghostty-applescript", "agent-workspace"))
            .unwrap();

        let state = store.0.load().unwrap();
        assert_eq!(state.surfaces.len(), 2);
        assert_eq!(state.surfaces[0].session, "agent-workspace");
        assert_eq!(state.surfaces[1].session, "default");

        assert!(store.0.forget("agent-workspace").unwrap());
        assert!(!store.0.forget("agent-workspace").unwrap());
        assert_eq!(store.0.load().unwrap().surfaces.len(), 1);
    }

    #[test]
    fn rejects_empty_records_and_a_wrong_store_version() {
        let store = TestStore::new();
        let error = store
            .0
            .record(SurfaceRecord {
                backend: "ghostty-applescript".into(),
                session: "default".into(),
                handle: SurfaceHandle::new("  "),
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("empty handle"), "{error:#}");

        std::fs::create_dir_all(store.0.path.parent().unwrap()).unwrap();
        std::fs::write(&store.0.path, r#"{"version":9,"surfaces":[]}"#).unwrap();
        let error = store.0.load().unwrap_err();
        assert!(format!("{error:#}").contains("supports 1"), "{error:#}");
    }

    #[test]
    fn raises_only_the_adopted_surface_for_the_session() {
        let store = TestStore::new();
        store
            .0
            .record(record("ghostty-applescript", "agent-workspace"))
            .unwrap();
        let backend = FakeBackend::new("ghostty-applescript");
        let link = TerminalLink::new(Box::new(backend), store.0.clone());

        assert_eq!(link.raise_for_session("default").unwrap(), None);
        assert_eq!(
            link.raise_for_session("agent-workspace").unwrap(),
            Some(SurfaceHandle::new("agent-workspace-handle"))
        );
    }

    #[test]
    fn a_changed_backend_or_stale_handle_degrades_without_guessing() {
        let store = TestStore::new();
        store.0.record(record("other-backend", "default")).unwrap();
        let link = TerminalLink::new(
            Box::new(FakeBackend::new("ghostty-applescript")),
            store.0.clone(),
        );
        let error = link.raise_for_session("default").unwrap_err();
        assert!(
            format!("{error:#}").contains("re-run `beckon adopt`"),
            "{error:#}"
        );

        let store = TestStore::new();
        store
            .0
            .record(record("ghostty-applescript", "default"))
            .unwrap();
        let mut backend = FakeBackend::new("ghostty-applescript");
        backend.fail_focus = true;
        let link = TerminalLink::new(Box::new(backend), store.0.clone());
        let error = link.raise_for_session("default").unwrap_err();
        assert!(
            format!("{error:#}").contains("raise adopted surface"),
            "{error:#}"
        );
    }
}
