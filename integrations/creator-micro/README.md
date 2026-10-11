# Creator Micro status frames

Optional, source-only integration of Beckon's selected native agent slots with
Micro Manager's WLKit library. The core Beckon package has no WLKit dependency.
There is no executable, device writer, polling loop, HID listener, activation flag,
keymap editor, focus action or approval sender in this package.

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
configuration or time; this library cannot clear physical lights when disconnected.

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
