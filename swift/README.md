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
- Supported mutations at core `88a2a2f`: `set_theme`, explicit source-qualified
  `bind`, and `release_key`. Typed acknowledgements are checked before readback.
  Failure never changes the displayed model optimistically. A transport failure
  after sending a write reports an unknown outcome; failed readback reports an
  acknowledged but unverified change.
- Binding names/order, device instances, controls, actions, members and effects
  come from the configuration model. No fixed capacity, F-key or AG placement is
  assumed. Adapter options remain typed JSON extension data for read-only inspection.
- The UI refuses occupied keys and refuses silently moving an already-bound agent.
  Selection identifies source + session/scope + pane ID, never a title alone.

## Current integration gaps

1. Colour/brightness/motion mutation endpoints are absent at `88a2a2f`. Native
   treatment draft controls are explicitly local preview only; Save is unavailable.
   Device presentation and idle/held/latched treatments are read-only. No invented
   appearance requests are emitted. Parent owns the eventual capability/schema.
2. `status.bindings` reports live bindings, not the entire ledger. Missing rows
   are labelled “No live owner reported”, not “free”. A dormant owner can still
   cause the daemon to reject a bind. Explicit release remains available for every
   configured key so the user can release a known dormant binding.
3. `release_key` has no expected-owner/revision parameter. Confirmation describes
   releasing the current owner; another client can change ownership between the
   displayed snapshot and dispatch. An atomic conditional-release endpoint is
   needed to eliminate this race.
4. There is no live device connectivity/interaction-state endpoint. The UI shows
   configured mappings and feedback definitions only. Held/latched lights describe
   inferred state, not proof of a receiver's state. `reset_controls` exists but is
   intentionally not exposed here because it releases output, beyond configuration.
5. Runtime UI/accessibility inspection and integration with a deployed daemon are
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
