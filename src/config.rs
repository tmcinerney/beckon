use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::render::DisplayConfig;

const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    config_version: u32,
    #[serde(default)]
    pub focus: FocusConfig,
    #[serde(default)]
    pub display: DisplayConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusConfig {
    /// Executable followed by arguments. It runs before `herdr agent focus`.
    pub command: Option<Vec<String>>,
}

pub fn path() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(env::temp_dir)
        .join("beckon/config.toml")
}

pub fn initialize() -> Result<()> {
    let path = path();
    if path.exists() {
        bail!(
            "{} already exists; Beckon will not overwrite it",
            path.display()
        );
    }
    let directory = path.parent().expect("config path has a parent");
    fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    fs::write(&path, DEFAULT_CONFIG).with_context(|| format!("write {}", path.display()))?;
    println!("created {}", path.display());
    Ok(())
}

pub fn load() -> Result<Config> {
    let path = path();
    let contents = fs::read_to_string(&path).with_context(|| {
        format!(
            "read {} (run `beckon init` to create a configuration template)",
            path.display()
        )
    })?;
    let config: Config =
        toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;
    if config.config_version != CONFIG_VERSION {
        bail!(
            "{} has config_version {}; this Beckon version supports {}",
            path.display(),
            config.config_version,
            CONFIG_VERSION
        );
    }
    if let Some(command) = &config.focus.command
        && command.is_empty()
    {
        bail!("focus.command must contain an executable when set");
    }
    config.display.validate()?;
    Ok(config)
}

const DEFAULT_CONFIG: &str = r#"# Beckon's portable settings. Machine-specific focus behavior is optional.
config_version = 1

# Run this before Beckon focuses a Herdr agent pane. Leave commented out when
# Ghostty is already frontmost. Use an executable and its arguments, not a shell
# string, so no shell quoting or interpolation is involved.
# [focus]
# command = ["/Users/you/.config/beckon/focus-ghostty"]

# Display settings have safe defaults. Override these only to tune the physical
# keyboard once Beckon has a compatible LED firmware.
# [display.states.working]
# brightness = 0.6
# motion = "breathe"
"#;
