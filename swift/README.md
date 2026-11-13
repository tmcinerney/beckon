# Native Configuration tab

macOS 13+ SwiftUI client for Beckon's newline-delimited Unix-socket protocol.
Launch is disconnected. Connecting is an explicit user action. The client never
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

`BeckonConfigurationUI.ConfigurationView(model:)` is also a reusable tab body.
The standalone app hosts it in a native `TabView`. Views use labelled native
pickers, text fields, sliders, colour pickers, disclosure groups and buttons.

## Ownership and protocol

- Endpoint discovery follows `src/main.rs` and `src/state.rs`: `XDG_RUNTIME_DIR`,
  otherwise `XDG_STATE_HOME/beckon`, otherwise `HOME/.local/state/beckon`, with
  the daemon's temporary-directory fallback if HOME is absent. The endpoint can
  be edited; reconnect applies it. No directories or sockets are created by the client.
- Each exchange uses one socket, a bounded deadline and one newline-terminated
  JSON response. Disconnect cancels the owned request; generation fencing prevents
  late responses from replacing the current snapshot. Mutations are never retried.
- `config_model` and `status` must both decode before a new snapshot is published.
  Last-known data stays visibly stale on failure or disconnect. Reads are manual;
  after 15 seconds edits require a refresh. These two reads are not a daemon-side
  transaction and do not establish hardware or source-cache freshness.
- Basic mutations at core `88a2a2f`: `set_theme` and explicit source-qualified
  `bind`. Guarded presentation/release use the contract from parent's `391f2ba`,
  inspected read-only without merging Rust into this Swift branch.
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

`device_effect` replaces the named effect's three states together, including
explicit JSON nulls for disabled states. `device_channel` addresses an advertised
device channel. Native colour pickers, hex fields, brightness sliders and animation
pickers edit local drafts; only Save sends IPC. Discard restores the original values.
The UI captures the draft's revision and resets unsaved drafts when the effective
revision changes. The model rejects stale drafts, invalid treatments and unavailable
targets before dispatch. The daemon rejects revision conflicts and persistence errors.
Failed writes retain the old displayed model and never announce success or retry.

Successful `set_presentation` returns the full canonical model; the client publishes
that response directly rather than its draft. This does not refresh agent status's
timestamp. Old daemons continue to support basic reads/theme selection/bind while
appearance Save and guarded Release stay unavailable.

## Current integration gaps

1. Parent must integrate the Swift commits with core `391f2ba` or its successor
   before new appearance/release operations are available. These operations have
   been checked using isolated fixtures; the live service was not touched.
2. `status.bindings` reports live bindings, not the entire ledger. Missing rows
   are labelled “No live owner reported”, not “free”. A dormant owner can still
   cause the daemon to reject a bind. UI release requires an observed owner; dormant
   release remains a separate explicit CLI operation until status exposes the ledger.
3. There is no live device connectivity/interaction-state endpoint. The UI shows
   configured mappings and feedback definitions only. Held/latched lights describe
   inferred state, not proof of a receiver's state. `reset_controls` exists but is
   intentionally not exposed here because it releases output, beyond configuration.
4. Runtime UI/accessibility inspection and integration with a deployed daemon are
   not part of isolated verification. Build/tests do not claim physical LED success.

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
