# Creator Micro status preview

Optional, source-only integration of Beckon's selected native agent slots with
Micro Manager's WLKit library. The core Beckon package has no WLKit dependency.
The optional `beckon-creator-micro-preview` app displays the prepared status on
screen. The original mode uses the existing panel observer and has no device writer or HID listener,
keymap editor, focus action or approval sender in the original status and offline Input modes.
The separately opt-in one-key observation mode below can open an exact USB vendor collection.

`StatusAdapter.prepare(configuration:snapshot:directory:now:)` uses the same
`PanelConfiguration` and `SlotResolver` as the panel. Call it with an existing
read-only `AgentSnapshot` and, for coordinator slots, explicit registered metadata.
It returns six `OAI.Thread` records and a presentation or unavailable reason per
slot. `encodedParams()` returns the bare JSON array for WLKit's
`OAI.methodThreads`; it does **not** submit the array anywhere.

| Slot | Physical location | Firmware assignment | Thread ID |
|---|---|---|---|
| 1 | Top-left | `KV_OAI_AG01` | 1 |
| 2 | Top-right | `KV_OAI_AG00` | 0 |
| 3 | Second row, left | `KV_OAI_AG02` | 2 |
| 4 | Second row, middle-left | `KV_OAI_AG03` | 3 |
| 5 | Second row, middle-right | `KV_OAI_AG04` | 4 |
| 6 | Second row, right | `KV_OAI_AG05` | 5 |

These keycodes emit vendor HID events instead of normal keystrokes. This package
does not bind them or process presses. It reuses WLKit's physical reading order
rather than assigning agents by their observation array order.

Working is blue/breathing; idle white; Herdr-reported done/unread green;
blocked/attention amber; unknown gray. Blocked does not mean a structured approval
request; done does not prove tracker completion. Missing, ambiguous, incomplete,
stale (over 10 seconds), future-dated, disconnected, wrong-source and unassigned
observations produce off records. Invalid configuration turns the entire frame
off. Coordinator rebindings require explicit reselection through existing policy.

The frame represents Herdr's latest reported state. It cannot detect a native
session replacement before Herdr reports it. `focusAvailable` and
`liveWriterAvailable` are always false. Frames must be recomputed for new status,
configuration or time. The preview recomputes against the shared panel model and
clock; neither the library nor the preview can clear physical lights when disconnected.

Future hardware activation requires a negotiated single writer with Input, a
backed-up/read-back profile, and proven layer-scoped write/revocation support.
Per-key firmware lighting requires AG codes on the active layer; a successful RPC
alone does not demonstrate a visible light. Do not send even an off frame after
ownership/layer loss. No zone, ambient, joystick, dial or other-key writes are
prepared. Strict native focus remains a separate unresolved contract.

## Offline development checks

WLTools is pinned to `32caf5a429cd0618dbb8c8354e34223d601ee235` from
https://github.com/schacon/micro-manager.git. No dependency source is copied.
Upstream license/redistribution permission has not been established; this is not a
claim that a redistributable hardware bundle has been cleared.

For an explicit local override, set `BECKON_WLKIT_PATH` to a clean checkout at that
revision. Verify with `git -C "$BECKON_WLKIT_PATH" rev-parse HEAD` and
`git -C "$BECKON_WLKIT_PATH" status --porcelain`. Without the override SwiftPM
resolves the pinned remote revision. From Beckon's worktree:

```sh
devenv shell -- bash -c '
  unset SDKROOT DEVELOPER_DIR
  export DEVELOPER_DIR="$(/usr/bin/xcode-select -p)"
  export SDKROOT="$(/usr/bin/xcrun --sdk macosx --show-sdk-path)"
  exec /usr/bin/xcrun swift test --package-path integrations/creator-micro --sdk "$SDKROOT"
'
```

Use `swift build` instead of `swift test` for the library build. The integration
suite uses decoded fixtures only; upstream WLKit live-device/server tests are not
run. Core verification remains `devenv shell -- ./swift/run-swift test`.

## Preview app

The optional app composes the core `BeckonPanelUI` and `PanelModel` with the status
adapter. The core package stays independent of WLKit. A single observer supplies
both the assignment panel and the physical two-key/four-key layout; no second
service or device connection is started. The original modes disable saving, hardware and agent focus. Assignments, source selection and explicit coordinator metadata
selection affect memory only. Existing panel configuration may be read but is
never written; corrupt files are preserved and editing is disabled.

Expand **Assign agents to the six slots** for the existing chooser. Slots use
explicit native identities or accepted coordinator bindings. Missing identities
and roles stay unassigned; titles or tab order never select a replacement.
Changing a source, disconnecting or expiring an observation turns the prepared
indicators off. Displayed unavailable reasons and connection errors come from the
existing model, not a claim that an agent has crashed.

Build without launching (same shell/SDK and pinned WLKit selection as above):

```sh
devenv shell -- bash -c '
  unset SDKROOT DEVELOPER_DIR
  export DEVELOPER_DIR="$(/usr/bin/xcode-select -p)"
  export SDKROOT="$(/usr/bin/xcrun --sdk macosx --show-sdk-path)"
  exec /usr/bin/xcrun swift build --package-path integrations/creator-micro \
    --product beckon-creator-micro-preview --sdk "$SDKROOT"
'
```

Launching and installing are separate decisions. For a later approved manual
check, `swift run` with that same wrapper, SDK and package takes
`beckon-creator-micro-preview --demo --no-save`. A bounded live canary additionally
selects `--herdr-socket ABSOLUTE` and optionally `--config ABSOLUTE`; it reads the
existing default server without starting or restarting it. `--save`, `--focus` and
hardware options are rejected. No automatic AppSense link or layer switch exists.
Input owns the active profile; its documented manual native ChatGPT layer fallback
is preserved. A hardware pilot still requires exact device/profile/layer ownership
and proven write revocation.

In `--demo`, the six example headings are Chief of Staff, Project Coordinator,
Releases and three selectable agents. They label fictional slots; they do not
register real roles, assign real agents or change non-demo defaults. The same two
fixed demo identities remain assigned and the other four slots remain empty. The
feature worker has a long fictional name for wrapping checks. **Demo appearance**
offers System, Light and Dark for this preview window only, stays in memory and
is absent outside demo. It never changes macOS appearance settings.

For a reviewed demo build, check the example headings and long name at the normal
and minimum permitted 920-point window width in both appearances. Open a second
preview window and confirm changing one window's appearance leaves the other
unchanged. When macOS combines them as native tabs, switch tabs repeatedly,
close the inactive second tab, then repeat with the active second tab. The
surviving view must keep refreshing beyond ten seconds without going stale.
Close the final view, reopen from the menu, and confirm observation resumes.
Views share one observer; duplicate appearance/release cannot add a polling loop
or stop another owner. The top health header and cards both expire under the
same freshness rule. Fixture tests cover cancellation and rapid reopen without
live service failures. Replay all eight states and verify identity retention, mismatch rejection and unavailable
hardware/focus/saving. Keyboard and VoiceOver checks remain separate; a Tab key
that does not move AX focus does not establish its cause or justify a global
keyboard-setting change.

Fixture tests and compilation do not establish GUI visual acceptance or physical
LED behavior. No device or live server test is part of the fixture suite.


## Offline Input one-key pilot

The same preview executable accepts `--input-pilot` as its only argument in this
mode. It does not mount the live panel, read panel assignment settings or start
Herdr observation. It offers a bundled sample and a read-only JSON file importer;
no app was launched to validate this increment.

For a later approved manual check, use the existing SDK wrapper above with
`swift run --package-path integrations/creator-micro beckon-creator-micro-preview
--input-pilot`. Choose **Use sample snapshot**, then explicitly select
**Daily Workflow · ID 2** and **Herdr · ID 0**, shown at current position 3.
For an Input export, enter its device-directory ID and choose **Import Input JSON**.
**Reload file** re-resolves the same IDs without selecting the first layer.
Deleting a selection or a bad import makes it unavailable. File imports are
limited to 256 KiB, regular files and Input schema version 1; detected concurrent
edits, duplicate IDs and unsupported geometry are refused. No file is written.
The device ID labels a snapshot source; it is not authenticated hardware identity.

The six cards use physical reading order 1,0,2,3,4,5. Only worker slot4 (firmware
key3 / matrix1,1) has the bundled `beckon.display` v1 `f4` working treatment.
Other slots remain unassigned. **Simulate AG03 press** increments a local
acknowledgment only; it is not a HID listener or binding/focus/approval action.
Snapshot, selection and render changes clear old acknowledgments. Colors and
motion labels come from the existing Rust RenderPlan shape and palette, not
Micro Manager's list order or a second Herdr watcher. Fixture status is always
labeled as such; it supplies no native identity or observation freshness.

Input keeps layers, AppSense, ordinary key actions, joystick, dial, microphone,
Smart Actions, backlight and underglow. No keymap apply/BridgeController or device
transport is used. CLI/UI authoritative binding migration remains a separate
increment. Physical acceptance requires full profile backup/restore, exact device
and active-layer verification, controller coexistence/ownership, narrow one-key
write/cleanup isolation, an approved event route, and native focus contracts.
Fixture tests/build success does not satisfy those gates or prove GUI usability.

## Bounded one-key observation (source prototype)

`--input-one-key-live` is exclusive with all earlier preview flags. It requires
`--backend-socket ABSOLUTE`, `--device-serial EXACT`, `--profile-id 2`, `--layer-id 0`
and `--expected-target JSON`. The JSON contains `session`, `socket` (Herdr source),
`pane_id`, `terminal_id` and optionally `agent_session: {agent,kind:"id",value}`.
There is no Chief of Staff preset or role inference. Shell observations retain the
pane.list terminal ID and use unknown status. A change to f4, source generation,
backend epoch or the observed native tuple requires explicit reselection. The IDs
cannot establish an immutable runtime lifetime or detect every ABA replacement.

The foreground UI starts no I/O in its constructor. Select the target to poll
Beckon's pure `status_pilot` endpoint (one read/second, two-second and 256 KiB
budgets). It uses the existing per-session cache and single binding ledger; this
request never reconciles, saves, prunes, focuses or sends input. The existing
five-second cache refresh joins pane.list and agent.list. Failure, disconnect,
a native event lacking identity, or a lower revision makes the pilot unavailable
until a fresh full observation. The ten-second expiry uses backend age plus local
monotonic elapsed time; repeated old refreshes cannot extend it.

A separate explicit UI action observes the exact USB serial/VID/PID/vendor usage
collection for at most 60 seconds. It reads only sys.version, device.status and
fs.read keymap.json. Replies and layer reports are labelled unverified because
fresh-process reset/drain and controller coexistence have not been established.
It never auto-reconnects or changes Input, keymaps, AppSense, other controls,
permissions, running agents or services. Reconnection needs a fresh release/press.

Physical slot 4 is firmware id3 / AG03 / matrix[1][1]. Exact v.oai.hid AG03 press
observations are debounced and labelled **UN-ATTRIBUTED**. The notification has no
event-time profile/layer identity. The native ChatGPT layer also uses AG03;
polling Herdr before a press does not prove where that press originated. No event
selects, focuses, approves or answers an agent. Last reported layer and age are
shown separately, never used as event attribution.

`--allow-one-key-leds` does not override missing firmware or ownership evidence.
The production writer stays disarmed: omitted-ID preservation, layer scoping,
Input/native-controller coexistence, wire draining and restoration after cleanup
are unverified. The tested writer policy allows only one id3 array record with
sk=sa=0, brightness at most 0.2, solid/breath effects; pulse/flicker fall back to
solid. It serializes paint, retires pending paint before bounded cleanup and never
clears a foreign/revoked layer. An off ACK cannot prove release of a black overlay.
No broad reset, fs.write, rgbcfg or lights.preview is exposed by this mode.

This slice is source/software verification only. Live backend replacement,
GUI usability and the backed-up, owner-verified USB experiment remain separate
acceptance gates. The optional package requires the reviewed WLKit transport
revision; BECKON_WLKIT_PATH may point at its isolated development worktree.
