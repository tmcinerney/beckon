# Adapter architecture: logical Beckon keys

## Decision

Treat `f1` through `f10` as **logical Beckon slots**, not physical keyboard
keys. The binding ledger, Herdr integration, navigation policy, repeat-press
confirmation, render planning, and sidebar metadata operate only on those
slots. Keyboard-specific code is an adapter at either edge:

```text
Herdr panes ──> binding/navigation core ──> RenderPlan ──> display sinks
                         ^                         \
                         │                          └─ Glove80 USB HID
                   input sources
                         ^
             Glove80 hotkeys / MacBook F1–F10
```

The first new adapter is a MacBook input-only profile. It maps standard
`F1`–`F10` global hotkeys to the same ten logical slots. It has no display
sink. The existing Glove80 profile continues to map `F16`–`F20` and
`Shift+F16`–`Shift+F20`; the existing Glove80 HID transport remains its
optional display sink.

This is deliberately built-in adapter configuration, not a third-party Rust
plugin ABI. A stable ABI would freeze error handling, lifecycle, and async/main
thread behaviour before Beckon has a second independent hardware backend.

## Current seams and coupling

The binding core is already mostly hardware-neutral:

- `core::BindingService` owns `f1`–`f10`, durable bindings, reconciliation,
  pane close cleanup, and only depends on `BindingStore` and `PaneDirectory`.
- `render::render` produces a `RenderPlan` with logical keys and resolved
  colors, brightness, and motion. It has no HID coordinates.
- `hid::RenderSink<W>` already isolates Glove80 report writing behind
  `StatusWriter` and de-duplicates reconnect-safe snapshots.

The remaining coupling is in `main.rs`:

- `beckon_hotkeys`, `register_hotkeys`, and `hotkey_description` hard-code the
  Glove80 F16 profile.
- The daemon constructs exactly one `hid::RenderSink<UsbStatusWriter>` and
  calls it directly from `publish_display`.
- `Config` has a `display` section but no input-source or display-sink
  selection.
- HID diagnostics and Glove80 vendor constants share the public `hid` module
  with transport-neutral status semantics.

## Minimal module shape

Keep the existing crate for this increment. A workspace split is unnecessary
until a second independently packaged backend exists.

```text
src/
  core.rs                 # BindingService, PaneDirectory, KeyId
  navigation.rs           # activation/focus/repeat-press policy
  render.rs               # RenderPlan and themes
  adapters/
    input.rs              # InputProfile -> logical hotkey registrations
    macos_hotkeys.rs      # one manager/router, main-thread event integration
    display.rs            # DisplaySink fan-out boundary
    glove80.rs            # Glove80 profile + USB HID display adapter
  hid.rs                  # protocol implementation, private to glove80 adapter
```

`KeyId` should become a validated logical newtype or enum while continuing to
serialize as the existing lowercase strings. That keeps CLI, state-file, and
Herdr-token compatibility (`fkey=f3`) while preventing adapters from inventing
unknown slots.

Suggested boundaries:

```rust
pub trait DisplaySink {
    fn id(&self) -> &'static str;
    fn publish(&mut self, plan: &RenderPlan) -> anyhow::Result<bool>;
}

pub struct InputRegistration {
    pub source_id: String,
    pub hotkey: HotKeySpec,
    pub key: KeyId,
}

pub trait InputProfile {
    fn registrations(&self) -> anyhow::Result<Vec<InputRegistration>>;
}
```

`MacosHotkeyRouter`, not each profile, owns `GlobalHotKeyManager`, the global
event receiver, and Tao's main-thread event loop. It converts received event
IDs into `KeyId` activation requests. This is important because
`global-hotkey` exposes a process-wide receiver and the existing daemon already
requires the Tao event loop on macOS's main thread. Profiles are therefore
declarative registration providers, not independently polling event sources.

`NavigationService` receives a `KeyId` and owns the existing focus and optional
repeat-press-confirm flow. It depends on `PaneDirectory`, `FocusAdapter`, and
the action configuration; input adapters never call Herdr or send agent input.

`DisplaySet` fans a `RenderPlan` to enabled sinks. A failed Glove80 sink is
logged/deduplicated independently and must not disable bindings, navigation,
or other display sinks. An empty set is valid for a MacBook-only setup.

## Configuration migration

The initial implementation keeps config version 2 and adds a plural
`input.profiles` setting. An absent `[input]` section or the legacy singular
`input.profile` setting keeps its existing behavior. The plural setting lets a
desktop Glove80 and mobile MacBook remain active simultaneously.

```toml
# Default: exact current Glove80 navigation behaviour.
[input]
profiles = ["glove80"]

# Or opt in to both sources. macOS must deliver standard F keys (Function
# Keys setting, or Fn). Both sources map one-to-one to the same logical slots.
profiles = ["glove80", "macbook-function-keys"]
```

Profiles initially remain fixed, auditable mappings:

| Profile | Logical slots | macOS registrations |
| --- | --- | --- |
| `glove80` | F1–F10 | F16–F20, Shift+F16–Shift+F20 |
| `macbook-function-keys` | F1–F10 | F1–F10 |

Do not expose arbitrary key strings in this first migration. Add per-key
overrides only after the registration model and collision errors have been used
in practice. Unknown profile values, duplicate profile names, and mixing the
legacy `profile` field with `profiles` are configuration errors.

The Glove80 display is separately optional. Its input profile can be enabled
without a connected display, and `macbook-function-keys` may be enabled
alongside it. The existing `beckon hid` diagnostic commands remain explicitly
Glove80-specific.

## Migration sequence

1. Introduce `KeyId` and convert core/render/HID slot conversion without
   changing serialized values, CLI arguments, or the firmware protocol.
2. Extract the current focus/repeat-press logic into `NavigationService` and
   cover its `activate(KeyId)` result paths with unit tests.
3. Extract the current fixed Glove80 hotkey table into an `InputProfile`; add a
   central macOS router that preserves the current event-loop ownership.
4. Add a backwards-compatible plural input profile setting. Keep an absent
   input setting and the singular legacy setting equivalent to one Glove80
   adapter.
5. Add the MacBook F1–F10 profile. Preflight all registrations, then register
   them as one set. Keep the current Glove80 profile byte-for-byte equivalent.
6. Wrap the existing `hid::RenderSink<UsbStatusWriter>` in `DisplaySink` and
   make daemon rendering fan out to the enabled `DisplaySet`. No display is a
   supported mode.
7. Move Glove80-specific constants and CLI diagnostics under the Glove80
   adapter module. Optionally make `hidapi` a `glove80-hid` Cargo feature after
   the behaviour refactor is stable; this is packaging cleanup, not a blocker
   for MacBook input-only support.

## Collision and lifecycle rules

- Validate all enabled registrations before calling `register`. Duplicate
  physical shortcuts across profiles are an error naming both sources and the
  logical keys; never let registration order silently choose a winner.
- If OS registration fails partway through, unregister registrations made by
  this daemon and fail startup. Do not run with a partial invisible key map.
- Input profiles only emit logical activation requests. They do not infer
  source device identity: macOS global hotkeys normally identify a shortcut,
  not which keyboard produced it. This is correct because either physical
  source intentionally activates the same slot.
- MacBook `F1`–`F10` conflict with macOS/app shortcuts. The profile is opt-in;
  the user must enable standard function keys in macOS or hold Fn/Globe. A
  registration failure reports the exact function key.
- Sink absence or HID disconnect is a display-only condition. It must neither
  stop `beckond` nor remove bindings.
- Never permit adapters to mutate the binding ledger directly. CLI and daemon
  requests remain the sole binding writers.

## Test plan

Unit tests:

- V2 config upgrades to one enabled Glove80 input and display, and yields the
  same ten physical registrations as today.
- MacBook profile produces F1–F10 mapped one-to-one to logical F1–F10.
- Invalid/duplicate source IDs, unknown profiles, duplicate shortcuts, and
  empty enabled configuration fail validation with actionable messages.
- Router maps registered event IDs to `KeyId`; unrelated/released events are
  ignored.
- Router rollback unregisters prior registrations when a later registration
  fails (using a fake registrar).
- `NavigationService` keeps focus and repeat-confirm semantics unchanged for
  events from either profile.
- `DisplaySet` calls all enabled fake sinks; a failed sink is isolated and
  retried without republishing unchanged data to a healthy deduplicating sink.
- Existing binding/reconciliation/pane-close/HID encoding tests remain
  unchanged or are relocated with no behavior loss.

Integration/manual checks on macOS:

1. Existing Glove80 Beckon layer still sends all ten mappings and focuses their
   current panes while a different app is active.
2. With only the MacBook profile enabled, standard F1–F10 focus the same bound
   panes; Fn/Globe retains the expected macOS media/system-key path.
3. With both profiles enabled, either shortcut for a slot takes the identical
   focus/confirm path and no duplicate press is generated.
4. With all displays disabled or the Glove80 unplugged, input navigation and
   `beckon status` keep working; reconnecting restores the display.

## Non-goals for this increment

- A third-party dynamically loaded adapter ABI.
- macOS per-key function-row lighting (there is no supported per-key RGB
  display target comparable to the Glove80 HID firmware).
- Bluetooth display transport.
- Changing Herdr bindings, pane identity, or keyboard firmware protocol.
