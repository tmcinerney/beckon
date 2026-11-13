# VoiceInk / Creator Micro integration research

Read-only primary-source audit, 2026-11-13. No live input tests, recordings,
permission/configuration changes, service launches, or VoiceInk builds.

## Findings

- **500 ms is the real hybrid boundary in the 2.21 release-era source**, not
  Beckon's current 300 ms. There is also a separate **500 ms press cooldown**
  shared by the non-double-tap gestures. Main retains the hybrid boundary but
  removes that cooldown. [Release constants][v-constants], [main handler][m-handler].
- VoiceInk accepts a matching **D keyDown carrying Ctrl+Option flags**, then D
  keyUp; this key shortcut does not require separate modifier down/up events.
  Modifier-only shortcuts follow a different path. [Matching][v-match],
  [transitions][v-key-transitions].
- VoiceInk ships **Shortcuts/App Intents toggle and dismiss/cancel controls**,
  but no recording-state query/subscription was found. Internal Combine and
  NotificationCenter state are not external APIs. [Intents][v-intents],
  [state/publisher][v-ui-state].
- Equalizing thresholds reduces one predictable disagreement; it cannot make
  Beckon's locally inferred light track keyboard/UI stops, cancellations,
  startup failures, or ignored shortcuts. A dedicated secondary PTT shortcut
  avoids tap/hold inference but still cannot prove actual capture state.

## Installed version versus public source

Local read-only `plutil` checks of `/Applications/VoiceInk.app/Contents/Info.plist`
returned **2.21 / build 221**. Public GitHub identities inspected:

| Reference | Pinned commit | Interpretation |
|---|---|---|
| [`v2.21` release](https://github.com/Beingpax/VoiceInk/releases/tag/v2.21) | `640b0c8c36ee74d9e76d930f40e2c04bb739b4b3` | Tag resolves directly to this commit. Release published 2026-09-28 and advertises double-tap shortcuts. |
| [“Bump version to 2.21”](https://github.com/Beingpax/VoiceInk/commit/36abef95e685d26d383ce827f41471f5ee6e20af) | `36abef95e685d26d383ce827f41471f5ee6e20af` | Actual project version becomes 2.21 / 221 here. |
| `main` at inspection | [`b5d1be9a7c7377dd059f37bb3e0f791b802d4001`](https://github.com/Beingpax/VoiceInk/commit/b5d1be9a7c7377dd059f37bb3e0f791b802d4001) | “Release version 2.27”, dated 2026-11-11; **not installed 2.21**. |

**Release-source caveat:** `v2.21` itself still declares 2.20 / 220 in
[project settings][v-project]. The [tag-to-version-bump comparison][release-diff]
changes only `project.pbxproj`, `appcast.xml`, and release notes. Thus the
shortcut, intent, and integration code cited below is identical at those two
release-era revisions. This is the best public match, not proof of the exact
source used to compile the installed executable; binary provenance was not
established. Do not silently substitute main for it.

Existing local VoiceInk material was discovered but not used as upstream
evidence: the `feature-recording-state-export` checkout's Git metadata is invalid.
No existing checkout or patch was modified.

## Exact release-era shortcut behavior

### Timing and gates

| Mechanism | Exact source rule |
|---|---|
| Hybrid hold | `pressDuration >= 0.5` **and** engine state `.recording` on release. Otherwise sets hands-free true. No timer waits 500 ms before starting. |
| Press cooldown | On non-double-tap down, ignore when `Date().timeIntervalSince(lastShortcutPressTime) < 0.5`. Timestamp is updated on accepted down, before engine-state checks. It is shared across actions using the handler, not per physical device. |
| Double tap | Each press duration must be in `[0, 0.7]` seconds. Second release minus first release must also be in `[0, 0.7]`; both endpoints accepted. Thus this is **release-to-release**, not merely the gap between taps. |
| Accidental-start interruption | Additional non-modifier keyDown within `<= 1.0` second of shortcut down can interrupt; the same trigger key does not interrupt a key shortcut. Cancellation applies to a press that began with panel hidden and engine idle; double tap clears its pending gesture instead. |
| Engine gating | Recording shortcuts reject action execution while `.transcribing`, `.enhancing`, or `.busy`; double tap additionally rejects `.starting`. |
| Other delays | Initial monitor setup waits 100 ms. Recorder audio-muting action waits 220 ms; that is **not** a recording activation threshold. Default Escape cancellation is two presses within `<= 1.5` seconds, separate from Double Tap recording mode. |

Sources: [constants and down handler][v-constants], [up/interruption handler][v-up],
[monitor interruption][v-interrupt], [engine gate/setup][v-manager],
[mute delay][v-recorder], [Escape handling][v-escape].

The hybrid and double-tap thresholds are `private let` constants: no preference
or user-adjustable timing setting is read there. The mode enums are explicit
`toggle`, `pushToTalk`, `hybrid`, and `doubleTap`. Changing a recording mode
persists its own preference and clears that action's gesture state; it does not
change these constants. [Mode configuration][v-manager].

### Press/release state machine

For a matching key shortcut, down dispatches once and repeated matching downs
are suppressed while the monitor considers it down. Up must match the active
action in the shared handler. Release-era monitor timing uses
`ProcessInfo.processInfo.systemUptime` **when handling the event**, not the
CGEvent timestamp. Main-queue callbacks then launch asynchronous main-actor
tasks. [Monitor timing][v-monitor], [dispatch][v-interrupt],
[manager callbacks][v-manager].

| Mode | Accepted down | Matching up |
|---|---|---|
| Toggle | If internal hands-free flag is true, clear it and toggle panel; otherwise start only if panel hidden. | Set hands-free true. |
| Push to Talk | Start if panel hidden; do nothing if already visible. | If panel visible and engine gate permits, toggle panel. |
| Hybrid | Same down path as Toggle; recording starts immediately, subject to asynchronous startup. | If duration `>= 500 ms` **and already `.recording`**, toggle panel to finish; otherwise set hands-free true. |
| Double Tap | Store press state; do not start recording. Bypasses the 500 ms cooldown. | Validate duration and release-to-release interval; second valid release toggles panel. Pending first release is keyed by action. |

[Owning handler][v-down], [release logic][v-up]. “Toggle panel” is not an
unconditional record command: when visible it finishes `.recording`, cancels
`.starting`/`.transcribing`/`.enhancing`, and handles idle assistant follow-up or
dismissal separately. [Recorder UI control][v-ui-control]. The shortcut gate
normally prevents recording shortcuts reaching the processing cancellation
branches, but App Intents invoke panel control directly.

**Startup caveats:** there is no explicit minimum PTT hold duration in this
handler. Releasing PTT while the panel is visible but engine `.starting` cancels
startup through panel control. Hybrid released after 500 ms while still
`.starting` instead takes the hands-free branch; a long physical hold therefore
does not by itself guarantee stop-on-release. The release-era engine sets
`.starting` inside an asynchronous permission/preflight task. [Engine startup][v-engine],
[UI dispatch][v-ui-control].

**Secondary versus Mode shortcuts:** primary and secondary have independent
recording-mode preferences, as the [official shortcuts docs][docs-shortcuts]
claim. However, they share one handler's pressed action, cooldown, start time,
and hands-free flag; they are not independent recording sessions. A second
action pressed while another is active can be ignored. A shortcut assigned to a
VoiceInk **Mode** inherits the **primary** recording gesture, not the secondary
gesture. [Manager construction/configuration][v-manager], [Mode shortcuts][v-mode-shortcuts].
The validator rejects duplicate stored shortcuts, so pad PTT needs a **different
chord**, not the same Ctrl+Option+D assigned twice. [Validator][v-validator].

## Event shape, key-state queries, and stale D

For the configured key shortcut, matching requires keycode equality and exact
equality of normalized relevant modifiers: Ctrl, Option, Shift, Command, Fn
(Caps Lock and unrelated bits are ignored). Thus extra Shift/Command will not
match Ctrl+Option+D. D keyUp releases an existing matched down regardless of its
modifier flags. A `flagsChanged` event that drops a required modifier also ends
the logical press, even before D keyUp. [Shortcut matching][v-match],
[normalization][v-normalization], [transitions][v-key-transitions].

The release-era event tap is `.cgSessionEventTap`, `.headInsertEventTap`,
`.defaultTap`. Its input extraction reads type, keycode/button, and event flags;
there is **no source-PID/user-data or synthetic-event rejection** in this path.
The active-console/login/unlocked session policy still applies. Therefore a
delivered synthetic D down/up pair with matching flags is accepted by source
logic; this does not prove macOS delivery/permission success. [Tap/input][v-monitor],
[session policy][v-session]. Beckon's emitter currently posts that shape using
an `HIDSystemState` event source at the HID tap; it does not post separate
modifier transitions. [Beckon emitter][b-chord].

**Important version distinction:** the inspected 2.21 shortcut path does **not**
query `CGEventSource.keyState` using either `.hidSystemState` or
`.combinedSessionState`; it maintains its own `isDown` and pressed-key set from
events. Modifier-only shortcuts require `flagsChanged`; Toggle/Double Tap
modifier-only activation is deferred until a standalone release, and intervening
keyboard events invalidate it. That is not the Ctrl+Option+D path.
[Monitor state and modifier-only handling][v-modifiers].

Consequences and limits:

- A **missed D-up in VoiceInk's own monitor** can leave `isDown` true; later
  matching downs are then suppressed until release/reset. This follows directly
  from [key transitions][v-key-transitions]. It is a possible failure mechanism,
  **not an attribution of the observed stale macOS D-down**.
- A stale macOS HID/session key table is not directly consulted by 2.21's key
  matcher. It might affect upstream delivery/flags, but no live test established
  that causal chain. The reported key-up-only clearing and original source of
  the stale down remain user/session evidence, not findings of this audit.
- Tap-disable recovery in 2.21 dispatches release callbacks for down shortcuts
  before re-enabling the tap. Main instead adds richer reset/recovery behavior.
  Neither proves that a particular physical release reached the app.
  [Release-era reset][v-monitor], [main recovery][m-recovery].

### Relevant differences on main

- Hybrid remains **500 ms**, Double Tap **700 ms**; the **500 ms cooldown is
  gone**. Toggle now explicitly toggles on every accepted down. Main validates
  nonnegative release timing and clears pressed timing before awaits.
  [Main handler][m-handler].
- Monitor runs on a dedicated shortcut thread; duration uses CGEvent's
  nanoseconds-since-boot timestamp, falling back to uptime when zero. It
  explicitly ignores auto-repeat and recovers fresh non-repeat downs after a
  missing up. It checks/rebuilds disabled taps on a **10 s** timer and on
  lifecycle changes. [Main monitor][m-monitor], [tap implementation][m-tap].
- Main adds `Shortcut.isCurrentlyPressed`, reading modifier/key/button state
  from **`.combinedSessionState`**, not `.hidSystemState`. Handler takeover and
  recovery now depend on those observations. A stale combined-session D-down
  could therefore affect those particular decisions on main; that is **not**
  evidence that installed 2.21 did so. [Main state queries][m-key-state],
  [main handler][m-handler].
- Main sets engine `.starting` before the permission callback and adds
  session-bound held-recording completion on monitor reset. These are material
  startup/recovery changes, not evidence that upgrading solves the observed
  mismatch. No upgrade or live comparison was performed. [Main startup][m-engine],
  [held completion][m-held].

## Shipped integration inventory

Audit scope: public application Swift sources, plist/project configuration,
entitlements, shared XPC protocol, and official docs at the pinned release-era
revision and main. “Not found” below is a scoped source finding, not a promise
that no undocumented behavior could exist in a binary/dependency.

| Interface | What actually exists | State/control usefulness |
|---|---|---|
| App Intents / Shortcuts / Siri | `ToggleMiniRecorderIntent` and `DismissMiniRecorderIntent`; `openAppWhenRun = false`. Start **and stop** Siri phrases map to the **same toggle** intent. Dismiss cancels active work. Implementations unchanged between inspected release and main. [Intents][v-intents], [toggle][v-toggle-intent], [dismiss][v-dismiss-intent]. | Real shipped control entry point. No idempotent Start, idempotent successful Stop, state query, recording boolean, or state stream. Returned dialog confirms posting a request, not audio capture success: intent posts NotificationCenter and returns; UI work is asynchronous. |
| URL scheme / deep link | No custom scheme registration found in plist/project configuration. AppDelegate's `open urls` handler routes supported media to file transcription, not recorder commands. [Plist][v-plist], [AppDelegate][v-appdelegate]. | Do not invent `voiceink://start`, `/stop`, or `/status`. Opening a media file is not live recording control. |
| AppleScript / scripting dictionary | No `.sdef` or scriptable recorder command surface found. Apple-events usage permission is for browser context. [Plist description][v-plist]. | `osascript` or UI scripting is not a VoiceInk recording API. No supported state getter established. |
| VoiceInk CLI / local HTTP / socket server | No inbound recording-control CLI, HTTP listener, or Unix socket endpoint found. “Local CLI” runs an external command as an **enhancement provider**. [Official Local CLI docs][docs-cli], [provider source][v-cli]. | Not an external `voiceink record/status` facility. |
| XPC | Bundled Refine service exposes `prepare`, `enhance`, `shutdown`. [Shared protocol][v-xpc]. | Internal model inference service, not recording state/control. Network-server entitlement alone is not proof of an API. |
| NotificationCenter / Combine | `.toggleRecorderPanel`, `.dismissRecorderPanel`, transcription notifications, `@Published recordingState`, and `recordingStatePublisher`. [Notification names][v-notifications], [engine state][v-engine], [UI publisher][v-ui-state]. | **In-process** mechanisms; another process posting the same NotificationCenter name will not control VoiceInk. No external recording publication found. |
| DistributedNotificationCenter | None found in release-era application source. Main observes Apple's screen-lock/unlock notifications. [Main lock observers][m-session]. | Those are incoming session signals, **not** VoiceInk recording broadcasts. |
| App-group files / UserDefaults | No app-group recording-state export or shared-suite state store found. Standard UserDefaults stores shortcut/configuration data; `recordingState` is runtime state. [Shortcut store][v-store], [entitlements][v-entitlements], [engine][v-engine]. | Reading shortcut preferences cannot tell whether recording is active. No supported state file contract found. |
| Custom Command output | Shipped per-Mode delivery hook, after completed transcription and optional enhancement. Final text goes to stdin and `VOICEINK_TRANSCRIPT`; `/bin/zsh -lc`, 10 s timeout, panel dismissed before command. [Official docs][docs-commands], [delivery][v-delivery], [runner][v-command-runner]. | Real external **result-delivery** integration, not start/stop/cancel callbacks. Misses cancellation, failed starts, and stop-to-transcription delay; cannot be authoritative recording feedback. Changes output behavior and exposes final text to the command. |
| Logs / audio-device observation | Main logs state transitions; UI visibility and capture are distinct. [Main state logging][m-state-log], [UI state][v-ui-state]. | Diagnostic evidence, not a stable state protocol. Generic “microphone in use” is not VoiceInk-specific or equivalent to its recording lifecycle. No logs/transcripts were read here. |

State meanings matter: `RecordingState` contains `idle`, `starting`, `recording`,
`transcribing`, `enhancing`, `busy`. Engine sets `.recording` after awaited
hardware start, but sets `.transcribing` **before** awaited hardware stop; it is
application lifecycle state, not a sample-exact hardware-running flag. Panel
visibility can persist during processing or assistant interaction, and is set
before startup completes. [Enum][v-state-enum], [engine sequencing][v-engine],
[UI control][v-ui-control]. `ShortcutRecorderModel.isRecording` refers to
**capturing a shortcut assignment**, not microphone recording.
[Shortcut assignment recorder][v-shortcut-capture].

## Minimal alignment paths

1. **Keep hybrid: change Beckon timing only, after authorization.** Set the
   dictation action's `tap_ms` to **500**. Beckon's short predicate is `< tap`,
   so exactly 500 ms becomes long, matching VoiceInk's `>= 0.5` boundary.
   Today a 300–499 ms press clears Beckon's latch while VoiceInk can remain
   hands-free. [Template][b-template], [switch logic][b-switches]. No VoiceInk
   source change is needed. Limits remain: clocks/delivery latency differ,
   2.21 ignores rapid presses under its cooldown, startup can fail/lag, and
   keyboard/UI changes are invisible to Beckon's latch. Do not label that light
   “actual recording”.
2. **Prefer dedicated secondary PTT if pad hold-only behavior is acceptable.**
   Preserve primary Ctrl+Option+D Hybrid for the keyboard. With explicit user
   permission, assign a different unused secondary chord in VoiceInk's supported
   UI, choose Push to Talk, and configure Beckon's grouped pad action as `hold`
   with that chord and held-only feedback. This uses shipped configuration; no
   custom VoiceInk build. It removes hybrid timing and pad latch inference, but
   2.21's shared cooldown/active-action gate remains. Avoid overlapping pad and
   keyboard gestures; releasing PTT can stop a recording already started by the
   keyboard. Held red should mean **pad held**, not verified capture.
3. **Use Shortcuts for request-based toggle/cancel automation, not feedback.**
   A user-created Shortcut wrapping the shipped intents is a legitimate control
   route if a later Beckon action adapter is wanted. It does not supply PTT
   start/end semantics or a capture-state acknowledgement; do not infer LED
   truth from the returned dialog. No Shortcuts were created or executed.
4. **For a truthful LED across every input source, obtain actual state.** No
   shipped supported feed was found in either inspected revision. A supported
   state query/publication would need a VoiceInk-side upstream feature (or a
   separately researched noninvasive OS observation strategy with clearly
   weaker semantics), plus Beckon consumption. A custom build is **not** the
   default recommendation and is not authorized here. Equal durations alone
   cannot observe a keyboard stop: Beckon receives no corresponding event and
   its local latch does not change.

## Evidence boundaries

Current Beckon baseline was inspected at
`391f2ba99398a2961e00bcbb6a4ebf22c8889f5c`: one grouped control across AG16/AG17
on Rex position 3 and test position 6, immediate chord down/last-member up,
300 ms tap/hold inference, solid held red/breathing latched red. Preference and
permission facts supplied by the parent session were not re-read or changed.
The observed desynchronization is consistent with the proven threshold mismatch
and missing state feedback; the cause of any individual missed interpretation
or stale D-down remains unproven.

### Pinned sources

[release-diff]: https://github.com/Beingpax/VoiceInk/compare/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3...36abef95e685d26d383ce827f41471f5ee6e20af
[v-project]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk.xcodeproj/project.pbxproj#L625-L693
[v-constants]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/RecordingShortcutManager.swift#L309-L405
[v-down]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/RecordingShortcutManager.swift#L373-L429
[v-up]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/RecordingShortcutManager.swift#L431-L511
[v-manager]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/RecordingShortcutManager.swift#L21-L238
[v-monitor]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/ShortcutMonitor.swift#L99-L196
[v-key-transitions]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/ShortcutMonitor.swift#L303-L332
[v-modifiers]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/ShortcutMonitor.swift#L365-L453
[v-interrupt]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/ShortcutMonitor.swift#L455-L497
[v-match]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Models/Shortcut.swift#L85-L143
[v-normalization]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Models/Shortcut.swift#L410-L415
[v-session]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Infrastructure/SystemIntegration/Lifecycle/UserSessionInputPolicy.swift#L4-L31
[v-recorder]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Recording/Capture/Recorder.swift#L18-L142
[v-escape]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/RecorderPanelShortcutManager.swift#L158-L180
[v-mode-shortcuts]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Coordination/ModeShortcutManager.swift#L65-L128
[v-validator]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/State/ShortcutValidator.swift#L27-L97
[v-ui-control]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Recording/Presentation/RecorderUIManager.swift#L222-L249
[v-ui-state]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Recording/Presentation/RecorderUIManager.swift#L251-L338
[v-engine]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Recording/Workflows/VoiceInkEngine.swift#L95-L279
[v-state-enum]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Core/Recording/RecordingState.swift#L3-L10
[v-intents]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/App/AppIntents/AppShortcuts.swift#L4-L33
[v-toggle-intent]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/App/AppIntents/ToggleMiniRecorderIntent.swift#L5-L18
[v-dismiss-intent]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/App/AppIntents/DismissMiniRecorderIntent.swift#L5-L18
[v-plist]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Info.plist#L4-L52
[v-appdelegate]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/App/Lifecycle/AppDelegate.swift#L31-L60
[v-cli]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Infrastructure/Providers/Enhancement/LocalCLI/LocalCLIService.swift#L35-L95
[v-xpc]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/Shared/VoiceInkRefineXPCProtocol.swift#L31-L43
[v-notifications]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/App/Notifications/AppNotifications.swift#L3-L27
[v-store]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/State/ShortcutStore.swift#L3-L50
[v-entitlements]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/VoiceInk.entitlements#L4-L38
[v-delivery]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Recording/Workflows/TranscriptionDelivery.swift#L83-L118
[v-command-runner]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Infrastructure/SystemIntegration/Processes/CustomCommandDeliveryRunner.swift#L5-L16
[v-shortcut-capture]: https://github.com/Beingpax/VoiceInk/blob/640b0c8c36ee74d9e76d930f40e2c04bb739b4b3/VoiceInk/Features/Shortcuts/Views/ShortcutRecorder.swift#L161-L188
[m-handler]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Shortcuts/Coordination/RecordingShortcutManager.swift#L340-L667
[m-monitor]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Shortcuts/Coordination/ShortcutMonitor.swift#L186-L580
[m-recovery]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Shortcuts/Coordination/ShortcutMonitor.swift#L364-L432
[m-tap]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Shortcuts/Coordination/ShortcutEventTap.swift#L17-L57
[m-key-state]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Shortcuts/Models/Shortcut.swift#L31-L49
[m-engine]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Recording/Workflows/VoiceInkEngine.swift#L258-L280
[m-held]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Recording/Presentation/RecorderUIManager.swift#L266-L291
[m-state-log]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Features/Recording/Workflows/VoiceInkEngine.swift#L95-L100
[m-session]: https://github.com/Beingpax/VoiceInk/blob/b5d1be9a7c7377dd059f37bb3e0f791b802d4001/VoiceInk/Infrastructure/SystemIntegration/Lifecycle/UserSessionInputPolicy.swift#L67-L85
[b-template]: https://github.com/tmcinerney/beckon/blob/391f2ba99398a2961e00bcbb6a4ebf22c8889f5c/templates/creator-micro.toml#L63-L80
[b-switches]: https://github.com/tmcinerney/beckon/blob/391f2ba99398a2961e00bcbb6a4ebf22c8889f5c/src/switches.rs#L117-L187
[b-chord]: https://github.com/tmcinerney/beckon/blob/391f2ba99398a2961e00bcbb6a4ebf22c8889f5c/src/chord.rs#L50-L83
[docs-shortcuts]: https://tryvoiceink.com/docs/shortcuts
[docs-commands]: https://tryvoiceink.com/docs/custom-commands
[docs-cli]: https://tryvoiceink.com/docs/local-cli
