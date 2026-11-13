//! macOS event synthesis. No application names or device addresses live here.
use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Chord {
    code: u16,
    modifiers: u8,
}

impl Chord {
    pub(crate) fn shares_primary_key(&self, other: &Self) -> bool {
        self.code == other.code
    }
    /// Physical ANSI key names, not layout-dependent text. Exactly one ordinary
    /// key and zero or more explicit modifiers make a chord.
    pub fn parse(keys: &[String]) -> Result<Self> {
        let mut modifiers = 0;
        let mut code = None;
        for key in keys {
            let flag = match key.as_str() {
                "ctrl" => Some(1),
                "opt" => Some(2),
                "shift" => Some(4),
                "cmd" => Some(8),
                _ => None,
            };
            if let Some(flag) = flag {
                if code.is_some() {
                    bail!("put chord modifiers before the ordinary key");
                }
                if modifiers & flag != 0 {
                    bail!("duplicate modifier {key:?}");
                }
                modifiers |= flag;
            } else {
                if code.is_some() {
                    bail!("a chord must have exactly one non-modifier key");
                }
                code = Some(
                    keycode(key).ok_or_else(|| anyhow::anyhow!("unsupported chord key {key:?}"))?,
                );
            }
        }
        Ok(Self {
            code: code.ok_or_else(|| anyhow::anyhow!("a chord requires one non-modifier key"))?,
            modifiers,
        })
    }

    #[cfg(target_os = "macos")]
    pub fn post(&self, down: bool) -> Result<()> {
        use core_graphics::{
            event::{CGEvent, CGEventFlags, CGEventTapLocation},
            event_source::{CGEventSource, CGEventSourceStateID},
        };
        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn CGPreflightPostEventAccess() -> bool;
        }
        // Do not prompt from a LaunchAgent or silently pretend synthesis worked.
        if !unsafe { CGPreflightPostEventAccess() } {
            bail!(
                "event posting denied; grant Accessibility to the Beckon process in System Settings"
            );
        }
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| anyhow::anyhow!("create keyboard event source"))?;
        let event = CGEvent::new_keyboard_event(source, self.code, down)
            .map_err(|()| anyhow::anyhow!("create keyboard event"))?;
        let mut flags = CGEventFlags::empty();
        for (mask, flag) in [
            (1, CGEventFlags::CGEventFlagControl),
            (2, CGEventFlags::CGEventFlagAlternate),
            (4, CGEventFlags::CGEventFlagShift),
            (8, CGEventFlags::CGEventFlagCommand),
        ] {
            if self.modifiers & mask != 0 {
                flags |= flag;
            }
        }
        event.set_flags(flags);
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn post(&self, _down: bool) -> Result<()> {
        bail!("system chord synthesis is implemented only on macOS");
    }
}

fn keycode(key: &str) -> Option<u16> {
    // Apple's physical virtual-key codes. This is protocol data, not bindings.
    let codes = [
        ("a", 0),
        ("s", 1),
        ("d", 2),
        ("f", 3),
        ("h", 4),
        ("g", 5),
        ("z", 6),
        ("x", 7),
        ("c", 8),
        ("v", 9),
        ("b", 11),
        ("q", 12),
        ("w", 13),
        ("e", 14),
        ("r", 15),
        ("y", 16),
        ("t", 17),
        ("1", 18),
        ("2", 19),
        ("3", 20),
        ("4", 21),
        ("6", 22),
        ("5", 23),
        ("=", 24),
        ("9", 25),
        ("7", 26),
        ("-", 27),
        ("8", 28),
        ("0", 29),
        ("]", 30),
        ("o", 31),
        ("u", 32),
        ("[", 33),
        ("i", 34),
        ("p", 35),
        ("enter", 36),
        ("l", 37),
        ("j", 38),
        ("'", 39),
        ("k", 40),
        (";", 41),
        ("\\", 42),
        (",", 43),
        ("/", 44),
        ("n", 45),
        ("m", 46),
        (".", 47),
        ("tab", 48),
        ("space", 49),
        ("`", 50),
        ("backspace", 51),
        ("escape", 53),
        ("f1", 122),
        ("f2", 120),
        ("f3", 99),
        ("f4", 118),
        ("f5", 96),
        ("f6", 97),
        ("f7", 98),
        ("f8", 100),
        ("f9", 101),
        ("f10", 109),
        ("f11", 103),
        ("f12", 111),
        ("f13", 105),
        ("f14", 107),
        ("f15", 113),
        ("f16", 106),
        ("f17", 64),
        ("f18", 79),
        ("f19", 80),
        ("f20", 90),
        ("home", 115),
        ("pageup", 116),
        ("delete", 117),
        ("end", 119),
        ("pagedown", 121),
        ("left", 123),
        ("right", 124),
        ("down", 125),
        ("up", 126),
    ];
    codes
        .into_iter()
        .find(|(name, _)| *name == key)
        .map(|(_, code)| code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_before_any_events_are_posted() {
        let parse =
            |keys: &[&str]| Chord::parse(&keys.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(parse(&["ctrl", "opt", "d"]).is_ok());
        for keys in [
            vec![],
            vec!["ctrl"],
            vec!["d", "e"],
            vec!["ctrl", "ctrl", "d"],
            vec!["typo"],
        ] {
            assert!(parse(&keys).is_err());
        }
    }
}
