# Beckon Swift control-plane prototype

This package is an isolated policy/data integration prototype for the Swift backend rewrite. It does not replace beckond, register hotkeys, open HID, focus windows, answer approvals or mutate Trellis.

Run from the Beckon worktree:

```sh
devenv shell -- ./swift/run-swift test
devenv shell -- ./swift/run-swift run beckon-control-preview trellis /absolute/path/compact.json
```

The documented project shell supplies the repository environment; run-swift selects the native Apple Swift SDK and developer directory for this command. The current Rust environment has a deprecated nixfmt-rfc-style hook; this worktree uses ignored devenv.local.nix with git-hooks.enable = lib.mkForce false to prevent shared hook installation. That existing tooling issue remains separately tracked on chore/dev-tooling-fixes. No third-party Swift dependencies.

Feed a development build of Trellis's `task list --compact` JSON into the preview to verify the current native locators and claim fences. The decoder rejects runtime-verified recovery: stored native identity is not a live agent observation. Pagination metadata is retained; the caller must fetch subsequent pages.

The routing model consumes a version1 snapshot of fresh provider observations and version1 config. Native provider/session identity plus role/board association owns key routing. Configured board order controls board cycling; array/tab order does not choose role targets. Missing, contradictory, unsupported and stale targets are unavailable. The preview's route command prints an intent with dryRun=true; it never dispatches it.

Next Attention ranks approval, question, blocked and unread completion. This can focus unsupported providers but never manufacture a Yes/No capability. A request gate requires fresh exact identity, displayed structured request, unchanged target/request generation and choices, matching native focus, expiry and one-use consumption. An eventual native transport must verify these observations and use provider-owned request IDs to deduplicate delivery. In-memory consumption alone is not a persistent or cross-process approval guarantee.

Hardware prerequisites are prospective policy only. Supported controller handoff, firmware and durable active-profile backup must be established before any external driver runs. No active keymap, Enter/Escape, Herdr shortcut or installed runtime has been changed.

Before runtime cutover: native request/focus transports, SwiftUI configuration/status, existing Rust CLI/config/binding/surface parity, durable request-delivery receipts, exclusive hotkey ownership, device-specific adapters and emulator/live acceptance. Existing Rust executable and data remain the rollback. Micro Manager protocol ideas were researched; its source code is not copied here.
