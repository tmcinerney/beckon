use std::{collections::BTreeSet, env, fs, path::PathBuf};

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
    pub outputs: OutputConfig,
    #[serde(default)]
    pub focus: FocusConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub actions: ActionsConfig,
    #[serde(default)]
    pub herdr: HerdrConfig,
    #[serde(default)]
    pub terminal: TerminalConfig,
}

/// Terminal surface control. The backend is a stable registry name; adopted
/// session-to-surface handles are explicit machine-local state and never part
/// of configuration. See `docs/terminal-backend-architecture.md`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalConfig {
    #[serde(default)]
    pub backend: TerminalBackendKind,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalBackendKind {
    /// No terminal control plane. The user's focus command remains the only
    /// surface mechanism, matching pre-backend behavior.
    #[default]
    None,
    /// Ghostty >= 1.3 through its scripting dictionary.
    #[serde(rename = "ghostty")]
    GhosttyAppleScript,
}

impl TerminalBackendKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::GhosttyAppleScript => "ghostty",
        }
    }
}

/// Herdr topology: which server CLI to invoke, where the default session's
/// socket lives, and which sessions Beckon manages.
///
/// Leaving `[herdr]` absent preserves the original behavior: the `herdr`
/// program from `PATH`, the default socket location, and every live session.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HerdrConfig {
    /// The Herdr CLI program to invoke. Under launchd, `PATH` is often narrower
    /// than an interactive shell's, so an absolute path is more predictable.
    #[serde(default = "default_herdr_cli")]
    pub cli: String,
    /// Socket path for the default session. The `BECKON_HERDR_SOCKET`
    /// environment variable still wins for tests and one-off overrides.
    #[serde(default)]
    pub socket: Option<PathBuf>,
    #[serde(default)]
    pub sessions: SessionsConfig,
    /// Sidebar text published as `beckon_binding` for a live pane with no key.
    /// An empty string clears the token so Herdr renders nothing. This is
    /// presentation only: `beckon status` always reports `unbound`.
    #[serde(default = "default_unbound_label")]
    pub unbound_label: String,
}

impl Default for HerdrConfig {
    fn default() -> Self {
        Self {
            cli: default_herdr_cli(),
            socket: None,
            sessions: SessionsConfig::default(),
            unbound_label: default_unbound_label(),
        }
    }
}

fn default_herdr_cli() -> String {
    "herdr".into()
}

fn default_unbound_label() -> String {
    crate::core::UNBOUND.into()
}

impl HerdrConfig {
    pub fn validate(&self) -> Result<()> {
        if self.cli.trim().is_empty() {
            bail!("herdr.cli must name the herdr program when set");
        }
        if let Some(socket) = &self.socket
            && socket.as_os_str().is_empty()
        {
            bail!("herdr.socket must be a path when set");
        }
        if let SessionsConfig::Only(names) = &self.sessions {
            if names.is_empty() {
                bail!("herdr.sessions must name at least one session when set");
            }
            let mut seen = BTreeSet::new();
            for name in names {
                if name.trim().is_empty() {
                    bail!("herdr.sessions must not contain empty session names");
                }
                if !seen.insert(name.as_str()) {
                    bail!("herdr.sessions contains duplicate session {name}");
                }
            }
        }
        Ok(())
    }

    /// `None` manages every live session; `Some` is the explicit allowlist.
    pub fn allowed_sessions(&self) -> Option<&[String]> {
        match &self.sessions {
            SessionsConfig::Auto(_) => None,
            SessionsConfig::Only(names) => Some(names),
        }
    }
}

/// `sessions = "auto"` (default) or an explicit list of session names.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SessionsConfig {
    Auto(AutoSessions),
    Only(Vec<String>),
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AutoSessions {
    Auto,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self::Auto(AutoSessions::Auto)
    }
}

/// Built-in output integrations. The stable names form a configuration
/// registry; they do not expose Beckon's Rust ABI to third-party code.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum OutputAdapter {
    Glove80Usb,
}

impl OutputAdapter {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Glove80Usb => "glove80-usb",
        }
    }
}

/// One trusted external program that consumes Beckon render snapshots.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessOutputPlugin {
    /// Stable name used in diagnostics. IDs share a namespace with built-in
    /// adapters and use lowercase kebab-case.
    pub id: String,
    /// Executable followed by arguments. Beckon never evaluates this through
    /// a shell.
    pub command: Vec<String>,
}

/// Selects built-in and out-of-process display integrations. An empty
/// configuration is valid for navigation-only installations.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    #[serde(default = "default_output_adapters")]
    pub adapters: Vec<OutputAdapter>,
    #[serde(default)]
    pub plugins: Vec<ProcessOutputPlugin>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            adapters: default_output_adapters(),
            plugins: Vec::new(),
        }
    }
}

impl OutputConfig {
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::<&str>::new();
        for adapter in &self.adapters {
            if !seen.insert(adapter.name()) {
                bail!(
                    "outputs.adapters contains duplicate adapter {}",
                    adapter.name()
                );
            }
        }

        for plugin in &self.plugins {
            if !valid_plugin_id(&plugin.id) {
                bail!(
                    "outputs.plugins id {:?} must use lowercase kebab-case",
                    plugin.id
                );
            }
            if !seen.insert(&plugin.id) {
                bail!("outputs contains duplicate id {}", plugin.id);
            }
            if plugin
                .command
                .first()
                .is_none_or(|value| value.trim().is_empty())
            {
                bail!(
                    "outputs.plugins entry {} must contain an executable in command",
                    plugin.id
                );
            }
        }
        Ok(())
    }

    pub fn ids(&self) -> Vec<&str> {
        self.adapters
            .iter()
            .map(|adapter| adapter.name())
            .chain(self.plugins.iter().map(|plugin| plugin.id.as_str()))
            .collect()
    }
}

fn valid_plugin_id(id: &str) -> bool {
    let mut characters = id.chars();
    matches!(characters.next(), Some('a'..='z'))
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

fn default_output_adapters() -> Vec<OutputAdapter> {
    vec![OutputAdapter::Glove80Usb]
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
                bail!(
                    "input.profiles contains duplicate profile {}",
                    profile.name()
                );
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
            outputs: OutputConfig::default(),
            focus: self.focus,
            display,
            actions: ActionsConfig::default(),
            herdr: HerdrConfig::default(),
            terminal: TerminalConfig::default(),
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
    config.outputs.validate()?;
    config.display.validate()?;
    config.herdr.validate()?;
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

# Herdr topology. By default Beckon uses the `herdr` program from PATH, the
# default session socket, and every live session (the default session plus
# named sessions under ~/.config/herdr/sessions). Pin the CLI for launchd,
# point at a nonstandard socket, or restrict management to named sessions.
# `unbound_label` is the `$beckon_binding` sidebar text for a pane without a
# key; set it to "" to show nothing for unbound panes.
# [herdr]
# cli = "herdr"
# socket = "/Users/you/.config/herdr/herdr.sock"
# sessions = "auto"  # or ["default", "agent-workspace"]
# unbound_label = "unbound"

# Optional terminal surface control. With a backend configured, Beckon can
# raise the window and tab displaying a bound session before it focuses the
# pane. Adoption is explicit: `beckon terminals` lists surfaces and
# `beckon adopt --session <name> --terminal <handle>` records one in the state
# directory. Defaults to "none", which keeps the focus command as the only
# surface mechanism.
# [terminal]
# backend = "ghostty"


# Select independent display outputs. The compatibility default is the
# optional Glove80 USB LED adapter. Use an empty list for navigation without
# any physical display; a disconnected optional adapter is not an error.
# [outputs]
# adapters = ["glove80-usb"]

# Add trusted, long-running display plugins as argv arrays. Beckon sends a
# versioned NDJSON stream to each plugin's stdin; it does not invoke a shell.
# [[outputs.plugins]]
# id = "status-log"
# command = ["/absolute/path/to/beckon-display-log"]

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
        assert_eq!(config.outputs.adapters, [OutputAdapter::Glove80Usb]);
        assert!(!config.actions.confirm.enabled);
        assert_eq!(config.actions.confirm.repeat_press_ms, 750);
        assert_eq!(config.actions.confirm.keys, ["enter"]);
    }

    #[test]
    fn allows_navigation_without_a_display_output() {
        let config = parse(
            r#"
config_version = 2
[outputs]
adapters = []
"#,
        )
        .unwrap();

        assert!(config.outputs.adapters.is_empty());
        assert!(config.outputs.plugins.is_empty());
    }

    #[test]
    fn rejects_duplicate_display_outputs() {
        let config = parse(
            r#"
config_version = 2
[outputs]
adapters = ["glove80-usb", "glove80-usb"]
"#,
        )
        .unwrap();
        let error = config.outputs.validate().unwrap_err();

        assert!(error.to_string().contains("duplicate adapter glove80-usb"));
    }

    #[test]
    fn parses_process_display_plugin() {
        let config = parse(
            r#"
config_version = 2
[outputs]
adapters = []

[[outputs.plugins]]
id = "status-log"
command = ["/usr/bin/tee", "/tmp/beckon.ndjson"]
"#,
        )
        .unwrap();

        config.outputs.validate().unwrap();
        assert_eq!(config.outputs.ids(), ["status-log"]);
        assert_eq!(
            config.outputs.plugins[0].command,
            ["/usr/bin/tee", "/tmp/beckon.ndjson"]
        );
    }

    #[test]
    fn rejects_invalid_or_colliding_process_display_plugins() {
        for (contents, expected) in [
            (
                r#"
config_version = 2
[[outputs.plugins]]
id = "Status Log"
command = ["logger"]
"#,
                "lowercase kebab-case",
            ),
            (
                r#"
config_version = 2
[[outputs.plugins]]
id = "glove80-usb"
command = ["logger"]
"#,
                "duplicate id glove80-usb",
            ),
            (
                r#"
config_version = 2
[outputs]
adapters = []
[[outputs.plugins]]
id = "status-log"
command = ["logger"]
[[outputs.plugins]]
id = "status-log"
command = ["logger"]
"#,
                "duplicate id status-log",
            ),
            (
                r#"
config_version = 2
[[outputs.plugins]]
id = "status-log"
command = []
"#,
                "must contain an executable",
            ),
        ] {
            let config = parse(contents).unwrap();
            let error = config.outputs.validate().unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
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

    #[test]
    fn herdr_defaults_manage_every_live_session() {
        let config = parse("config_version = 2").unwrap();
        config.herdr.validate().unwrap();

        assert_eq!(config.herdr.cli, "herdr");
        assert!(config.herdr.socket.is_none());
        assert!(config.herdr.allowed_sessions().is_none());
    }

    #[test]
    fn herdr_session_allowlist_is_explicit() {
        let config = parse(
            r#"
config_version = 2
[herdr]
sessions = ["default", "agent-workspace"]
"#,
        )
        .unwrap();
        config.herdr.validate().unwrap();

        assert_eq!(
            config.herdr.allowed_sessions(),
            Some(["default".to_string(), "agent-workspace".to_string()].as_slice())
        );
    }

    #[test]
    fn herdr_rejects_duplicate_or_empty_session_lists() {
        for (contents, expected) in [
            (
                r#"
config_version = 2
[herdr]
sessions = ["default", "default"]
"#,
                "duplicate session default",
            ),
            (
                r#"
config_version = 2
[herdr]
sessions = []
"#,
                "at least one session",
            ),
            (
                r#"
config_version = 2
[herdr]
sessions = ["  "]
"#,
                "empty session names",
            ),
        ] {
            let config = parse(contents).unwrap();
            let error = config.herdr.validate().unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[test]
    fn herdr_rejects_unknown_keys() {
        assert!(parse("config_version = 2\n[herdr]\nunknown = 1\n").is_err());
    }

    #[test]
    fn terminal_backend_defaults_to_none_and_accepts_registry_names() {
        let default = parse("config_version = 2").unwrap();
        assert_eq!(default.terminal.backend, TerminalBackendKind::None);

        let ghostty = parse(
            r#"
config_version = 2
[terminal]
backend = "ghostty"
"#,
        )
        .unwrap();
        assert_eq!(
            ghostty.terminal.backend,
            TerminalBackendKind::GhosttyAppleScript
        );
        assert_eq!(ghostty.terminal.backend.name(), "ghostty");
    }

    #[test]
    fn terminal_rejects_unknown_backends_and_keys() {
        assert!(
            parse("config_version = 2\n[terminal]\nbackend = \"tmux\"\n").is_err(),
            "unregistered backend names must fail validation"
        );
        assert!(parse("config_version = 2\n[terminal]\nsurfaces = []\n").is_err());
    }

    #[test]
    fn herdr_accepts_a_pinned_cli_and_socket() {
        let config = parse(
            r#"
config_version = 2
[herdr]
cli = "/nix/store/herdr/bin/herdr"
socket = "/tmp/custom-herdr.sock"
"#,
        )
        .unwrap();
        config.herdr.validate().unwrap();

        assert_eq!(config.herdr.cli, "/nix/store/herdr/bin/herdr");
        assert_eq!(
            config.herdr.socket.as_deref(),
            Some(std::path::Path::new("/tmp/custom-herdr.sock"))
        );
    }

    #[test]
    fn herdr_unbound_label_defaults_to_unbound() {
        let config = parse("config_version = 2\n").unwrap();
        assert_eq!(config.herdr.unbound_label, "unbound");
        let config = parse("config_version = 2\n[herdr]\ncli = \"herdr\"\n").unwrap();
        assert_eq!(config.herdr.unbound_label, "unbound");
    }

    #[test]
    fn herdr_unbound_label_accepts_custom_and_empty_values() {
        let config = parse("config_version = 2\n[herdr]\nunbound_label = \"-\"\n").unwrap();
        config.herdr.validate().unwrap();
        assert_eq!(config.herdr.unbound_label, "-");

        let config = parse("config_version = 2\n[herdr]\nunbound_label = \"\"\n").unwrap();
        config.herdr.validate().unwrap();
        assert_eq!(config.herdr.unbound_label, "");
    }

    #[test]
    fn default_template_documents_the_unbound_label() {
        assert!(DEFAULT_CONFIG.contains("# unbound_label = \"unbound\""));
        parse(DEFAULT_CONFIG).unwrap();
    }
}
