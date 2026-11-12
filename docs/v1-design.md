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
beckon repo (versioned):
  templates/
    devices/creator-micro.toml     # reference definitions, heavily commented
    devices/glove80.toml
    devices/macbook.toml
    sources/rex.toml
    sources/herdr.toml
    themes/rex.toml

machine (local, NOT in dotfiles):
  ~/.config/beckon/
    conf.d/
      10-creator-micro.toml       # enabled fragments; symlink/copy of a template
      20-rex.toml
      30-macbook.toml
    settings.toml                 # app-owned, mutable (themes, colours, toggles)
  ~/.local/state/beckon/
    bindings.json                 # temporal, NEVER in dotfiles
    surfaces.json                 # temporal
    beckon.sock                   # ephemeral
    key-events.log                # ephemeral
```

There is **no single Nix-generated `config.toml`**. The enabled set of
integrations is exactly the set of files in `conf.d/`. Adding support means
dropping in a fragment and restarting; removing support means deleting it.
Nothing about Glove80, Creator Micro, MacBook, Rex or Herdr is compiled into
core as an object.

The repo templates are the durable, reviewable definitions. A deployed fragment
is a machine-local, user-editable copy or symlink. Nix/dotfiles may install a
chosen subset of templates into `conf.d/`; the rest are the user's to add. That
keeps reproducibility (templates are versioned) without putting runtime
config in git.

`settings.toml` is merged over only the presentation fields and is what the
Swift app writes. It is not drift-checked against dotfiles, because it is not
policy. If theme reproducibility is wanted later, add a `mutableConfig` entry
with `dotfilesSrc = null` (report drift, never export).

`bindings.json` and friends are already in `~/.local/state/beckon/` and are
already untracked; that part is correct today and should stay that way.

## Naming convention: device-neutral slots

`f1` is Glove80/MacBook vocabulary. `AG06` is Creator Micro vocabulary. Neither
belongs in core. The naming layers are:

| Name | Example | Where it exists |
|---|---|---|
| **Slot id** | `s1`, `s2`, ... `s10` | declared once in a `kind = "slots"` fragment; the only name core knows |
| **Channel id** | `key-1`, `ring`, `underlay`, `led-1` | local to one device fragment |
| **Physical address** | `ag = 6`, `zone = "ambient"`, `shortcut = "F1"`, `index = 0` | only inside a device fragment, only interpreted by the adapter named there |
| **Target ref** | `{ source, scope, id }` | opaque identity, no `pane_id` / `session` |

A slot is an **opaque id in a declared ordered list**, not a function key. Core
never sees `F1`, `F16`, `AG06`, or an LED index. This removes `KEY_IDS`,
`valid_key`'s `f1`..`f10` check, and `DisplayConfig::validate`'s ten-key rule.

Code organisation follows the same boundary:

```
core/       slots, targets, treatments, render frame   (no device vocabulary)
adapters/   per-transport translation: slot id <-> physical address
config/     declared slot list, device fragments, source fragments, routes
```

## Simplest v1 slice

The minimum that works, with everything else deferred:

1. A `slots` fragment declares `s1`..`s10` (the count is a config choice, not
   a constant).
2. `creator-micro.toml` maps six channels to `s1`..`s6`, plus `ring` and
   `underlay` channels with no slot.
3. `glove80.toml` maps its first six LEDs to `s1`..`s6` (ten if desired).
4. `macbook.toml` maps `F1`..`F6` to `s1`..`s6`.
5. One `rex` source fragment. Bindings are `slot -> Target`.
6. **No layer support, no app-focus awareness, no per-device binding sets.**

That is enough for "Glove80's first key and Creator Micro's first key bind to
the same thing", which is explicitly acceptable for v1.

### Can bindings mix programs? Yes

A binding stores `(slot, source, scope, id)`. `s1` can be a Rex session and
`s2` a Herdr pane. Pressing either calls that source's declared focus command.
Nothing forces all slots to be one program; if all six are Rex it is only
because six Rex sessions were bound. Beckon does **not** need to know which app
is focused, and the Glove80 does **not** need layers. The source recorded at
bind time is the answer to "where does this key go".

### Choosing which bindings appear on which device

This is the device fragment's channel-to-slot map, not a second binding
namespace. `creator-micro.toml` can map `key-1 -> s3` and `key-2 -> s1` to show
a different selection or order. The Glove80 maps its own channels to whichever
slots it wants. Same binding set, different projections.

### Layers are a device-local view, not a core concept

Switching the pad to the Rex layer does not focus anything; it changes which
physical keys exist. Focus happens on press. For v1 the Creator Micro has one
flat channel map. The firmware already scopes AG overrides to the active layer,
so on other layers those physical keys carry different keycodes and our
overrides simply do not apply. Beckon needs no layer logic.

If we later want the Herdr layer to show Herdr bindings, add per-layer maps to
the device fragment:

```toml
[layers.rex]
"key-1" = "s1"
[layers.herdr]
"key-1" = "s7"
```

The adapter selects the map using `device.status.layer_index`. That is a
**config addition to one fragment**, not a core change, and it never touches
the Glove80 or MacBook. So starting flat does not paint us into a corner.

## Fragment convention and dynamic construction

Beckon scans `conf.d/*.toml` in **lexical filename order** and builds the
device/source/channel topology from the validated result. There is no
hardcoded list of devices, no fixed slot count, and no implicit objects.

- Each file declares **exactly one object**, with a `kind` discriminator and a
  stable `id`: `kind = "device" | "source" | "theme" | "route"`. One object per
  file keeps a fragment self-contained and safely removable.
- Each kind parses with `deny_unknown_fields`. A typo is a startup error naming
  the file and field, not a silently ignored key.
- Duplicate `(kind, id)` across files is a hard error. No cross-file merging;
  an object is defined in one place or it is not defined at all.
- After parsing all fragments, a **cross-reference validation pass** runs:
  every adapter name is registered, every route references an existing slot and
  an existing device channel, every theme reference resolves, and no two
  devices claim the same input shortcut. Failure refuses to start with a
  precise message. A partially valid directory never yields partial, invisible
  behaviour.
- Restart is the activation mechanism for v1 (no hot reload / directory watch).

**Boundary to be explicit about:** adapters, i.e. transport implementations
that speak USB HID, macOS global hotkeys, or a process plugin, are Rust code
registered by name. Devices, sources, channels, slots and routes are data.
Adding a *device instance* is config-only; adding a *new transport* still needs
code. That is the intended seam for "keep as much as possible outside core".

### Slot and channel ownership

Each device fragment owns its own channel and slot declarations, so slot counts
are never a core constant. `creator-micro.toml` declares six channels (one per
AG key) plus `ring` and `underlay`; a different layout declares thirteen. The
daemon creates that channel cluster dynamically from the validated file.

Logical slots stay global, and each device declares how its channels map to
them. That keeps the earlier "one binding lights both devices" behaviour
without hardcoding either device's width. If device-local slot namespaces are
wanted instead, that changes whether a binding can span devices and must be
decided before the render model is built.

## What the clean break deletes

- `ConfigV1` and the v1/v2 upgrade path; one `config_version` value only
- `#[serde(default)]` on every behavioural field (~30 sites in `config.rs`)
- `DEFAULT_SOURCE = "herdr"`, `default_herdr_cli`, `default_output_adapters`,
  `default_keys`, `default_theme_name`, `default_*_brightness`, and friends
- serde aliases for legacy `session` / `pane_id` field names
- the implicit Herdr source in `build_sources()`. Herdr becomes an ordinary
  `conf.d/` source fragment with its own `list`, `focus`, `activate` and
  `identity`
- the single monolithic `config.toml`, replaced by validated `conf.d/`
  fragments
- the global Herdr-flavoured `[focus] command`; activation becomes per-source
- `focus_key`'s `source == DEFAULT_SOURCE` special case, replaced by a
  source-declared activation step

An empty `conf.d/` is a valid config that simply does nothing. It is not an
error, and it does not silently enable anything.

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
   are lists in the device fragment; nothing in core counts them. This is a v1
   requirement now, not a deferred one.
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
