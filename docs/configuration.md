# Configuration v3

## Ownership and names

- `config.toml`: explicit global `bindings` order, source integrations, themes,
  optional terminal/focus wiring and repeat-press confirmation policy.
- `config.d/NAME.toml`: one device instance. Its adapter, physical addresses,
  actions and feedback live together. `NAME` is arbitrary lowercase kebab-case;
  `adapter` selects code, not the instance's identity or connection transport.
- `settings.toml`: machine-owned presentation overrides: `[display] theme`,
  `[display.themes]`, and `[devices.NAME.channels]` / `[devices.NAME.effects]`.
  Channels are declared by the adapter, not invented by the UI. The daemon writes
  atomically; settings cannot add devices, binding targets or actions.
- State belongs in `$XDG_STATE_HOME/beckon`, not in configuration.

The filename is a grouping/enable boundary, not a loading-order convention. Each
file is parsed before resolving references. Actions and effects are local to
that device; global binding ids are declared once in `config.toml`. Identical
action names in different files are independent. There is no mandatory shared
file, implicit import, last-file-wins merge or dependency on filename order.

The root template declares all fields, including disabled features. Adding a
device does not enable a source. A disconnected optional device is not a config
error. Removing its file disables it on daemon restart; structural hot reload
is not implemented.

## Example control

Inside `creator-micro.toml`, with an `options.layers.test` declaration:

```toml
[keys.key11]
members = ["creator-micro:test/16", "creator-micro:test/17"]
action = "dictate"
effects = "dictation"

[actions.dictate]
kind = "chord"
keys = ["ctrl", "opt", "d"]
mode = "tap-hold"
tap_ms = 300

[effects.dictation]
held = { color = "#FF0000", brightness = 1.0, motion = "steady" }
latched = { color = "#FF0000", brightness = 1.0, motion = "breathe" }
```

`key11`, `dictate` and `dictation` are arbitrary names. Only `kind`, `mode`,
field names and motion values are schema vocabulary. There is no PTT/VoiceInk
action in core. Another application can use another chord without code changes.

### Explicit grouping, not accidental coupling

One `[keys.NAME]` is one control. Every `members` address participates in its
any-held state. Only the first down/last up edge emits an action. Duplicate
reports and rolling from one member to another do not re-trigger it. Feedback
is written to all active-layer members of that group.

Two separate controls referencing the same action are **not joined**. They
share a reusable action definition but retain independent state. The system
emitter reference-counts identical held chords so one control cannot release
another's output prematurely.

`binding = "key1"` instead of `action` makes a control navigate a declared agent
binding. A control cannot declare both; there is no hidden action priority.
Binding ids can be arbitrary names and have arbitrary capacity. The root's
ordered list controls first-free allocation; map sorting never defines hardware
position or assignment order.

## Modes describe emitted events and inferred feedback

| Mode | Press | Release | Feedback after release |
|---|---|---|---|
| `hold` | chord down | chord up | idle |
| `toggle` | chord down then up immediately | no additional output | alternate latched/idle on successive presses |
| `tap-hold` | chord down | chord up | short press flips latch; long press clears it |

For `tap-hold`, `tap_ms` is required and positive. It is forbidden for the other
modes. A duration strictly below the threshold is a tap; exactly at it is a hold.
The example's 300 ms is a configurable starting value, **not a measured VoiceInk
constant**. Firmware events contain no timestamp, so timing uses host receipt.

These are output gestures, not a receiving-application API. `toggle` sends a pulse
for an app that toggles on its shortcut; it does not leave an OS key held between
presses. `tap-hold` passes the physical hold through and mirrors the receiver's
assumed tap/hold rule for feedback. Configure matching rules at both ends.

The light is **inferred state**, not proof that a microphone is listening.
Stopping capture elsewhere, changing receiver settings, a missed event, layer
changes or restarting the daemon can desynchronize it. No VoiceInk status API is
claimed. `beckon reset --device NAME` clears inference and releases held output;
it does not send a toggle pulse or promise to stop the receiver's ongoing capture.

Layer changes, disconnects and orderly SIGINT/SIGTERM/SIGHUP shutdown release held
output and clear inference. SIGKILL/power loss cannot run cleanup. Posting failures
are surfaced instead of latching a successful-looking light.

## Physical addresses

An address is `device-instance:adapter-address`. Core validates the prefix;
the selected adapter parses the rest.

| Adapter | Example | Meaning |
|---|---|---|
| `creator-micro` | `pad:rex/16` | declared layer `rex`, firmware id `AG16` |
| `hotkeys` | `macbook:F1` | a global standard function-key shortcut |
| `glove80` | `glove80:Shift+F16` | a global shortcut emitted by its firmware |

`16` is not physical key 16 or the name `key11`. It is the vendor's id carried
by a keymap cell. In the tested layout, bottom-left carries AG16 and bottom-centre
AG17. There is no universal `key11 → AG16` convention. Configuration owns that
mapping; the canonical device keymap owns placement. Rearranged-layer behaviour
remains a hardware unknown, so the reference template keeps placement consistent.

Creator Micro options explicitly declare refresh interval, the firmware `s`
parameter (`effect_speed`), base/ring treatments, ring-state colour selection and
layer selectors. The adapter seeds selectors from `device.status`, follows ordered
`kb.pl` events and translates input before it reaches core. A layer's optional
`source` filters both agent lighting and navigation, not chord-only controls.
Unknown layers receive no normal lighting publications. AG00–AG05 are never claimed.

Ordinary `KC_ENT`/macro keys do not emit the AG events consumed by this adapter.
Leaving bottom-right as `KC_ENT` preserves Enter without host synthesis. The
reference Rex layer keeps existing non-agent macros; its extra four agent bindings
are available only on the test layer. The joined pair requires AG16/AG17 in both
layers if both are configured.

Hotkey registration is system-wide, not physically per keyboard. Shortcut aliases
are canonicalized before collision checks. One hardware instance per built-in
hardware adapter is supported; configuring two without hardware selectors fails
rather than opening the same endpoint twice. Bluetooth RPC is unverified.

## Validation and inspection

`beckon config check` performs offline validation without opening devices:

- strict serde fields and tagged action/mode enums (syntax errors have TOML spans);
- duplicate names, physical addresses and canonical shortcut aliases;
- missing binding/action/effects/source references, with nearby-name suggestions;
- adapter-specific addresses, reserved ids, layer selectors and options;
- positive thresholds/refresh periods, complete treatments and brightness ranges;
- chord modifiers/keys, and synthesized chords that would feed back into an input;
- presentation settings cannot inject structural objects.

Semantic errors name the file/reference; they are not all span annotated. Validation
reports the first failure, never silently ignores an invalid definition. Unknown
fields inside adapter options are rejected by that adapter's typed parser.

`beckon keys` prints the **configured** model, not a live discovery result.
`beckon config show` asks the daemon for its effective model over IPC. A Swift UI
should use this protocol, never open HID or parse/rewrite structural TOML.
`beckon config theme NAME` validates the theme and updates `settings.toml` via IPC.
The [configuration IPC contract](ipc-configuration.md) supports revision-fenced
colour/brightness/motion changes and owner-fenced UI binding release. Presentation
updates preserve held/latching state; they do not re-trigger an action.

With the daemon stopped, `beckon creator-micro check` reads the actual keymap and
checks that configured layer/id combinations exist. `get keymap.json FILE`
provides the source for inspection; `verify` checks byte equality after `put`.
Readback and observed LED behaviour remain necessary: a firmware ACK alone does
not establish addressability or synchronized animation.

## Permissions

Chord output is currently implemented on macOS. Supported chord keys are ANSI
letters/digits/punctuation, Enter/Tab/Space/Backspace/Escape, arrows/navigation
keys and F1–F20, with explicit `ctrl`, `opt`, `shift`, `cmd` modifiers before the
single ordinary key. Unsupported/duplicate keys fail validation. Non-macOS chord
posting reports unsupported instead of pretending to send events.

The actual daemon needs macOS event-posting permission (Accessibility); it is
checked before posting. A successful terminal-launched probe does not prove a
LaunchAgent's grant. Prefer a stable, properly signed executable for deployment.
Raw Creator HID access can additionally require Input Monitoring. Global shortcut
registration and pane RPC are separate capabilities. Ghostty scripting can need
Automation. No permission prompts are opened automatically by the daemon.

## Upgrade checklist

1. Stop the old daemon; preserve configuration, binding ledger and native recovery
   context. Do not run diagnostic readers alongside it.
2. Install a binary supporting config v3, create the root config and enable only
   the device files you want. Configure sources explicitly. Keep the keymap compact.
3. Explicitly migrate old ledger ids (`f1` → a chosen name such as `key1`) without
   changing source/scope/agent identities. There is no implicit rename/import.
4. Run offline checks, keymap readback verification and online address checks.
   Ensure the joined pair is AG16/AG17 and bottom-right remains Enter on each
   intended layer; do not overwrite the reserved ChatGPT layer.
5. Start the actual service and verify its grants, tap/hold behaviour, layering,
   source filters and device reconnect. Test real receiver timing, not only mocks.

Do not deploy v3 TOML to a service pinned to v0.4.x. Do not erase a ledger merely
because names changed. Firmware replacement is not part of this upgrade.
