# Beckon v1 design: clean break, explicit config, portable device model

Status: design note, not yet implemented. This replaces the incremental
adapter-architecture plan in `adapter-architecture.md` where the two disagree.

## Ground rules

- **Single user, no external consumers. No backwards compatibility.** Delete v1/v2
  config readers, serde aliases for old field names, `ConfigV1`, and implicit
  fallbacks. A config either matches v1 exactly or fails with a precise error.
- **No defaults for behaviour.** Anything that changes what Beckon does is
  declared in config. Protocol facts (USB vendor/product ids, report framing,
  state-file layout) stay in code, since they are not choices.
- **Split state by lifetime and owner.** Structural wiring, user-editable
  settings, and runtime/temporal state are three different things.
- **Devices are uniform objects.** MacBook Pro, Glove80 and Creator Micro 2 are
  all input devices; some also expose display capabilities. No device gets a
  privileged position in core.
- **Do not paint into a corner.** Slot counts, channel names, source kinds and
  transport names are data, not constants.

## Vocabulary

| Term | Meaning | Lives in |
|---|---|---|
| **Device** | A config-declared physical object with a stable `id` (`macbook`, `glove80`, `creator-micro`). | config |
| **Port** | A capability a device exposes: `InputPort` (emits activations) or `DisplayPort` (accepts frames). A device may have both. | config declares; code implements |
| **Adapter** | Transport code implementing one port for one device (`creator-micro-usb`, `macos-function-keys`, `glove80-hid`). | code, selected by name |
| **Channel** | An addressable output target inside a display port (`key.6`, `ring`, `underlay`, `f1`). | config |
| **Slot** | A logical, device-independent binding target. Config maps slots to channels per device. | config |
| **Target** | A focusable thing (session, window, block, pane) with an opaque, source-scoped identity. | runtime |
| **Agent** | A `Target` that additionally has state. Modelled as optional state on the target, not a subclass. | runtime |
| **Source** | An integration that enumerates targets and focuses them (Rex, Herdr, later VoiceInk). | config + adapter code |
| **Treatment** | Colour / brightness / motion applied to a channel. | settings |

`Device` is deliberately a **config object, not a trait with behaviour**. A
`Device` trait returning `Vec<&dyn InputPort>` / `Vec<&dyn DisplayPort>` looks
tidy but fights Rust object safety and lifetimes, and would force every input
and display to be co-owned. Keeping the two port traits orthogonal matches the
code that already works (`register_input`, `DisplaySet`) and lets a device with
only one capability declare only that one.

## Type sketch

```rust
// A focusable thing. Everything a binding can point at is a Target.
pub struct Target {
    pub reference: TargetRef,        // { source, scope, id } — opaque to core
    pub title: String,
    pub focused: bool,
    pub agent: Option<AgentStatus>,  // Some(..) only when an agent runs here
}

pub struct AgentStatus {
    pub state: AgentState,           // idle | working | blocked | done | error
    pub detail: Option<String>,      // progress / message
}
```

`Target` first, `agent` optional. This is composition rather than inheritance:
a plain terminal is a `Target` with `agent: None`, and an agent is a `Target`
with `agent: Some(..)`. Agent-state classes never leak into the terminal path,
and the render layer can ask `target.agent` for a treatment or fall back to a
declared neutral treatment. Rust has no subtyping, and a
`trait AgentLike: Focusable` supertrait would drag `dyn` dispatch into every
source for no benefit.

The alternative (a separate `Agent` struct with a `Target` field) is equivalent
but duplicates identity; the optional field is the smaller surface.

## Render model

`RenderPlan` stops being `Vec<KeyRender>` over a fixed ten keys. It becomes a
map of named channels to treatments:

```rust
pub struct RenderFrame {
    pub treatments: BTreeMap<ChannelId, Treatment>,
}
```

`render()` resolves each **bound slot** to a treatment and adds any declared
ambient channels (ring, underlay). A device adapter then maps its declared
channels to its wire format. Six slots, ten slots, or all thirteen Creator Micro
keys are simply different `channels` lists in config. Core never sees `AG06`,
`f1`, or a slot count.

This removes, in one move:

- `KEY_IDS` and `valid_key`'s fixed `f1`..`f10`
- `DisplayConfig::validate`'s "exactly ten keys"
- `CreatorMicroConfig::validate`'s "exactly six slots"
- the hardcoded "first six keys -> AG06..AG11" in the Creator Micro adapter
- `built_in_themes()` in `render.rs`

Worst-state aggregation stays in core (`RenderPlan::worst_state`) so every
display agrees on urgency, but the channel it drives is declared in config.

## Configuration split

```
~/.config/beckon/
  config.toml     Nix-generated, READ-ONLY, structural only, fully explicit
  settings.toml   app-owned, mutable, NOT in dotfiles (themes, colours, toggles)
~/.local/state/beckon/
  bindings.json   temporal runtime state, NEVER in dotfiles
  surfaces.json   temporal
  beckon.sock     ephemeral
  key-events.log  ephemeral
```

`config.toml` is generated from structured Nix options (per the dotfiles style
rule), so absolute paths are interpolated rather than hand-written. It declares
devices, ports, adapters, channels, slot-to-channel routes, sources and their
commands, and identity env vars. Missing any required field is an error.

`settings.toml` is merged over only the presentation fields. It is what the
Swift app writes. It is not drift-checked against dotfiles, because it is not
policy. If theme reproducibility is wanted later, add a `mutableConfig` entry
with `dotfilesSrc = null` (report drift, never export).

`bindings.json` and friends are already in `~/.local/state/beckon/` and are
already untracked; that part is correct today and should stay that way.

## What the clean break deletes

- `ConfigV1` and the v1/v2 upgrade path; one `config_version` value only
- `#[serde(default)]` on every behavioural field (~30 sites in `config.rs`)
- `DEFAULT_SOURCE = "herdr"`, `default_herdr_cli`, `default_output_adapters`,
  `default_keys`, `default_theme_name`, `default_*_brightness`, and friends
- serde aliases for legacy `session` / `pane_id` field names
- the implicit Herdr source in `build_sources()`. Herdr becomes an explicit
  `[[sources]]` entry with its own `list`, `focus`, `activate` and `identity`
- the global Herdr-flavoured `[focus] command`; activation becomes per-source
- `focus_key`'s `source == DEFAULT_SOURCE` special case, replaced by a
  source-declared activation step

## Consequences that need fixing alongside

- **Focus commands must not inherit the daemon's environment.** The current
  daemon inherits `REX_BLOCK` / `REX_SESSION` from its launching shell, and
  `rex window focus -s SESSION` with no window argument resolves to "the window
  containing REX_BLOCK", i.e. the wrong block. Focus argv and its environment
  must be explicitly declared, with a clean base environment.
- **Session-level binding is ambiguous** when a session has 6-11 blocks. Exact
  "take me to the agent" needs a block-level target; session-level keys should
  show the worst state across the session's agents. Both should be expressible
  as separate sources, chosen at bind time.
- **Agent vs terminal colouring** is a declared `kind` plus a declared neutral
  treatment, not a hardcoded branch in core.
- **The launchd service uses the published `beckon` binary**, not a worktree
  build. Until the branch is released and `nup` updates the flake input, the
  service cannot parse v1 config. Manual daemon runs are the test path.

## Explicitly deferred (keep in mind, do not design corners around)

These are recorded so the v1 shape does not preclude them. None is a v1
requirement.

1. **Glove80 parity.** It has not been used for a while. Re-verify input
   registration and LED display after the refactor; do not assume the old path
   still works, but keep its adapter behind the same port traits.
2. **Ghostty / OmniWM focus adapter.** Revisit alongside the Herdr integration.
   There is now a dedicated **Herdr app** and **HC (Herdr Core)**; the
   integration may move towards a desktop-app-native focus path instead of a
   shell script that queries `omniwmctl`. Keep activation per-source so this can
   be swapped without touching core.
3. **VoiceInk / microphone key.** State (`idle`, `starting`, `recording`,
   `transcribing`, `enhancing`, `busy`, error) is readable from unified logging
   on the installed paid binary (`log stream --predicate 'subsystem ==
   "com.prakashjoshipax.voiceink"'`). No source build, no REA, no Accessibility
   permission. Enter and mic keys do not emit `v.oai.hid`, so they need either
   OAI/ACT keycodes or an external state source.
4. **Flexible slot counts.** Six is the current layout, not a limit. Other
   people may bind every key (13 on the Creator Micro 2). Channels and routes
   are lists in config; nothing in core counts them.
5. **Plain terminals / logs.** Possibly bindable as `Target` with
   `agent: None`, but v1 focuses on the agent side. Keep `Target` as the base
   so this is additive.
6. **Enter key / translucent keycap indication**, confirm-armed state,
   per-key AG addressing for non-agent keys, and `sk`/`sa` sync flags that let
   a thread drive the keys backlight or ambient ring.
7. **Ring vs underlay decoupling.** They are separate `v.oai.rgbcfg` zones;
   the current `keys.m = 1` makes the keycap backlight mirror the ring. Both
   magic and speed should be independently configurable.
8. **Input keymap persistence.** The `AG06`-`AG11` Rex layer is on the device
   but not committed to Input's saved copy; reopening Input may revert it.
   Power-cycle persistence is unproven.
9. **Swift app** as UI/config client over IPC. It never drives hardware;
   `beckond` stays the single writer.

## Non-goals for v1

- Third-party dynamically loaded adapter ABI
- Backwards compatibility of any kind
- Bluetooth display transport
- macOS per-key function-row lighting
- Mixed Herdr + Rex slot allocation policy (explicit bindings only)
