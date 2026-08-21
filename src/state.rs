use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::core::{BindingState, BindingStore, STATE_VERSION, validate_bindings};

pub struct JsonBindingStore {
    path: PathBuf,
}

impl JsonBindingStore {
    pub fn from_environment() -> Self {
        let directory = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .unwrap_or_else(env::temp_dir)
            .join("beckon");
        Self {
            path: directory.join("bindings.json"),
        }
    }

    pub fn directory(&self) -> &std::path::Path {
        self.path.parent().expect("bindings path has a parent")
    }
}

impl BindingStore for JsonBindingStore {
    fn load(&self) -> Result<Option<BindingState>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let contents = fs::read_to_string(&self.path)
            .with_context(|| format!("read {}", self.path.display()))?;
        let state: BindingState = serde_json::from_str(&contents)
            .with_context(|| format!("parse {}", self.path.display()))?;
        if state.state_version != STATE_VERSION {
            bail!(
                "{} has state_version {}; this Beckon version supports {}",
                self.path.display(),
                state.state_version,
                STATE_VERSION
            );
        }
        validate_bindings(&state.bindings)?;
        Ok(Some(state))
    }

    fn save(&self, state: &BindingState) -> Result<()> {
        validate_bindings(&state.bindings)?;
        fs::create_dir_all(self.directory())
            .with_context(|| format!("create {}", self.directory().display()))?;
        let temporary = self
            .directory()
            .join(format!(".bindings-{}.tmp", std::process::id()));
        let contents = serde_json::to_vec_pretty(state)?;
        fs::write(&temporary, contents)
            .with_context(|| format!("write {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .with_context(|| format!("replace {}", self.path.display()))?;
        Ok(())
    }
}
