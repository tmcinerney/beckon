//! Physical input adapters.
//!
//! Beckon's navigation core deals only in logical keys (`f1` through `f10`).
//! An adapter owns the platform- and keyboard-specific translation from a
//! physical shortcut to one of those logical keys.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};

/// One physical shortcut exposed by an input adapter.
#[derive(Clone, Copy, Debug)]
pub struct InputBinding {
    /// Beckon's hardware-independent binding identifier.
    pub key: &'static str,
    /// A human-readable representation of the physical input for diagnostics.
    pub description: &'static str,
    modifiers: Option<Modifiers>,
    code: Code,
}

impl InputBinding {
    pub const fn new(
        key: &'static str,
        description: &'static str,
        modifiers: Option<Modifiers>,
        code: Code,
    ) -> Self {
        Self {
            key,
            description,
            modifiers,
            code,
        }
    }

    fn hotkey(self) -> HotKey {
        HotKey::new(self.modifiers, self.code)
    }
}

/// The registration surface required by a global-hotkey input adapter.
///
/// Keeping this small makes the physical mapping testable without starting
/// Tao's macOS event loop or registering machine-global shortcuts.
pub trait HotkeyRegistrar {
    fn register(&self, hotkey: HotKey) -> Result<()>;
}

impl HotkeyRegistrar for GlobalHotKeyManager {
    fn register(&self, hotkey: HotKey) -> Result<()> {
        GlobalHotKeyManager::register(self, hotkey).map_err(Into::into)
    }
}

/// A source of physical global shortcuts.
///
/// Implementations map physical shortcuts to Beckon's logical key IDs, while
/// the caller remains responsible for navigation and confirmation behavior.
pub trait InputAdapter {
    fn bindings(&self) -> &'static [InputBinding];

    fn register<R: HotkeyRegistrar>(&self, registrar: &R) -> Result<RegisteredInput> {
        let mut bindings = BTreeMap::new();
        for binding in self.bindings() {
            let hotkey = binding.hotkey();
            registrar.register(hotkey).with_context(|| {
                format!(
                    "register {}; another application may already own it",
                    binding.key
                )
            })?;
            bindings.insert(hotkey.id(), *binding);
        }
        Ok(RegisteredInput { bindings })
    }
}

/// A registered input adapter, able to translate system events to logical keys.
#[derive(Debug)]
pub struct RegisteredInput {
    bindings: BTreeMap<u32, InputBinding>,
}

impl RegisteredInput {
    /// Returns a logical key only for a press event owned by this adapter.
    pub fn pressed(&self, event: &GlobalHotKeyEvent) -> Option<InputBinding> {
        (event.state == HotKeyState::Pressed)
            .then(|| self.bindings.get(&event.id).copied())
            .flatten()
    }

    #[cfg(test)]
    fn binding_for_id(&self, id: u32) -> Option<InputBinding> {
        self.bindings.get(&id).copied()
    }
}

/// The existing Glove80 Beckon-layer transport.
///
/// F1 through F5 emit F16 through F20; F6 through F10 emit their Shift
/// variants. This preserves the deployed firmware and its intentionally
/// uncommon shortcuts while keeping that device detail outside the daemon.
#[derive(Debug, Default)]
pub struct Glove80HotkeyInput;

const GLOVE80_BINDINGS: [InputBinding; 10] = [
    InputBinding::new("f1", "F16", None, Code::F16),
    InputBinding::new("f2", "F17", None, Code::F17),
    InputBinding::new("f3", "F18", None, Code::F18),
    InputBinding::new("f4", "F19", None, Code::F19),
    InputBinding::new("f5", "F20", None, Code::F20),
    InputBinding::new("f6", "Shift+F16", Some(Modifiers::SHIFT), Code::F16),
    InputBinding::new("f7", "Shift+F17", Some(Modifiers::SHIFT), Code::F17),
    InputBinding::new("f8", "Shift+F18", Some(Modifiers::SHIFT), Code::F18),
    InputBinding::new("f9", "Shift+F19", Some(Modifiers::SHIFT), Code::F19),
    InputBinding::new("f10", "Shift+F20", Some(Modifiers::SHIFT), Code::F20),
];

impl InputAdapter for Glove80HotkeyInput {
    fn bindings(&self) -> &'static [InputBinding] {
        &GLOVE80_BINDINGS
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct FakeRegistrar {
        registered: RefCell<Vec<HotKey>>,
    }

    impl HotkeyRegistrar for FakeRegistrar {
        fn register(&self, hotkey: HotKey) -> Result<()> {
            self.registered.borrow_mut().push(hotkey);
            Ok(())
        }
    }

    #[test]
    fn glove80_adapter_registers_the_deployed_ten_shortcuts() {
        let registrar = FakeRegistrar::default();
        let input = Glove80HotkeyInput.register(&registrar).unwrap();
        let registered = registrar.registered.borrow();

        assert_eq!(registered.len(), 10);
        assert_eq!(input.binding_for_id(registered[0].id()).unwrap().key, "f1");
        assert_eq!(
            input
                .binding_for_id(registered[5].id())
                .unwrap()
                .description,
            "Shift+F16"
        );
    }

    #[test]
    fn glove80_adapter_maps_each_shortcut_to_a_distinct_logical_key() {
        let registrar = FakeRegistrar::default();
        let input = Glove80HotkeyInput.register(&registrar).unwrap();
        let registered = registrar.registered.borrow();

        let keys = registered
            .iter()
            .map(|hotkey| input.binding_for_id(hotkey.id()).unwrap().key)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec!["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10"]
        );
    }

    #[test]
    fn adapter_ignores_releases_and_unowned_shortcuts() {
        let registrar = FakeRegistrar::default();
        let input = Glove80HotkeyInput.register(&registrar).unwrap();
        let id = registrar.registered.borrow()[0].id();

        assert_eq!(
            input
                .pressed(&GlobalHotKeyEvent {
                    id,
                    state: HotKeyState::Pressed,
                })
                .unwrap()
                .key,
            "f1"
        );
        assert!(
            input
                .pressed(&GlobalHotKeyEvent {
                    id,
                    state: HotKeyState::Released,
                })
                .is_none()
        );
        assert!(
            input
                .pressed(&GlobalHotKeyEvent {
                    id: u32::MAX,
                    state: HotKeyState::Pressed,
                })
                .is_none()
        );
    }
}
