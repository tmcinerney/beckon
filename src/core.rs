use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const KEY_IDS: [&str; 10] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10"];
pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub key: String,
    pub pane_id: String,
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
    pub tokens: BTreeMap<String, String>,
}

impl Pane {
    pub fn fkey(&self) -> Option<&str> {
        self.tokens.get("fkey").map(String::as_str)
    }
}

/// Durable storage for the binding ledger. The ledger, not Herdr metadata, is
/// authoritative because Herdr tokens are intentionally not restart-durable.
pub trait BindingStore {
    fn load(&self) -> Result<Option<BindingState>>;
    fn save(&self, state: &BindingState) -> Result<()>;
}

/// The minimal Herdr surface Beckon's binding policy needs. The socket-backed
/// adapter will replace the current CLI implementation without changing core.
pub trait PaneDirectory {
    fn panes(&self) -> Result<Vec<Pane>>;
    fn write_fkey(&self, pane_id: &str, key: Option<&str>) -> Result<()>;
    fn focus_pane(&self, pane_id: &str) -> Result<()>;
}

pub struct BindingService<'a> {
    store: &'a dyn BindingStore,
    panes: &'a dyn PaneDirectory,
}

impl<'a> BindingService<'a> {
    pub fn new(store: &'a dyn BindingStore, panes: &'a dyn PaneDirectory) -> Self {
        Self { store, panes }
    }

    pub fn bind(&self, pane_id: &str, requested_key: Option<&str>) -> Result<BindResult> {
        let panes = self.panes.panes()?;
        if !panes.iter().any(|pane| pane.pane_id == pane_id) {
            bail!("pane no longer exists");
        }
        let mut state = self.reconcile_panes(&panes)?;
        let key = match requested_key {
            Some(key) => valid_key(key)?.to_string(),
            None => first_free_key(&state.bindings)
                .context("no Beckon keys are free")?
                .to_string(),
        };

        if let Some(owner) = state.bindings.iter().find(|binding| binding.key == key)
            && owner.pane_id != pane_id
        {
            bail!("{key} is already bound to {}", owner.pane_id);
        }
        if state
            .bindings
            .iter()
            .any(|binding| binding.pane_id == pane_id && binding.key == key)
        {
            return Ok(BindResult {
                key,
                pane_id: pane_id.to_string(),
                changed: false,
            });
        }

        state.bindings.retain(|binding| binding.pane_id != pane_id);
        state.bindings.push(Binding {
            key: key.clone(),
            pane_id: pane_id.to_string(),
        });
        self.store.save(&state)?;
        self.panes.write_fkey(pane_id, Some(&key))?;
        Ok(BindResult {
            key,
            pane_id: pane_id.to_string(),
            changed: true,
        })
    }

    pub fn release(&self, pane_id: &str) -> Result<bool> {
        let panes = self.panes.panes()?;
        if !panes.iter().any(|pane| pane.pane_id == pane_id) {
            bail!("pane no longer exists");
        }
        let mut state = self.reconcile_panes(&panes)?;
        let before = state.bindings.len();
        state.bindings.retain(|binding| binding.pane_id != pane_id);
        if state.bindings.len() == before {
            return Ok(false);
        }
        self.store.save(&state)?;
        self.panes.write_fkey(pane_id, None)?;
        Ok(true)
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
                    .find(|pane| pane.pane_id == binding.pane_id)
                    .cloned()
                    .map(|pane| (binding, pane))
            })
            .collect::<Vec<_>>();
        bindings.sort_by(|left, right| left.0.key.cmp(&right.0.key));
        Ok(bindings)
    }

    pub fn pane_for_key(&self, key: &str) -> Result<String> {
        let key = valid_key(key)?;
        let panes = self.panes.panes()?;
        let state = self.reconcile_panes(&panes)?;
        state
            .bindings
            .iter()
            .find(|binding| binding.key == key)
            .map(|binding| binding.pane_id.clone())
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
                        pane_id: pane.pane_id.clone(),
                    })
                })
                .collect(),
        });
        validate_bindings(&state.bindings)?;
        state
            .bindings
            .retain(|binding| panes.iter().any(|pane| pane.pane_id == binding.pane_id));
        self.store.save(&state)?;

        for pane in panes {
            match state
                .bindings
                .iter()
                .find(|binding| binding.pane_id == pane.pane_id)
            {
                Some(expected) if pane.fkey() != Some(expected.key.as_str()) => {
                    self.panes.write_fkey(&pane.pane_id, Some(&expected.key))?;
                }
                None if !imported_tokens && pane.fkey().is_some() => {
                    self.panes.write_fkey(&pane.pane_id, None)?;
                }
                _ => {}
            }
        }
        Ok(state)
    }
}

#[derive(Debug, Serialize)]
pub struct BindResult {
    pub key: String,
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
        if binding.pane_id.is_empty() {
            bail!("a binding has an empty pane_id");
        }
    }
    for (index, binding) in bindings.iter().enumerate() {
        if bindings[index + 1..]
            .iter()
            .any(|other| other.key == binding.key || other.pane_id == binding.pane_id)
        {
            bail!("bindings must have unique keys and pane IDs");
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

    struct FakePanes(RefCell<Vec<Pane>>);
    impl PaneDirectory for FakePanes {
        fn panes(&self) -> Result<Vec<Pane>> {
            Ok(self.0.borrow().clone())
        }
        fn write_fkey(&self, pane_id: &str, key: Option<&str>) -> Result<()> {
            let mut panes = self.0.borrow_mut();
            let pane = panes
                .iter_mut()
                .find(|pane| pane.pane_id == pane_id)
                .context("unknown pane")?;
            match key {
                Some(key) => pane.tokens.insert("fkey".into(), key.into()),
                None => pane.tokens.remove("fkey"),
            };
            Ok(())
        }
        fn focus_pane(&self, _pane_id: &str) -> Result<()> {
            Ok(())
        }
    }

    fn pane(id: &str) -> Pane {
        Pane {
            pane_id: id.into(),
            revision: 0,
            agent_status: "idle".into(),
            agent: None,
            label: None,
            cwd: None,
            tokens: BTreeMap::new(),
        }
    }

    #[test]
    fn assigns_the_first_free_key_and_mirrors_it() {
        let store = MemoryStore::default();
        let panes = FakePanes(RefCell::new(vec![pane("p1")]));
        let service = BindingService::new(&store, &panes);
        let result = service.bind("p1", None).unwrap();
        assert_eq!(result.key, "f1");
        assert_eq!(panes.panes().unwrap()[0].fkey(), Some("f1"));
    }

    #[test]
    fn rejects_duplicate_binding_keys() {
        let bindings = vec![
            Binding {
                key: "f1".into(),
                pane_id: "p1".into(),
            },
            Binding {
                key: "f1".into(),
                pane_id: "p2".into(),
            },
        ];
        assert!(validate_bindings(&bindings).is_err());
    }
}
