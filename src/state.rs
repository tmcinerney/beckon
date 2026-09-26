use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::core::{BindingState, BindingStore, STATE_VERSION, validate_bindings};

/// Beckon's machine-local state directory. It holds files that describe this
/// machine's current facts (the binding ledger, adopted terminal surfaces),
/// as opposed to the declarative configuration and its rendered themes.
pub fn state_directory() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(env::temp_dir)
        .join("beckon")
}

/// Atomically write a private JSON state file: 0700 directory, 0600 file, a
/// temporary sibling, and a rename into place. State files are written only by
/// the component that owns them (the daemon for the ledger, the explicit
/// commands for adopted surfaces).
pub fn save_private_json<T>(path: &Path, value: &T) -> Result<()>
where
    T: ?Sized + Serialize,
{
    let directory = path.parent().expect("state file path has a parent");
    fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("protect {}", directory.display()))?;
    let file_name = path
        .file_name()
        .expect("state file path has a file name")
        .to_string_lossy();
    let temporary = directory.join(format!(".{file_name}.{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect {}", path.display()))?;
    Ok(())
}

pub struct JsonBindingStore {
    path: PathBuf,
}

impl JsonBindingStore {
    pub fn from_environment() -> Self {
        Self {
            path: state_directory().join("bindings.json"),
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
        save_private_json(&self.path, state)
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
