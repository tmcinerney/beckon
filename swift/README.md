# Native Beckon companion

macOS 13+ SwiftUI client for Beckon's newline-delimited Unix-socket protocol.
Launch is disconnected. Connecting is an explicit user action. Devices,
Configuration and the accessible menu-bar summary share one model. The client never
opens HID, executes source commands, writes TOML, or focuses an agent.

## Build and isolated checks

From the repository root:

```sh
./swift/run-swift build
./swift/run-swift test
```

`run-swift` uses the selected Xcode's `xcrun swift` and macOS SDK, clearing the
Nix Rust/C SDK environment locally. It does not change global developer settings.
The repository's `devenv.nix` supplies Rust, not Swift; this native wrapper is
selectively reused from the prototype. Tests use mocks and temporary Unix sockets,
never the discovered daemon endpoint. No app launch is required for these checks.

For an explicitly authorized interactive session:

```sh
./swift/run-swift run beckon-configuration
```

Before leaving a new build running, use the bounded native startup regression
in an attended macOS GUI session (after `./swift/run-swift build`):

```sh
bash swift/check-startup "$PWD/swift/.build/out/Products/Debug/beckon-configuration" /absolute/path/to/new-log-prefix
```

This launches one disconnected instance for roughly 6–10 seconds, records CPU/RSS,
fails on sustained CPU above 70% or RSS above 512 MiB, and stops only its own child.
It never connects, clicks controls, or changes the daemon/device. Limits are sampled,
not OS-enforced memory quotas. This catches the native `TimelineView`/`MenuBarExtra`
label startup loop observed on macOS 27; the label now uses a presentation-only
one-second timer. Unit tests alone do not exercise that native launch path.
A passing resource probe is **not** proof of visible UI, VoiceOver, or live hardware
acceptance; verify the window/menu and stable idle usage separately before rollout.

`ConfigurationView(model:)`, `DeviceStatusView(model:)`, and
`MenuHealthView(model:openMainWindow:)`
are reusable views. The standalone app hosts native tabs and a window-style
`MenuBarExtra` (macOS 13+, unchanged minimum OS). Views use labelled native
pickers, text fields, sliders, colour pickers, disclosure groups and buttons.

The primary scene is a value-presenting `WindowGroup` (`beckon-main`) containing
Devices and Configuration. Its private `Codable, Hashable` token has one case,
`main`, also supplied as the default value. The popup's **Open Beckon…** action
passes that explicit value and ID: SwiftUI brings an existing equal-value window
forward, or opens one if absent. The standard New command is removed. A constant
default value alone is **not** a guarantee against duplicates from restoration or
other window-creation routes; those remain native acceptance checks.

WindowGroup-primary lifetime keeps the app running after all companion windows
close. A binding-based `MenuBarExtra` uses app-local `@State menuInserted = true`;
closing the window does not change that binding. If the user removes the menu
extra, SwiftUI sets the binding to false. No insertion preference is persisted.
This is not a menu-only/Dock-hiding or no-window-on-launch policy.

The native development command launches an unbundled SwiftPM executable. A measured
startup reported AppKit's `prohibited` activation policy even though the window
rendered; Apple documents that policy as the unbundled/no-Info.plist default. The
app delegate now requests `.regular` once before launch completes and reports a
rejected policy change to stderr. The explicit Open action requests app activation
after routing to the canonical window (`activate()` on macOS 14+, the legacy API on
macOS 13). No periodic focus stealing, activation retry, other-app enumeration or
window-manager configuration change is involved. Activation remains an OS request,
not an immediate or guaranteed foreground result; attended confirmation is required.

For an explicitly authorized diagnostic session, prefix the normal launch/probe
command with `BECKON_FOREGROUND_DIAGNOSTICS=1`. Opt-in stderr lines tagged
`[DEBUG-beckon-foreground-20261116]` report only event, this app's activation policy
and `isActive` at launch, main-scene appearance, activation changes and Open events.
They contain no endpoint, configuration, window contents or other-app data. The
set-regular event records the returned Bool. Diagnostics are off by default and
perform no activation themselves. A post-request inactive sample can precede a later
activation notification; it is not by itself a foreground verdict.

The app injects the open action into the menu view, so reusable UI code does not
know the scene identifier or token. Both scenes retain the same app-owned model;
opening/reopening the window never connects, starts another read loop, or changes
daemon/device state. The action is available while disconnected or busy,
independently of Connect. Window content may be recreated after closing; unsaved
editor drafts are not promised to survive without specific evidence.

In the next explicitly attended session, verify the main window, repeated opening,
close/reopen with the menu/app still available, absence of standard New, restoration,
keyboard access and VoiceOver label/help, plus menu open/closed timer freshness and
stable idle usage. The lifecycle and same-value reuse above follow SwiftUI's documented
contracts; these native behaviors are **not** established by the model unit tests or a
successful build. No native acceptance is claimed here.

## Ownership and protocol

- Endpoint discovery follows `src/main.rs` and `src/state.rs`: `XDG_RUNTIME_DIR`,
  otherwise `XDG_STATE_HOME/beckon`, otherwise `HOME/.local/state/beckon`, with
  the daemon's temporary-directory fallback if HOME is absent. The endpoint can
  be edited; reconnect applies it. No directories or sockets are created by the client.
- Each exchange uses one socket, a bounded deadline and one newline-terminated
  JSON response. Disconnect cancels the owned request; generation fencing prevents
  late responses from replacing the current snapshot. Mutations are never retried.
- Explicit Connect bootstraps `config_model` capability discovery. With
  `runtime_snapshot_versions:[1]`, `{"op":"runtime_snapshot","version":1}`
  returns one cached sample containing the complete configuration, ledger, source
  and device observations. Its configuration revision must match the embedded
  model. It is coherent per sample, not a filesystem or multi-source transaction.
- **Old daemon safety:** old `status` can reconcile/write. Automatic fallback is
  `config_model` only; legacy status is read only when `status_read_only:true` is
  explicitly advertised, and is never polled. Missing capability disables health
  and complete-ledger claims rather than calling old status or inferring defaults.
  A runtime protocol error latches runtime unavailable until explicit Reconnect;
  config-only display/sampling remains available. No error-string parsing.
- One model-owned cancellable loop starts only after explicit Connect. Named
  `clientRefreshInterval` is 2 seconds **between completed attempts**, not a device
  hold threshold. No overlapping reads/writes, catch-up bursts, automatic reconnect,
  startup polling, or second loop from opening a tab/window/menu. A user mutation
  or Refresh cancels/drains the owned poll before dispatch. Disconnect invalidates
  generation; Reconnect drains old requests before contacting the new endpoint.
- Last-known data remains visibly stale on failure/disconnect. Instance changes,
  lower sequences and mismatched configuration revisions fence publication or
  editing. Receiving the same cached sequence never refreshes its evidence age.
  Device/source/layer/label freshness adds producer observation age, sample age
  and client elapsed time; null evidence stays unknown (v1 upper bound 15000ms).
- Background sampling does not renew the 15-second explicit editing-verification
  deadline, reconcile drafts, clear unknown write outcomes or retry mutations.
  Use explicit Refresh before editing. A pre-write cached sample cannot verify
  an acknowledged change; a newer sample is required. Appearance acknowledgements
  remain authoritative while a cached runtime sample catches up to that revision.
- Basic mutations at core `88a2a2f`: `set_theme` and explicit source-qualified
  `bind`. Guarded presentation/release use the contract from parent's `391f2ba`,
  subsequently cherry-picked by explicit parent authorization (`78589fe` here).
  Typed acknowledgements are checked before readback.
  Failure never changes the displayed model optimistically. A transport failure
  after sending a write reports an unknown outcome; failed readback reports an
  acknowledged but unverified change.
- Binding names/order, device instances, controls, actions, members and effects
  come from the configuration model. No fixed capacity, F-key or AG placement is
  assumed. Adapter options remain typed JSON extension data for read-only inspection.
- The UI refuses occupied keys and refuses silently moving an already-bound agent.
  Selection identifies source + session/scope + pane ID, never a title alone.
- Release captures key + exact reference + endpoint when confirmation opens.
  `release_binding` sends `{key,reference:{source,session,pane_id}}`; the reply is
  `{key,changed}`. The model checks the captured selection locally and the daemon
  checks the owner atomically. The Swift client has no `release_key` request.

## Devices, legend and health

- Device rows show observed connectivity/layer only when supplied, with independent
  freshness, configured control/member names or exact-address fallbacks, action
  legends, feedback and recent typed diagnostics. No fixed key/layer/source count.
- The complete legend shows configured keys plus orphaned ledger keys, exact
  source + scope/session + pane/block identity, actual workspace/task labels and
  live/dormant/unknown/free status. Cached labels are visibly stale. Missing
  structured labels say unavailable alongside exact IDs; no title parsing,
  renaming, native badge writes, or invented Delta agent source.
- Only an authoritative ledger can report a key free. Dormant owners stay occupied;
  only fresh live pane/source observations are bind candidates. Legacy live-only
  status never establishes complete ledger ownership or hardware health.
- Menu-bar symbols have accessible text/value and a summary distinguishing
  disconnected, stale, unavailable/degraded, observed availability and scoped active
  errors. Neutral device counts do not assume all configured devices are required.
  An unavailable device does not turn another device's connected row into a fault.
  Resolved error history is distinct from active errors; unknown diagnostics are
  unsupported, never green. Socket reachability alone is not device/permission health.
- `tap-hold-chords` decodes `tap_keys`, `hold_keys`, explicit positive `hold_ms`
  without mandatory legacy `keys`/`mode`. Its legend explains one-shot tap/hold
  output and no tap on held release. Unknown action/feedback variants are
  inspect-only. Behavioral timing/output stays read-only device action TOML;
  no structural-edit or presentation-settings timing writer was introduced.
- Diagnostics use closed typed code/recovery values and fixed client text, not
  external stderr/argv. Permission/cleanup guidance requires attended action;
  the client never requests permission or retries key cleanup itself.

## Guarded appearance editing

The daemon's `config_model` advertises an opaque `revision` and typed capabilities:

```json
{
  "presentation_changes": ["theme_state", "device_effect", "device_channel"],
  "binding_release_guard": true,
  "device_channels": {"configured-device-id": ["base", "ring"]}
}
```

The values above are an example, not a fixed list of instances or channels.
Controls are enabled only for advertised changes and existing targets; an absent
capability disables writes. Channel treatment values are decoded from
`devices[].definition.options[channel]` (verified against the current adapter).
Unknown/unreadable channels remain unavailable instead of receiving invented defaults.

All appearance writes send exactly:

```json
{"op":"set_presentation","expected_revision":"opaque-value","change":{"kind":"theme_state","theme":"configured-theme","state":"working","treatment":{"color":"#123456","brightness":0.6,"motion":"breathe"}}}
```

Legacy `device_effect` replaces only three-state targets and is blocked on extended
targets. Advertised `device_effect_v2` replaces all six states (`idle`, `held`,
`latched`, `pending`, `hold_fired`, `error`) with explicit null clearing. Error
feedback includes its treatment and positive `duration_ms` (presentation only).
Unsupported fields remain inspect-only, never silently dropped by a legacy Save.
`device_channel` addresses an advertised
device channel. Native colour pickers, hex fields, brightness sliders and animation
pickers edit local drafts; only Save sends IPC. Discard restores the original values.
The UI captures the draft's revision and retains unsaved drafts when the effective
revision changes. It displays the new saved value alongside the draft and requires
an explicit “Keep draft against current revision” or “Discard draft / use current”
choice. A conflict never silently refreshes/retries a write or adopts a new revision.
The model rejects stale drafts, invalid treatments and unavailable
targets before dispatch. The daemon rejects revision conflicts and persistence errors.
Failed writes retain the old displayed model and never announce success or retry.
If the daemon explicitly reports that settings were saved but display refresh failed,
that message is preserved, the snapshot becomes stale and edits remain blocked until
an explicit refresh retrieves the authoritative saved model. The draft is retained.

Successful `set_presentation` returns the full canonical model; the client publishes
that response directly rather than its draft. This does not renew the explicit
editing-verification deadline. Old daemons support config-only display/theme
selection; binding requires safe observed candidates. Appearance Save and guarded
Release stay capability-gated.
The ring editor explains when state-following makes its colour a fallback.
Success notices explicitly confirm host state, not physical LED observation.

## Current integration gaps

1. This branch includes core `391f2ba` via authorized cherry-pick. The parent still
   owns integration/release/deployment and production remains v2 until coordinated
   migration. These operations have
   been checked using isolated fixtures; the live service was not touched.
2. `Fixtures/RustProducerV3/` now contains ten byte-identical actual Rust serializer
   envelopes, with original manifest/source provenance. Dedicated producer tests
   verify bundled SHA-256 identities and exercise typed decoding, model/projections,
   read-only polling and fragmented isolated sockets. The config_model bootstrap
   adapter is derived from the actual configuration, not claimed as an exact Rust
   bootstrap wire fixture. Existing `SyntheticRuntimeFixture.swift` / synthetic
   transports remain separately labelled consumer scenarios, not producer evidence.
   This software replay does not replace deployed-daemon or physical acceptance.
3. Old daemons lack complete ledger/device diagnostics; display is honestly
   unavailable or stale. `reset_controls` is intentionally not exposed: it
   releases output, beyond read-only inspection/configuration.
4. Native visual/VoiceOver inspection, deployed-daemon compatibility, actual device
   layers/LEDs, receiver behavior and permission/hardware acceptance are NOT RUN.
   Build/tests do not prove physical behavior, signed/portable packaging, or issue
   completion. No app/daemon launch, live socket or hardware access is needed for
   isolated verification. Packaging/rpath work remains separate.

## Reuse audit

Read-only source: sibling `tmcinerney-control-plane-prototype`, committed head
`92ba792`, including current staged files (six files, 79 insertions / 7 deletions
at inspection). That worktree was not modified.

| Prototype component | Decision |
| --- | --- |
| `swift/run-swift` | Retain native Xcode/SDK wrapper pattern. |
| `PanelSupport.c` token/deadline/socket functions | Extract socket-only helper into `SocketSupport`; retain bounded nonblocking I/O, cancellation, size cap and SIGPIPE protection. Drop process-spawning and cleanup machinery because this client has no subprocesses. |
| `Transport.swift`, socket tests | Reuse cancellation-handler/detached-I/O and fragmented fixture patterns. Replace Herdr `agent.list`, pilot/status-slots and checked-focus requests with typed current Beckon operations. |
| SwiftUI app/panel scaffolding | Retain native window, observable model and reusable view approach; expose a Configuration tab. No dependency on old panel state persistence. |
| Codable identity/freshness concepts | Retain exact identity, last-known observations and generation fences; replace six-slot/native-session models with the v3 contract. |
| Trellis/coordinator/focus screens and process runner | Leave intact in the prototype; not needed in a configuration client. |
| `integrations/creator-micro` physical/one-key pilots, config transfer, HID and six-slot renderers | Do not import: obsolete f4/AG03/six-slot assumptions and direct physical ownership conflict with the daemon-owned model. |
| Staged `OneKeyBackendProjection` and related view/presentation changes | Inspected current diff: toggles pilot/slots, retains f4 and fallback slot presentation. Useful explicit-protocol/freshness idea retained, incompatible wire types not imported. |

The source audit included `docs/configuration.md`, hardware notes, `device.rs`,
`config.rs`, `main.rs`, `core.rs`, `render.rs` and `state.rs`. SwiftUI controls and
state ownership were checked against current Apple documentation through Context7.
Canonical worker startup guidance was explicitly read from installed
`~/.config/agent-workflows/{role-contracts,native-startup,personal-profiles,session-continuity}.md`.
The assignment is the bounded implementation authority; no tracker task or board
authority was inferred.
