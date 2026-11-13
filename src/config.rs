//! Version 3: explicit global bindings/sources and self-contained device files.
use crate::{
    device::{self, DeviceConfig},
    render::{DisplayConfig, KeyDisplay, Rgb},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

const CONFIG_VERSION: u32 = 3;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    config_version: u32,
    pub bindings: Vec<String>,
    pub focus: FocusConfig,
    pub display: DisplayConfig,
    pub actions: ActionsConfig,
    pub herdr: HerdrConfig,
    pub terminal: TerminalConfig,
    pub sources: Vec<AgentSourceConfig>,
    pub outputs: OutputConfig,
    #[serde(skip)]
    pub devices: Vec<DeviceConfig>,
    #[serde(skip)]
    root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentSourceConfig {
    pub id: String,
    pub list: Vec<String>,
    pub focus: Vec<String>,
    pub activate: Vec<String>,
    pub identity: Option<SourceIdentity>,
    pub refresh_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub scope_env: String,
    pub id_env: String,
}

impl AgentSourceConfig {
    pub fn validate(&self) -> Result<()> {
        if !device::valid_id(&self.id)
            || self.list.first().is_none_or(|arg| arg.is_empty())
            || self.refresh_ms == 0
        {
            bail!(
                "sources.{} requires a valid id, list command and positive refresh_ms",
                self.id
            );
        }
        if let Some(identity) = &self.identity
            && (identity.scope_env.is_empty() || identity.id_env.is_empty())
        {
            bail!("sources.{}: identity env names must not be empty", self.id);
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalConfig {
    pub backend: TerminalBackendKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalBackendKind {
    None,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HerdrConfig {
    pub enabled: bool,
    pub cli: String,
    pub socket: Option<PathBuf>,
    pub sessions: SessionsConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum SessionsConfig {
    Auto(AutoSessions),
    Named(Vec<String>),
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoSessions {
    Auto,
}

impl HerdrConfig {
    pub fn allowed_sessions(&self) -> Option<&[String]> {
        match &self.sessions {
            SessionsConfig::Auto(_) => None,
            SessionsConfig::Named(names) => Some(names),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.enabled && self.cli.is_empty() {
            bail!("herdr.cli must name an executable");
        }
        if let SessionsConfig::Named(names) = &self.sessions {
            let unique: BTreeSet<_> = names.iter().collect();
            if names.is_empty() || unique.len() != names.len() || names.iter().any(String::is_empty)
            {
                bail!("herdr.sessions must contain unique non-empty names");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessOutputPlugin {
    pub id: String,
    pub command: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub plugins: Vec<ProcessOutputPlugin>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionsConfig {
    pub confirm: ConfirmConfig,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmConfig {
    pub enabled: bool,
    pub repeat_press_ms: u64,
    pub keys: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusConfig {
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
            "{} already exists; refusing to overwrite it",
            path.display()
        );
    }
    fs::create_dir_all(path.parent().context("configuration directory")?)?;
    fs::write(&path, include_str!("../templates/config.toml"))?;
    fs::create_dir_all(path.parent().unwrap().join("config.d"))?;
    println!(
        "created {}; no devices enabled (copy a template into config.d/)",
        path.display()
    );
    Ok(())
}

pub fn load() -> Result<Config> {
    load_at(&path())
}

pub fn load_at(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    // Diagnose old configs before serde reports all newly required fields.
    let document: toml::Value = toml::from_str(&text)?;
    if document
        .get("config_version")
        .and_then(toml::Value::as_integer)
        != Some(CONFIG_VERSION as i64)
    {
        bail!(
            "{}: expected config_version = {CONFIG_VERSION}; use the v3 templates (no legacy reader)",
            path.display()
        );
    }
    let mut config: Config =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    config.root = path.into();
    for argv in config
        .sources
        .iter_mut()
        .flat_map(|source| [&mut source.list, &mut source.focus, &mut source.activate])
        .chain(config.focus.command.iter_mut())
        .chain(
            config
                .outputs
                .plugins
                .iter_mut()
                .map(|plugin| &mut plugin.command),
        )
    {
        for argument in argv {
            *argument = expand_home(argument)?;
        }
    }
    config.herdr.cli = expand_home(&config.herdr.cli)?;
    if let Some(socket) = &mut config.herdr.socket {
        *socket = expand_home(&socket.to_string_lossy())?.into();
    }
    let directory = path.parent().context("configuration directory")?;
    config.devices = device::load(&directory.join("config.d"), &config.bindings)?;
    config.display.keys = config
        .bindings
        .iter()
        .map(|id| KeyDisplay {
            id: id.clone(),
            color: Rgb {
                red: 0,
                green: 0,
                blue: 0,
            },
        })
        .collect();
    // Mutable presentation only. It cannot inject actions, bindings or devices.
    let settings = directory.join("settings.toml");
    if settings.exists() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Settings {
            display: crate::render::PresentationSettings,
        }
        let settings: Settings = toml::from_str(&fs::read_to_string(&settings)?)?;
        if let Some(theme) = settings.display.theme {
            config.display.theme = theme;
        }
        config.display.themes.extend(settings.display.themes);
    }
    config
        .validate()
        .with_context(|| format!("validate {}", path.display()))?;
    Ok(config)
}

fn expand_home(text: &str) -> Result<String> {
    if !(text.starts_with("~/")
        || text.contains("${HOME}")
        || text == "$HOME"
        || text.starts_with("$HOME/"))
    {
        return Ok(text.into());
    }
    let home = env::var("HOME").context("HOME is required by a configured path")?;
    let text = text
        .strip_prefix("~/")
        .map(|suffix| format!("{home}/{suffix}"))
        .unwrap_or_else(|| text.into());
    Ok(text
        .replace("${HOME}", &home)
        .strip_prefix("$HOME")
        .map(|suffix| format!("{home}{suffix}"))
        .unwrap_or_else(|| text.replace("${HOME}", &home)))
}

impl Config {
    /// Only the single writer changes machine-owned presentation settings.
    /// Structural config is never rewritten by a UI request.
    pub fn set_theme(&mut self, name: &str) -> Result<()> {
        device::require(
            "theme",
            name,
            self.display.themes.keys().map(String::as_str),
        )?;
        let path = self
            .root
            .parent()
            .context("configuration directory")?
            .join("settings.toml");
        let mut settings: toml::Value = if path.exists() {
            toml::from_str(&fs::read_to_string(&path)?)?
        } else {
            toml::Value::Table(toml::Table::new())
        };
        let table = settings
            .as_table_mut()
            .context("settings must be a table")?;
        let display = table
            .entry("display")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .context("settings.display must be a table")?;
        display.insert("theme".into(), toml::Value::String(name.into()));
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| -> Result<()> {
            file.write_all(toml::to_string(&settings)?.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        self.display.theme = name.into();
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.config_version != CONFIG_VERSION {
            bail!("unsupported configuration version");
        }
        let mut names = BTreeSet::new();
        for key in &self.bindings {
            if !device::valid_id(key) || !names.insert(key) {
                bail!("bindings must have unique valid names; found {key:?}");
            }
        }
        self.display.validate()?;
        self.herdr.validate()?;
        if self
            .focus
            .command
            .as_ref()
            .is_some_and(|args| args.first().is_none_or(|arg| arg.is_empty()))
        {
            bail!("focus.command requires an executable");
        }
        if self.actions.confirm.repeat_press_ms == 0 || self.actions.confirm.keys.is_empty() {
            bail!("confirm requires positive repeat_press_ms and keys");
        }
        let mut sources = BTreeSet::new();
        if self.herdr.enabled {
            sources.insert("herdr");
        }
        for source in &self.sources {
            source.validate()?;
            if source.id == "herdr" {
                bail!(
                    "source id 'herdr' is reserved for the built-in integration; use [herdr] to enable it"
                );
            }
            if !sources.insert(&source.id) {
                bail!("duplicate source {:?}", source.id);
            }
        }
        let mut outputs = BTreeSet::new();
        let mut hardware = BTreeSet::new();
        for device in &self.devices {
            device
                .validate(&self.bindings)
                .with_context(|| format!("{}", device.path.display()))?;
            device::require(
                "adapter",
                &device.definition.adapter,
                ["creator-micro", "hotkeys", "glove80"].into_iter(),
            )?;
            if device.definition.adapter != "hotkeys"
                && !hardware.insert(&device.definition.adapter)
            {
                bail!(
                    "multiple {:?} instances need hardware selectors, which are not implemented; refusing two writers for one endpoint",
                    device.definition.adapter
                );
            }
            match device.definition.adapter.as_str() {
                "creator-micro" => crate::creator_micro::validate_device(device, &sources)
                    .with_context(|| format!("{}", device.path.display()))?,
                "hotkeys" | "glove80" => crate::input::validate_device(device)
                    .with_context(|| format!("{}", device.path.display()))?,
                other => bail!(
                    "{}: unknown adapter {other:?}; available: creator-micro, hotkeys, glove80",
                    device.path.display()
                ),
            }
            outputs.insert(device.id.as_str());
        }
        for plugin in &self.outputs.plugins {
            if !device::valid_id(&plugin.id)
                || !outputs.insert(&plugin.id)
                || plugin.command.first().is_none_or(|v| v.is_empty())
            {
                bail!("invalid/duplicate output plugin {:?}", plugin.id);
            }
        }
        crate::input::ConfiguredInput::plan(&self.devices)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn template_has_no_implicit_devices_or_sources() {
        let config: Config = toml::from_str(include_str!("../templates/config.toml")).unwrap();
        config.validate().unwrap();
        assert!(config.devices.is_empty() && config.sources.is_empty() && !config.herdr.enabled);
    }
    #[test]
    fn unknown_fields_and_versions_fail() {
        let text = include_str!("../templates/config.toml");
        assert!(toml::from_str::<Config>(&format!("typo=true\n{text}")).is_err());
        let mut config: Config = toml::from_str(text).unwrap();
        config.config_version = 2;
        assert!(config.validate().is_err());
    }
}
