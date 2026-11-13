# Creator Micro 2 hardware and protocol facts

Hardware observations here were established against a live device. SDK-derived
names and implementation choices are called out separately from measured facts.
Work Louder publishes no protocol reference; the Input app ships only its own
default templates, and the firmware enforces rules the app never expresses.

Record the **method** alongside each fact. `put` acceptance is not evidence of
anything — see [Validation traps](#validation-traps).

## Device identity

| Fact | Value |
|---|---|
| Name | `Creator Micro 2` |
| USB vendor id | `0x303A` |
| RPC report id | `0x06` |
| RPC channel byte | `0x02` |
| Report size | 64 bytes |
| Firmware observed | `0.7.0-rc.16` |

`device.status` returns `{battery, is_charging, layer_index, name, profile_index, version}`.

## Agent id space

**19 addressable ids, `AG00`–`AG18`.** Not 13, and not `0`–`25`.

| Ids | Status | How it was established |
|---|---|---|
| `0`–`5` | reserved for the ChatGPT layer | firmware rejects these keycodes on any other layer, deterministically |
| `6`–`18` | usable, 13 ids — one per physical key | all 13 keys lit with distinct `thstatus` colours |
| `19`+ | **not addressable** | accepted by `put`, key renders the layer base colour instead of the override |

The ceiling is enforced by firmware, not the app. The Input app's only OAI
keycodes are `AG00`–`AG05` and `ACT06`–`ACT12` (its default layer template); it
carries no validation table, so nothing in the app reveals the real range.

Consequence: **13 agent ids for 13 physical keys.** There is no second block, so
a second layer cannot have its own distinct id range. Layers must reuse
`AG06`–`AG18`, and the host must disambiguate by layer. See
[Layers](#layers).

## Key events

A key press arrives as:

```json
{"m":"v.oai.hid","p":{"act":1,"k":"AG18"}}
```

| Field | Meaning |
|---|---|
| `k` | firmware keycode string |
| `act` | `1` press, `0` release |

There is **no `ag` field and no layer field.** The slot must be parsed out of the
keycode string. A `v.oai.hid` event therefore cannot be attributed to a layer on
its own.

The event carries no timing either — only press and release. Any tap-versus-hold
distinction is measured host-side, which means it has to be tuned to whatever the
receiving app treats as a tap, or the two will disagree.

## Layers

- Layer **ids are arbitrary** and not positions. A profile was observed holding
  ids `1,3,6,0,5,7`; rewriting them to `0..5` was accepted and preserved order.
- `layer_index` is the **1-based position** in the profile's layer array. With
  Rex at position 3 it reported `3`; with the sixth layer active it reported `6`.
  Renumbering ids did not change it, because it tracks position, not id.
- Layer changes are announced live, including from the capacitive touch button:

```json
{"m":"kb.pl","p":{"l":6,"p":1}}
```

`l` is the 1-based position. This is the only in-band signal of a layer change.

### Why layer awareness exists for this device only

The Glove80 does not need it: each layer maps to **distinct logical keys**
(`F16`–`F20` vs `Shift+F16`–`Shift+F20`), so the layer is encoded in the keycode
and the host stays stateless. The MacBook keyboard has no layers.

The Creator Micro is different: all layers share the single `AG00`–`AG18` space,
so `AG09` on one layer is indistinguishable from `AG09` on another. Attributing a
press to a layer requires tracking the latest `kb.pl`, seeded from
`device.status.layer_index` on connect. That logic belongs in the
`creator-micro-usb` adapter; core still sees only logical slots.

## Lighting

Per-key colour is addressed by agent id:

```json
[{"id":6,"c":16711680,"b":1.0,"e":1,"s":0.5}]
```

sent via `v.oai.thstatus`, and zone lighting via `v.oai.rgbcfg`. Both acknowledge
with `{"ok":1}`, so a write can be confirmed rather than assumed. The payload is
an **array**, so one RPC request can update several keys. A request may span
multiple HID reports; atomic LED presentation has not been established.

| Field | Meaning |
|---|---|
| `id` | agent id, `0`–`18` |
| `c` | colour as `0xRRGGBB` |
| `b` | brightness `0.0`–`1.0` |
| `e` | effect id |
| `s` | effect speed, `0.0`–`1.0`; exact semantics unverified |

Effect ids in use, all confirmed by eye:

| `e` | Effect |
|---|---|
| `0` | off |
| `1` | solid |
| `2` | snake |
| `4` | breath |

`3` and `5` are unexercised. The vendor enum lists exactly six effects in the
order `off, solid, snake, rainbow, breath, gradient`, which is consistent with
`3` = rainbow and `5` = gradient, but that ordering has not been confirmed.

**Clearing an override** is `{"id":N,"c":0,"b":0.0,"e":0}`. The key drops back to
the layer base colour rather than to black — there is no "unset" separate from
"off".

**Overrides are runtime state, not keymap data.** A competing host can overwrite
them. Observed: a held override disappeared mid-press until the host began
rewriting it every 400 ms. Exact partial-update/merge semantics and persistence
across every reconnect case have not been established.

- Only a key carrying an `AGnn` keycode has an id, and therefore a per-key colour.
  Keys carrying macros, `KC_*`, or `SA_*` sit on the global `keys` zone and can
  only take the layer base colour.
- `lights.preview` / `WLDeviceLightingConfig` is zone-level only (`keyboard`,
  `side`). It is not a per-key API.

## Keymap constraints

- A cell holds **exactly one keycode**. A key cannot both carry an agent id and
  emit a macro or multiAction.
- `KA_A<n>` is a device macro, `KA_M<n>` is a device multiAction (tap/hold/
  double-tap with a threshold). Both execute **on the device**, emitting
  keystrokes straight to the host — Beckon never sees them.
- `SA_*` keycodes are executed by the **Input app** on the host, so they stop
  working if that app is not running.
- `KI_*` are knob and joystick interactions.
- Keymap cells are ordered row-major in **display order**. On the 13-key grid,
  flat positions 10, 11 and 12 are bottom-left, bottom-centre and bottom-right;
  observed carrying `AG16`, `AG17` and `KC_ENT`.

## Joined controls

Nothing in the firmware groups keys. Two adjacent ids become one control purely in
the host: map both ids to one logical slot, write the same frame to both, and
treat "any member held" as the control being held.

Verified on `AG16`/`AG17` (bottom-left and bottom-centre): pressing either lights
both, releasing the last one clears both, and rolling a finger from one to the
other does not re-fire the action. Only the transition between nothing-held and
something-held matters.

## Validation traps

**`put` acceptance means nothing.** Observed directly:

- `AG18` was rejected with `400 InvalidInput`, then accepted on retry.
- `AG20` was rejected, then accepted, and rendered as a non-slot.
- A `put` returned `400 InvalidInput` while the write had in fact landed — the
  readback showed the new keymap.
- A `KC_ENT` write returned `400 InvalidInput`, then succeeded unchanged on retry.

Practical rule: retry until a **readback** shows the intended value. A single
failure is not a rejection, and a single success is not proof.

`beckond` consumes the same `v.oai.hid` reports, so a probe run alongside it
competes for frames. Stop the daemon before observing raw traffic.

**Only LED behaviour is trustworthy.** Send a `thstatus` override and look:

| Observed | Meaning |
|---|---|
| key takes the override colour | real, addressable slot |
| key keeps the layer base colour | not a slot |

## Two writers, one resource

The Input app pushes the **whole keymap** to the device on save. A keymap written
by anything else is reverted by the next UI save. Any repo-owned keymap must
therefore be re-applied and verified on activation rather than assumed to persist.

## Unverified

Listed so they are not mistaken for settled:

- **Bluetooth RPC/event/lighting parity.** The SDK contains Bluetooth HID discovery
  support, but that does not prove the Creator Micro exposes this vendor RPC
  collection wirelessly. Two product ids are not proof of transport variants.
- **VoiceInk's exact tap threshold.** Hold behaviour was observed; 300 ms was a
  probe/config choice, not a measured receiver constant. Lights infer gestures,
  not the receiving app's actual recording state.
- **Profile selector semantics under renumbering.** `p=1` and
  `device.status.profile_index=1` matched the Daily Workflow profile, whose id
  and zero-based array index were both 1. Arbitrary profile renumbering is untested.

- **Whether the id → key mapping follows the active layer's arrangement.** Proven
  only for layers that place the same id at the same position. If layer A puts
  `AG06` at position 0 and layer B puts it at position 5, it is unproven that
  `thstatus` id 6 follows the active layer. This matters for re-arranging keys
  per layer.
- Whether `ACT06`–`ACT12` report as slots 6–12 the way `AG` ids do.
- Whether 6 layers is a firmware cap, or merely where the profile stopped.
- Whether a firmware update moves the `AG18` ceiling.
- Whether the ChatGPT host and Beckon can hold the same id simultaneously without
  fighting; ids `0`–`5` are assumed exclusive to ChatGPT.
- Effect ids `3` and `5` — inferred as rainbow and gradient from the vendor enum
  order, never seen.
- What `s` actually controls, and its unit. Only `0.5` has been used.
- Whether two adjacent keys running the same animated effect stay in phase, or
  each animates independently. This decides whether a travelling animation across
  a joined pair is expressible as config or has to be driven frame by frame.
- Whether a per-key override survives a **layer switch**. Untested; relevant
  because layers share one id space.
