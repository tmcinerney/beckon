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
`--backend-socket ABSOLUTE`, `--device-serial EXACT`, `--device-product-id 0x8297|0x8298`, `--profile-id 2`, `--layer-id 0`
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
keymap.json through fs.list/fs.readbin. Replies and layer reports are labelled unverified because
fresh-process reset/drain and controller coexistence have not been established.
It never auto-reconnects or changes Input, keymaps, AppSense, other controls,
permissions, running agents or services. Reconnection needs a fresh release/press.

The keymap is read once per observation via `fs.list` and `fs.readbin`, following
Input's chunked file contract rather than the older whole-file `fs.read`. The
reader uses the physically demonstrated 192-byte chunks, canonical base64, a 256 KiB metadata cap with an effective 18,432-byte transfer cap, a 96-chunk
budget and the connection's 60-second deadline. It checks exact file metadata
before and after the read and verifies SHA-1 before publishing the snapshot.
Changed size/checksum, unexpected chunks, cancellation or generation changes
reject the snapshot. Status polling afterward does not reread the keymap.
Diagnostics show reply types, bounded field names and sizes, never file contents.


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


The optional **Experimental single ≤192-byte prefix read** makes exactly one `fs.readbin` call at offset0 between stable file listings, after firmware/status checks. It does not fall back, retry, build a snapshot, validate full-file checksum/schema, or enable a writer.192raw bytes yield at most256base64 characters; this is an experimental response-size reduction from the SDK default3072raw bytes, not a documented firmware limit or a fix. Success establishes a small prefix read on that attempt; it suggests size/load sensitivity only if the failed larger request was indeed larger. The previous screenshot lacks that request size. Failure preserves offset/requested length/completed chunks and matched numeric RPC code, or explicitly unavailable code. Neither outcome proves controller exclusivity or physical mapping. Full read errors now include the same request metadata. Do not request another physical test before review and an explicit coordinated handoff.


The a0b1ccf human prefix attempt accepted requested192/received192/total13298 on firmware0.6.3, reported layerIndex2 and stopped with writer disarmed. This establishes prefix success only and supports size sensitivity; it does not establish the firmware's exact allocation limit. The replacement full reader uses fixed <=192byte requests,50ms pacing,96chunk maximum, and rejects files over18432bytes before reading. The actual13298byte file fits70chunks (69x192 plus50). Stable size/SHA1 before-after, final digest, JSON/schema, generation, cancellation and60second deadline are still required. No adaptive retry or configuration/LED/focus writes. Next physical test uses the replacement normal full-read button once; do not rerun the prior3072byte build or claim prefix success as full snapshot acceptance. Backend panel status is independent of USB read acceptance.


A separate explicitly authorized **bounded slot4 light/event pilot** is opt-in through `--allow-one-key-leds` plus a manual GUI start. It requires the verified snapshot to show profileID2/Herdr layerID0 at current firmware status indices, with only matrix[1][1] assigned AG03. It does not edit the keymap. Exact-device ownership is a manual controller handoff. The adapter uses existing WLKit thread/AG primitives, writes only id3 at<=20% with both sync flagsfalse, checks current layer/profile/generation before operations, and stops after60seconds. Stop waits for one in-flight paint, clears only id3 if still on that layer, and requires Input-owner binding/appearance restoration; a cleanup ACK isn't visual restoration proof. The fixed index3 legacy proof gate is not used or fabricated. Events remain event-time-unattributed. **Focus is explicitly unsupported**: installed Herdr protocol22 exposes pane.focus(pane_id)/agent.focus(target), without atomic expectedterminal/nativeagent validation. No unfenced fallback, agentinput, globalF4 or genericwindow guessing is performed. This partial mode must not be described as the completed end-to-end pilot or production coexistence.

Layer checks are observational: the device can switch after a status reply before a write applies. The exclusive human pilot must keep Herdr selected and not use layer/profile controls until stopped. This mode does not claim atomic layer-scoped firmware writes or guaranteed coexistence; observed layer changes stop it. Normal Quit awaits bounded in-flight paint drain and id3 cleanup before terminating. All writes use the original observation deadline rather than a new budget after file transfer.

The pilot reserves the last10seconds of the original60second transport budget for stop/drain/status/off attempts; active updates end by50seconds, rather than trying cleanup after the HID request budget has already expired. Delayed operations also recheck this earlier deadline before dispatch. Cleanup remains bounded and ACK-only, with Input-owner restoration required.

### Explicit reversible slot4 configuration test

Add `--allow-slot4-config --config-backup-directory /absolute/durable/new-directory` to the explicit one-key launch arguments. This selects768 numeric IDs/120seconds before the first USB request; it cannot reset or extend an active process. Buttons Apply slot4 and Restore original are manual. Apply changes only profile2/layer0 keymap[1][1], KC_NONE→KV_OAI_AG03. Original/candidate/manifest/journal are privately created, synced and verified before transaction begin; existing backup folders are never overwritten. Restore reloads the device-bound backup and refuses observed unrelated edits.

Close competing device controllers through the coordinated handoff first. Keep Herdr selected for the LED test. Run Apply, the short LED/event test, Stop USB, then Restore promptly in the same process. Ordinary requests reserve302 IDs; cleanup preserves300; restore reserves2. Apply has a45second deadline and leaves70seconds; restore is bounded to60seconds. This preserves an opportunity to attempt restoration, not a guarantee of recovery if hardware stops responding. Stage/commit failures report bounded live readback classification, retain backups, and never automatically retry, abort, reset or restore. A new coordinated process may be needed after expiry. The backup parent must already exist at a durable path.

AG03 focus is explicitly unavailable in this test. No approval response is sent. No firmware reset/update, all-key mapping, zone writes or production service is enabled. Physical transaction support, full Apply/Restore readback, untouched-control checks and normal Input appearance after handoff require manual acceptance.
