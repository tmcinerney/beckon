//! Device files contain their controls, actions and feedback together. References
//! are resolved after parsing; filename order has no semantic meaning.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::render::{Motion, Rgb};

/// Core only understands the device prefix. The adapter owns the address grammar.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Address {
    pub device: String,
    pub address: String,
}

impl TryFrom<String> for Address {
    type Error = String;
    fn try_from(text: String) -> std::result::Result<Self, Self::Error> {
        let (device, address) = text.split_once(':').ok_or("expected device:address")?;
        if !valid_id(device) || address.is_empty() || address.chars().any(char::is_whitespace) {
            return Err(format!("invalid device:address {text:?}"));
        }
        Ok(Self {
            device: device.into(),
            address: address.into(),
        })
    }
}

impl From<Address> for String {
    fn from(address: Address) -> Self {
        format!("{}:{}", address.device, address.address)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Hold,
    Toggle,
    TapHold,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Chord {
        keys: Vec<String>,
        mode: Mode,
        /// Required for tap-hold, forbidden for the other modes. No hidden threshold.
        tap_ms: Option<u64>,
    },
}

impl Action {
    pub fn validate(&self) -> Result<()> {
        let Self::Chord { keys, mode, tap_ms } = self;
        crate::chord::Chord::parse(keys)?;
        match (mode, tap_ms) {
            (Mode::TapHold, Some(ms)) if *ms > 0 => Ok(()),
            (Mode::TapHold, _) => bail!("tap-hold requires an explicit positive tap_ms"),
            (_, None) => Ok(()),
            _ => bail!("tap_ms is only valid for tap-hold"),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Treatment {
    pub color: Rgb,
    pub brightness: f32,
    pub motion: Motion,
}

impl Treatment {
    pub fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.brightness) {
            bail!("brightness must be between 0 and 1");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Effects {
    pub idle: Option<Treatment>,
    pub held: Option<Treatment>,
    pub latched: Option<Treatment>,
}

impl Effects {
    /// No treatment means return control to the normal binding/base rendering.
    pub fn for_press(&self, held: bool, latched: bool) -> Option<Treatment> {
        if held {
            self.held
        } else if latched {
            self.latched
        } else {
            self.idle
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Key {
    /// Explicit grouping. Sharing an action reference does NOT join controls.
    pub members: Vec<Address>,
    /// Optional device-independent binding id (declared in the root config).
    pub binding: Option<String>,
    pub action: Option<String>,
    pub effects: Option<String>,
}

/// Adapter-specific options are deserialized and checked by that adapter, never
/// inspected by the switch engine. This also supports third-party adapters.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeviceFile {
    pub adapter: String,
    #[serde(default)]
    pub options: toml::Table,
    #[serde(default)]
    pub keys: BTreeMap<String, Key>,
    #[serde(default)]
    pub actions: BTreeMap<String, Action>,
    #[serde(default)]
    pub effects: BTreeMap<String, Effects>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeviceConfig {
    pub id: String,
    pub path: PathBuf,
    pub definition: DeviceFile,
}

impl DeviceConfig {
    pub fn validate(&self, bindings: &[String]) -> Result<()> {
        let file = &self.definition;
        let mut claimed = BTreeMap::new();
        for (name, key) in &file.keys {
            if !valid_id(name) {
                bail!("keys.{name}: invalid name");
            }
            if key.members.is_empty() {
                bail!("keys.{name}: members must not be empty");
            }
            if key.binding.is_some() && key.action.is_some() {
                bail!("keys.{name}: choose a binding or an action, not both");
            }
            if let Some(binding) = &key.binding {
                require("binding", binding, bindings.iter().map(String::as_str))?;
            }
            if let Some(action) = &key.action {
                require("action", action, file.actions.keys().map(String::as_str))?;
            }
            if let Some(effects) = &key.effects {
                require("effects", effects, file.effects.keys().map(String::as_str))?;
            }
            for member in &key.members {
                if member.device != self.id {
                    bail!(
                        "keys.{name}: member belongs to {:?}, not {:?}",
                        member.device,
                        self.id
                    );
                }
                if let Some(previous) = claimed.insert(member, name) {
                    bail!(
                        "address {:?} is claimed by keys.{previous} and keys.{name}",
                        member.address
                    );
                }
            }
        }
        for (name, action) in &file.actions {
            if !valid_id(name) {
                bail!("actions.{name}: invalid name");
            }
            action
                .validate()
                .with_context(|| format!("actions.{name}"))?;
        }
        for (name, effects) in &file.effects {
            if !valid_id(name) {
                bail!("effects.{name}: invalid name");
            }
            let treatments = [effects.idle, effects.held, effects.latched];
            if treatments.iter().all(Option::is_none) {
                bail!("effects.{name}: define at least one state");
            }
            for treatment in treatments.into_iter().flatten() {
                treatment
                    .validate()
                    .with_context(|| format!("effects.{name}"))?;
            }
        }
        Ok(())
    }
}

pub fn load(dir: &Path, bindings: &[String]) -> Result<Vec<DeviceConfig>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Follow symlinks: declarative installations project files this way.
        if path.extension().and_then(|v| v.to_str()) == Some("toml") && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let id = path
                .file_stem()
                .and_then(|v| v.to_str())
                .context("invalid device filename")?
                .to_string();
            if !valid_id(&id) {
                bail!(
                    "{}: device filename must be lowercase kebab-case",
                    path.display()
                );
            }
            let text = fs::read_to_string(&path)?;
            let definition =
                toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
            let device = DeviceConfig {
                id,
                path,
                definition,
            };
            device
                .validate(bindings)
                .with_context(|| format!("validate {}", device.path.display()))?;
            Ok(device)
        })
        .collect()
}

pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn require<'a>(
    kind: &str,
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let candidates: Vec<_> = candidates.collect();
    if candidates.contains(&name) {
        return Ok(());
    }
    let nearest = candidates
        .iter()
        .min_by_key(|candidate| distance(name, candidate));
    let hint = nearest
        .filter(|candidate| distance(name, candidate) <= 2)
        .map(|candidate| format!("; did you mean {candidate:?}?"))
        .unwrap_or_default();
    bail!(
        "unknown {kind} {name:?}{hint}; available: {}",
        candidates.join(", ")
    )
}

fn distance(a: &str, b: &str) -> usize {
    let mut row: Vec<_> = (0..=b.len()).collect();
    for (i, x) in a.bytes().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, y) in b.bytes().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (previous + usize::from(x != y))
                .min(row[j] + 1)
                .min(old + 1);
            previous = old;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> Result<DeviceConfig> {
        let config = DeviceConfig {
            id: "pad".into(),
            path: "pad.toml".into(),
            definition: toml::from_str(text)?,
        };
        config.validate(&["key1".into()])?;
        Ok(config)
    }
    const ACTION: &str =
        "[actions.dictate]\nkind='chord'\nkeys=['ctrl','opt','d']\nmode='tap-hold'\ntap_ms=300\n";
    #[test]
    fn forward_references_and_explicit_grouping() {
        let text = format!(
            "adapter='test'\n[keys.key11]\nmembers=['pad:16','pad:17']\naction='dictate'\n{ACTION}"
        );
        assert_eq!(
            parse(&text).unwrap().definition.keys["key11"].members.len(),
            2
        );
    }
    #[test]
    fn table_order_is_irrelevant() {
        let key = "[keys.key11]\nmembers=['pad:16']\naction='dictate'\n";
        let a = parse(&format!("adapter='test'\n{key}{ACTION}")).unwrap();
        let b = parse(&format!("adapter='test'\n{ACTION}{key}")).unwrap();
        assert_eq!(a.definition, b.definition);
    }
    #[test]
    fn missing_references_suggest_typos() {
        let err = parse(&format!(
            "adapter='test'\n[keys.key11]\nmembers=['pad:16']\naction='dictat'\n{ACTION}"
        ))
        .unwrap_err();
        assert!(err.to_string().contains("did you mean"));
    }
    #[test]
    fn duplicate_members_and_unknown_fields_fail() {
        assert!(parse("adapter='test'\n[keys.key1]\nmembers=['pad:6','pad:6']").is_err());
        assert!(parse("adapter='test'\n[keys.key1]\nmembers=['pad:6']\nacton='x'").is_err());
    }
    #[test]
    fn no_implicit_timing_or_treatments() {
        assert!(
            parse("adapter='test'\n[actions.a]\nkind='chord'\nkeys=['d']\nmode='tap-hold'")
                .is_err()
        );
        assert!(parse("adapter='test'\n[effects.a]\nheld={color='#FF0000'}").is_err());
        assert!(
            parse("adapter='test'\n[actions.a]\nkind='chord'\nkeys=['not-a-key']\nmode='hold'")
                .is_err()
        );
    }
    #[test]
    fn shared_actions_do_not_imply_grouping() {
        let file = parse(&format!("adapter='test'\n[keys.a]\nmembers=['pad:16']\naction='dictate'\n[keys.b]\nmembers=['pad:17']\naction='dictate'\n{ACTION}")).unwrap();
        assert_eq!(file.definition.keys.len(), 2);
    }
}
