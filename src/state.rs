use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf};

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

    pub fn ensure_directory(&self) -> Result<()> {
        fs::create_dir_all(self.directory())
            .with_context(|| format!("create {}", self.directory().display()))?;
        fs::set_permissions(self.directory(), fs::Permissions::from_mode(0o700))
            .with_context(|| format!("protect {}", self.directory().display()))?;
        Ok(())
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
        self.ensure_directory()?;
        let temporary = self
            .directory()
            .join(format!(".bindings-{}.tmp", std::process::id()));
        let contents = serde_json::to_vec_pretty(state)?;
        fs::write(&temporary, contents)
            .with_context(|| format!("write {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .with_context(|| format!("replace {}", self.path.display()))?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("protect {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::core::DEFAULT_SESSION;

    #[test]
    fn reads_pre_multi_session_ledgers_as_the_default_session() {
        let directory = env::temp_dir().join(format!(
            "beckon-state-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("bindings.json");
        fs::write(
            &path,
            r#"{"state_version":1,"bindings":[{"key":"f3","pane_id":"wB:p1A"}]}"#,
        )
        .unwrap();

        let store = JsonBindingStore { path };
        let state = store.load().unwrap().unwrap();
        assert_eq!(state.bindings.len(), 1);
        assert_eq!(state.bindings[0].session, DEFAULT_SESSION);
        assert_eq!(state.bindings[0].pane_id, "wB:p1A");

        fs::remove_dir_all(directory).unwrap();
    }
}
