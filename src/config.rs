use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::render::{DisplayConfig, Motion, Rgb, StateTreatment};

const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    config_version: u32,
    #[serde(default)]
    pub focus: FocusConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub actions: ActionsConfig,
}

/// Deliberately opt-in actions that can send input to a bound pane.
///
/// Beckon's default remains display and navigation only. A user must enable an
/// action explicitly because it can have effects inside an agent session.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionsConfig {
    #[serde(default)]
    pub confirm: ConfirmConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_repeat_press_ms")]
    pub repeat_press_ms: u64,
}

impl Default for ConfirmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            repeat_press_ms: default_repeat_press_ms(),
        }
    }
}

fn default_repeat_press_ms() -> u64 {
    750
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusConfig {
    /// Executable followed by arguments. It runs before `herdr agent focus`.
    pub command: Option<Vec<String>>,
}

/// Compatibility reader for config version 1. Version 1 had no named themes;
/// the fields are applied to a generated `legacy-v1` theme at load time so an
/// existing installation keeps working until its owner chooses to migrate.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigV1 {
    #[serde(rename = "config_version")]
    _config_version: u32,
    #[serde(default)]
    focus: FocusConfig,
    #[serde(default)]
    display: LegacyDisplayConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDisplayConfig {
    #[serde(default)]
    states: LegacyStateTreatments,
    // V1 used per-key identity colors. Themes intentionally use one semantic
    // treatment per state, so preserve the field while ignoring it here.
    #[serde(default)]
    #[serde(rename = "keys")]
    _keys: Vec<toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyStateTreatments {
    #[serde(default)]
    idle: LegacyStateTreatment,
    #[serde(default)]
    working: LegacyStateTreatment,
    #[serde(default)]
    blocked: LegacyStateTreatment,
    #[serde(default)]
    done: LegacyStateTreatment,
    #[serde(default)]
    unknown: LegacyStateTreatment,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyStateTreatment {
    brightness: Option<f32>,
    motion: Option<Motion>,
    #[serde(alias = "colour")]
    color: Option<Rgb>,
}

impl ConfigV1 {
    fn upgrade(self) -> Config {
        let mut display = DisplayConfig::default();
        let mut theme = display
            .selected_theme()
            .expect("built-in default theme must exist");
        apply_legacy_treatment(&mut theme.idle, self.display.states.idle);
        apply_legacy_treatment(&mut theme.working, self.display.states.working);
        apply_legacy_treatment(&mut theme.blocked, self.display.states.blocked);
        apply_legacy_treatment(&mut theme.done, self.display.states.done);
        apply_legacy_treatment(&mut theme.unknown, self.display.states.unknown);
        display.theme = "legacy-v1".into();
        display.themes.insert("legacy-v1".into(), theme);
        Config {
            config_version: CONFIG_VERSION,
            focus: self.focus,
            display,
            actions: ActionsConfig::default(),
        }
    }
}

fn apply_legacy_treatment(target: &mut StateTreatment, legacy: LegacyStateTreatment) {
    if let Some(brightness) = legacy.brightness {
        target.brightness = brightness;
    }
    if let Some(motion) = legacy.motion {
        target.motion = motion;
    }
    if let Some(color) = legacy.color {
        target.color = color;
    }
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
    let config = parse(&contents).with_context(|| format!("parse {}", path.display()))?;
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
    if config.actions.confirm.repeat_press_ms == 0 {
        bail!("actions.confirm.repeat_press_ms must be greater than zero");
    }
    config.display.validate()?;
    Ok(config)
}

fn parse(contents: &str) -> Result<Config> {
    let version = toml::from_str::<toml::Value>(contents)?
        .get("config_version")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| anyhow::anyhow!("config_version must be an integer"))?;
    match version {
        1 => Ok(toml::from_str::<ConfigV1>(contents)?.upgrade()),
        value if value == CONFIG_VERSION as i64 => Ok(toml::from_str(contents)?),
        other => bail!(
            "config_version {other} is unsupported; this Beckon version supports 1 and {CONFIG_VERSION}"
        ),
    }
}

const DEFAULT_CONFIG: &str = r##"# Beckon's portable settings. Machine-specific focus behavior is optional.
config_version = 2

# Run this before Beckon focuses a Herdr agent pane. Leave commented out when
# Ghostty is already frontmost. Use an executable and its arguments, not a shell
# string, so no shell quoting or interpolation is involved.
# [focus]
# command = ["/Users/you/.config/beckon/focus-ghostty"]

# Optional: repeat the same bound F key within this window after Beckon has
# focused it to send Enter to that pane. Disabled by default because Enter can
# confirm whatever is selected in an agent UI.
# [actions.confirm]
# enabled = true
# repeat_press_ms = 750

# The selected theme is resolved by Beckon and sent to the keyboard as concrete
# colors and effects. Firmware never receives a theme name.
[display]
theme = "herdr"

# Built-in themes are available without copying them here. Defining a theme in
# this file adds a new name or replaces a built-in theme of the same name.
# Every theme defines every state so a status is never ambiguous.
[display.themes.herdr]
idle    = { color = "#3BA0FF", brightness = 0.2, motion = "steady" }
working = { color = "#F9E2AF", brightness = 0.6, motion = "breathe" }
blocked = { color = "#F38BA8", brightness = 0.8, motion = "pulse" }
done    = { color = "#A6E3A1", brightness = 0.8, motion = "steady" }
unknown = { color = "#6C7086", brightness = 0.3, motion = "flicker" }
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_a_v1_state_override_into_a_named_theme() {
        let config = parse(
            r##"
config_version = 1
[display.states.working]
brightness = 0.4
motion = "pulse"
colour = "#112233"
"##,
        )
        .unwrap();
        assert_eq!(config.display.theme, "legacy-v1");
        let theme = config.display.selected_theme().unwrap();
        assert_eq!(theme.working.brightness, 0.4);
        assert_eq!(theme.working.motion, Motion::Pulse);
        assert_eq!(theme.working.color.to_string(), "#112233");
        assert!(!config.actions.confirm.enabled);
    }

    #[test]
    fn confirmation_is_disabled_unless_explicitly_enabled() {
        let config = parse("config_version = 2").unwrap();
        assert!(!config.actions.confirm.enabled);
        assert_eq!(config.actions.confirm.repeat_press_ms, 750);
    }

    #[test]
    fn rejects_unknown_config_versions() {
        assert!(parse("config_version = 9").is_err());
    }
}
