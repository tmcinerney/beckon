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
    pub input: InputConfig,
    #[serde(default)]
    pub focus: FocusConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub actions: ActionsConfig,
}

/// Selects the physical source that invokes Beckon's logical F1 through F10
/// bindings. This affects navigation only; displays remain independently
/// configured under `[display]`.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum InputProfile {
    /// Glove80's Beckon layer: F16-F20 and Shift+F16-F20. This deliberately
    /// avoids colliding with macOS's normal function-key behavior.
    #[default]
    Glove80,
    /// The MacBook's physical F1-F10 row. macOS must be configured to emit
    /// standard function keys before this profile can receive them.
    MacbookFunctionKeys,
}

impl InputProfile {
    /// Stable configuration and diagnostic name for this physical source.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Glove80 => "glove80",
            Self::MacbookFunctionKeys => "macbook-function-keys",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    /// Compatibility spelling for selecting exactly one profile. New
    /// configurations should use `profiles` so multiple physical keyboards
    /// can activate the same logical Beckon slots concurrently.
    #[serde(default)]
    profile: Option<InputProfile>,
    /// Physical input profiles to enable together. Their shortcuts must be
    /// distinct, but profiles may map those shortcuts to the same logical
    /// Beckon slots.
    #[serde(default)]
    profiles: Option<Vec<InputProfile>>,
}

impl InputConfig {
    /// Resolves the enabled physical input profiles.
    ///
    /// Leaving `[input]` absent preserves the original Glove80-only behavior.
    /// `profile` remains supported for existing configurations, while
    /// `profiles` permits multiple sources such as a desktop Glove80 and a
    /// mobile MacBook keyboard.
    pub fn enabled_profiles(&self) -> Result<Vec<InputProfile>> {
        if self.profile.is_some() && self.profiles.is_some() {
            bail!(
                "input.profile and input.profiles cannot be used together; use input.profiles for multiple inputs"
            );
        }

        let profiles = match (self.profile, &self.profiles) {
            (Some(profile), None) => vec![profile],
            (None, None) => vec![InputProfile::default()],
            (None, Some(profiles)) => profiles.clone(),
            (Some(_), Some(_)) => unreachable!("mixed input configuration is rejected above"),
        };

        if profiles.is_empty() {
            bail!("input.profiles must enable at least one profile");
        }

        let mut seen = std::collections::BTreeSet::new();
        for profile in &profiles {
            if !seen.insert(*profile) {
                bail!("input.profiles contains duplicate profile {profile:?}");
            }
        }
        Ok(profiles)
    }
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
    #[serde(default = "default_confirm_keys")]
    pub keys: Vec<String>,
}

impl Default for ConfirmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            repeat_press_ms: default_repeat_press_ms(),
            keys: default_confirm_keys(),
        }
    }
}

fn default_repeat_press_ms() -> u64 {
    750
}

fn default_confirm_keys() -> Vec<String> {
    vec!["enter".into()]
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
            input: InputConfig::default(),
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
    if config.actions.confirm.keys.is_empty()
        || config
            .actions
            .confirm
            .keys
            .iter()
            .any(|key| key.trim().is_empty())
    {
        bail!("actions.confirm.keys must contain one or more logical Herdr key names");
    }
    config.input.enabled_profiles()?;
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

# Select the physical input sources for Beckon's logical F1 through F10
# bindings. The default preserves the Glove80 Beckon-layer mappings:
# F16-F20, then Shift+F16-Shift+F20. To also navigate with the built-in
# MacBook keyboard, uncomment this and enable "Use F1, F2, etc. keys as
# standard function keys" in macOS Keyboard settings. Hold Fn/Globe when you
# need the normal macOS brightness, media, or volume behavior.
# [input]
# profiles = ["glove80", "macbook-function-keys"]

# Optional: repeat the same bound F key within this window after Beckon has
# focused it to send Enter to that pane. Disabled by default because Enter can
# confirm whatever is selected in an agent UI.
# [actions.confirm]
# enabled = true
# repeat_press_ms = 750
# keys = ["enter"]

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
        assert_eq!(
            config.input.enabled_profiles().unwrap(),
            [InputProfile::Glove80]
        );
        assert!(!config.actions.confirm.enabled);
        assert_eq!(config.actions.confirm.repeat_press_ms, 750);
        assert_eq!(config.actions.confirm.keys, ["enter"]);
    }

    #[test]
    fn confirmation_allows_a_user_selected_logical_key_sequence() {
        let config = parse(
            r#"
config_version = 2
[actions.confirm]
enabled = true
keys = ["ctrl+c"]
"#,
        )
        .unwrap();
        assert_eq!(config.actions.confirm.keys, ["ctrl+c"]);
    }

    #[test]
    fn parses_opt_in_macbook_function_key_input() {
        let config = parse(
            r#"
config_version = 2
[input]
profile = "macbook-function-keys"
"#,
        )
        .unwrap();

        assert_eq!(
            config.input.enabled_profiles().unwrap(),
            [InputProfile::MacbookFunctionKeys]
        );
    }

    #[test]
    fn enables_glove80_and_macbook_inputs_together() {
        let config = parse(
            r#"
config_version = 2
[input]
profiles = ["glove80", "macbook-function-keys"]
"#,
        )
        .unwrap();

        assert_eq!(
            config.input.enabled_profiles().unwrap(),
            [InputProfile::Glove80, InputProfile::MacbookFunctionKeys]
        );
    }

    #[test]
    fn rejects_ambiguous_or_duplicate_input_profiles() {
        let mixed = parse(
            r#"
config_version = 2
[input]
profile = "glove80"
profiles = ["macbook-function-keys"]
"#,
        )
        .unwrap();
        assert!(mixed.input.enabled_profiles().is_err());

        let duplicate = parse(
            r#"
config_version = 2
[input]
profiles = ["glove80", "glove80"]
"#,
        )
        .unwrap();
        assert!(duplicate.input.enabled_profiles().is_err());

        let empty = parse(
            r#"
config_version = 2
[input]
profiles = []
"#,
        )
        .unwrap();
        assert!(empty.input.enabled_profiles().is_err());
    }

    #[test]
    fn rejects_unknown_config_versions() {
        assert!(parse("config_version = 9").is_err());
    }
}
