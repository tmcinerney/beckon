use std::{collections::BTreeMap, fmt, str::FromStr};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::core::{Binding, KEY_IDS, Pane};

/// A host-independent LED instruction. Firmware transports this later; it is
/// deliberately not coupled to HID reports or keyboard-specific coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl fmt::Display for Rgb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "#{:02X}{:02X}{:02X}",
            self.red, self.green, self.blue
        )
    }
}

impl Serialize for Rgb {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl FromStr for Rgb {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let hex = value
            .strip_prefix('#')
            .ok_or_else(|| "colour must use #RRGGBB syntax".to_string())?;
        if hex.len() != 6 {
            return Err("colour must use #RRGGBB syntax".to_string());
        }
        let byte = |range| {
            u8::from_str_radix(&hex[range], 16)
                .map_err(|_| "colour must use #RRGGBB syntax".to_string())
        };
        Ok(Self {
            red: byte(0..2)?,
            green: byte(2..4)?,
            blue: byte(4..6)?,
        })
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    Steady,
    Breathe,
    Pulse,
    Flicker,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentState {
    pub fn from_herdr(status: &str) -> Self {
        match status.trim().to_ascii_lowercase().as_str() {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyDisplay {
    pub id: String,
    pub colour: Rgb,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateTreatment {
    pub brightness: f32,
    pub motion: Motion,
    #[serde(default)]
    pub colour: Option<Rgb>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateTreatments {
    pub idle: StateTreatment,
    pub working: StateTreatment,
    pub blocked: StateTreatment,
    pub done: StateTreatment,
    pub unknown: StateTreatment,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    #[serde(default = "default_keys")]
    pub keys: Vec<KeyDisplay>,
    #[serde(default = "default_states")]
    pub states: StateTreatments,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            keys: default_keys(),
            states: default_states(),
        }
    }
}

impl DisplayConfig {
    pub fn validate(&self) -> Result<()> {
        if self.keys.len() != KEY_IDS.len() {
            bail!("display.keys must configure exactly ten Beckon keys");
        }
        let configured = self
            .keys
            .iter()
            .map(|key| key.id.as_str())
            .collect::<Vec<_>>();
        for key in KEY_IDS {
            if configured
                .iter()
                .filter(|configured_key| **configured_key == key)
                .count()
                != 1
            {
                bail!("display.keys must configure {key} exactly once");
            }
        }
        for (name, treatment) in self.state_treatments() {
            if !(0.0..=1.0).contains(&treatment.brightness) {
                bail!("display.states.{name}.brightness must be between 0.0 and 1.0");
            }
        }
        Ok(())
    }

    fn state_treatments(&self) -> [(&str, &StateTreatment); 5] {
        [
            ("idle", &self.states.idle),
            ("working", &self.states.working),
            ("blocked", &self.states.blocked),
            ("done", &self.states.done),
            ("unknown", &self.states.unknown),
        ]
    }

    fn treatment(&self, state: AgentState) -> &StateTreatment {
        match state {
            AgentState::Idle => &self.states.idle,
            AgentState::Working => &self.states.working,
            AgentState::Blocked => &self.states.blocked,
            AgentState::Done => &self.states.done,
            AgentState::Unknown => &self.states.unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KeyRender {
    pub key: String,
    pub state: Option<AgentState>,
    pub colour: Rgb,
    pub brightness: f32,
    pub motion: Motion,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RenderPlan {
    pub keys: Vec<KeyRender>,
}

pub fn render(display: &DisplayConfig, bindings: &[(Binding, Pane)]) -> Result<RenderPlan> {
    display.validate()?;
    let panes_by_key = bindings
        .iter()
        .map(|(binding, pane)| (binding.key.as_str(), pane))
        .collect::<BTreeMap<_, _>>();

    let keys = display
        .keys
        .iter()
        .map(|key| match panes_by_key.get(key.id.as_str()) {
            Some(pane) => render_state(
                &key.id,
                key.colour,
                AgentState::from_herdr(&pane.agent_status),
                display,
            ),
            None => KeyRender {
                key: key.id.clone(),
                state: None,
                colour: key.colour,
                brightness: 0.0,
                motion: Motion::Steady,
            },
        })
        .collect();
    Ok(RenderPlan { keys })
}

pub fn all_state_examples(display: &DisplayConfig) -> Result<RenderPlan> {
    display.validate()?;
    let states = [
        AgentState::Idle,
        AgentState::Working,
        AgentState::Blocked,
        AgentState::Done,
        AgentState::Unknown,
    ];
    Ok(RenderPlan {
        keys: states
            .into_iter()
            .zip(display.keys.iter())
            .map(|(state, key)| render_state(&key.id, key.colour, state, display))
            .collect(),
    })
}

fn render_state(
    key: &str,
    key_colour: Rgb,
    state: AgentState,
    display: &DisplayConfig,
) -> KeyRender {
    let treatment = display.treatment(state);
    KeyRender {
        key: key.to_string(),
        state: Some(state),
        colour: treatment.colour.unwrap_or(key_colour),
        brightness: treatment.brightness,
        motion: treatment.motion,
    }
}

fn default_keys() -> Vec<KeyDisplay> {
    [
        ("f1", "#3BA0FF"),
        ("f2", "#00C48C"),
        ("f3", "#B47CFF"),
        ("f4", "#FFB020"),
        ("f5", "#35D0E8"),
        ("f6", "#6172FF"),
        ("f7", "#22C55E"),
        ("f8", "#F59E0B"),
        ("f9", "#E879F9"),
        ("f10", "#14B8A6"),
    ]
    .into_iter()
    .map(|(id, colour)| KeyDisplay {
        id: id.into(),
        colour: colour.parse().expect("valid built-in colour"),
    })
    .collect()
}

fn default_states() -> StateTreatments {
    StateTreatments {
        idle: StateTreatment {
            brightness: 0.2,
            motion: Motion::Steady,
            colour: None,
        },
        working: StateTreatment {
            brightness: 0.6,
            motion: Motion::Breathe,
            colour: None,
        },
        blocked: StateTreatment {
            brightness: 0.8,
            motion: Motion::Pulse,
            colour: Some("#FF2B2B".parse().expect("valid built-in colour")),
        },
        done: StateTreatment {
            brightness: 1.0,
            motion: Motion::Steady,
            colour: None,
        },
        unknown: StateTreatment {
            brightness: 0.1,
            motion: Motion::Flicker,
            colour: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(status: &str) -> Pane {
        Pane {
            pane_id: "p1".into(),
            agent_status: status.into(),
            agent: None,
            label: None,
            cwd: None,
            tokens: BTreeMap::new(),
        }
    }

    #[test]
    fn blocked_overrides_the_key_colour() {
        let plan = render(
            &DisplayConfig::default(),
            &[(
                Binding {
                    key: "f1".into(),
                    pane_id: "p1".into(),
                },
                pane("blocked"),
            )],
        )
        .unwrap();
        assert_eq!(plan.keys[0].colour.to_string(), "#FF2B2B");
        assert_eq!(plan.keys[0].motion, Motion::Pulse);
    }

    #[test]
    fn an_unknown_herdr_status_is_visible_as_unknown() {
        let plan = render(
            &DisplayConfig::default(),
            &[(
                Binding {
                    key: "f1".into(),
                    pane_id: "p1".into(),
                },
                pane("waiting_for_user"),
            )],
        )
        .unwrap();
        assert_eq!(plan.keys[0].state, Some(AgentState::Unknown));
        assert_eq!(plan.keys[0].motion, Motion::Flicker);
    }

    #[test]
    fn unbound_keys_are_explicitly_off() {
        let plan = render(&DisplayConfig::default(), &[]).unwrap();
        assert_eq!(plan.keys.len(), 10);
        assert_eq!(plan.keys[0].state, None);
        assert_eq!(plan.keys[0].brightness, 0.0);
    }

    #[test]
    fn rejects_incomplete_key_configuration() {
        let mut display = DisplayConfig::default();
        display.keys.pop();
        assert!(display.validate().is_err());
    }
}
