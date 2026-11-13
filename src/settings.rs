//! Typed, presentation-only overrides. No device wiring or binding identities
//! can be injected through this document or its mutation protocol.
use crate::{
    config::Config,
    device::{self, Effects, Treatment},
    render::{AgentState, PresentationSettings, StateTreatment},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DevicePresentation {
    #[serde(default)]
    pub channels: BTreeMap<String, Treatment>,
    #[serde(default)]
    pub effects: BTreeMap<String, Effects>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub display: PresentationSettings,
    #[serde(default)]
    pub devices: BTreeMap<String, DevicePresentation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PresentationChange {
    ThemeState {
        theme: String,
        state: AgentState,
        treatment: StateTreatment,
    },
    DeviceEffect {
        device: String,
        effect: String,
        effects: Effects,
    },
    DeviceChannel {
        device: String,
        channel: String,
        treatment: Treatment,
    },
}

impl Settings {
    pub fn read(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        toml::from_str(&fs::read_to_string(path)?)
            .with_context(|| format!("parse {}", path.display()))
    }

    pub fn apply(&self, config: &mut Config) -> Result<()> {
        if let Some(theme) = &self.display.theme {
            config.display.theme = theme.clone();
        }
        for (name, theme) in &self.display.themes {
            for treatment in theme.ordered() {
                check_treatment(treatment)?;
            }
            config.display.themes.insert(name.clone(), theme.clone());
        }
        for (id, settings) in &self.devices {
            let device = config
                .devices
                .iter_mut()
                .find(|device| &device.id == id)
                .with_context(|| format!("settings references unknown device {id:?}"))?;
            for (name, effects) in &settings.effects {
                device::require(
                    "effects",
                    name,
                    device.definition.effects.keys().map(String::as_str),
                )?;
                device
                    .definition
                    .effects
                    .insert(name.clone(), effects.clone());
            }
            if !settings.channels.is_empty() {
                crate::display::apply_channels(device, &settings.channels)?;
            }
        }
        Ok(())
    }

    pub fn change(&mut self, config: &Config, change: PresentationChange) -> Result<()> {
        match change {
            PresentationChange::ThemeState {
                theme,
                state,
                treatment,
            } => {
                check_treatment(treatment)?;
                let mut values = config
                    .display
                    .themes
                    .get(&theme)
                    .with_context(|| format!("unknown theme {theme:?}"))?
                    .clone();
                *match state {
                    AgentState::Idle => &mut values.idle,
                    AgentState::Working => &mut values.working,
                    AgentState::Blocked => &mut values.blocked,
                    AgentState::Done => &mut values.done,
                    AgentState::Unknown => &mut values.unknown,
                } = treatment;
                self.display.themes.insert(theme, values);
            }
            PresentationChange::DeviceEffect {
                device,
                effect,
                effects,
            } => {
                let definition = config
                    .devices
                    .iter()
                    .find(|value| value.id == device)
                    .context("unknown device")?;
                device::require(
                    "effects",
                    &effect,
                    definition.definition.effects.keys().map(String::as_str),
                )?;
                self.devices
                    .entry(device)
                    .or_default()
                    .effects
                    .insert(effect, effects);
            }
            PresentationChange::DeviceChannel {
                device,
                channel,
                treatment,
            } => {
                treatment.validate()?;
                self.devices
                    .entry(device)
                    .or_default()
                    .channels
                    .insert(channel, treatment);
            }
        }
        Ok(())
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| -> Result<()> {
            file.write_all(toml::to_string(self)?.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn check_treatment(t: StateTreatment) -> Result<()> {
    Treatment {
        color: t.color,
        brightness: t.brightness,
        motion: t.motion,
    }
    .validate()
}
