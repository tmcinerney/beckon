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

## Opt-in status/configuration panel

This MVP observes, configures six logical slots and previews identities. It does
not replace the Rust runtime, focus an agent, activate Ghostty, send text/approval
responses, open HID devices or write lighting/keymaps. No flag enables live focus.

Run through the project environment:

    devenv shell -- ./swift/run-swift test
    devenv shell -- ./swift/run-swift build --product beckon-control-panel
    devenv shell -- ./swift/run-swift run beckon-control-panel --demo --no-save

Next demo replays working, idle, done, blocked, unknown, disconnect/reconnect,
reorder and a coordinator pane mismatch. Demo edits never persist.

Source-only canary:

    devenv shell -- ./swift/run-swift run beckon-control-panel --herdr-socket /absolute/path/herdr.sock --config /absolute/path/panel.json --no-save

Only --demo, --no-save, --herdr-socket and --config are supported. Default paths
expand once under the current user's home: .config/herdr/herdr.sock and
.config/beckon/control-panel.json. No named/remote sessions are discovered.

For native macOS inspection, devenv shell -- ./swift/package-panel produces an
unsigned local .build/BeckonStatusPanel.app, installing nothing. Its executable
accepts the same options. Normal Quit exits only this panel and cancels/waits for
its bounded reads. SwiftPM package/SDK flags precede the run executable.

GUI acceptance is separate from sandboxed build/test execution. Run GUI commands
from an interactive macOS desktop terminal, or through a separately authorized
desktop launch. A restricted agent-shell acceptance launch aborted during
`_RegisterApplication` / `NSApplication` initialization; the same bundle succeeded
in the authorized desktop launch. This comparison does not identify the precise
OS denial. Retain the diagnostic on initialization failure; do not retry
automatically, change trust/permissions or infer a Herdr/HID failure.

### Assignment and authority

The panel owns a version-1 file containing six optional assignments, one socket
and up to six explicit boards. Deliberate edits save atomically; --no-save retains
changes in memory. Corrupt/unsupported files are preserved with editing disabled.
Existing Beckon/Input settings are untouched.

Assign lists all observed agents with exact harness/session identity; titles are
display labels. Missing/duplicate IDs cannot be assigned. Direct-native slots
survive array reorder and uniquely observed pane moves, without focus capability.

Coordinator selection requires explicit absolute Trellis executable, workflow
config, boards registry and board-config paths, plus alias, UID and ledger.
Select/refresh executes only paths, board-context and board-agents with pinned
argv and scrubbed routing environment. No agent inspect/read/send, provider
executable or open/focus operation runs.

Coordinator assignments retain authoritative registration/full Herdr bindings.
Board drift, changed provider/session/native identity, moved/missing surfaces and
ambiguity remain unavailable until reselected. Matched surfaces are labeled
"Live surface matched (provider command not revalidated)". Metadata refresh is
explicit and separate from status ticks.

### Bounded observation and cleanup

The visible panel polls agent.list every 2.5 seconds, one newline-framed Unix
socket request at a time. Deadline: two monotonic seconds; response cap: 1 MiB.
Failures discard state immediately and back off 1/2/4/8 seconds, capped at ten.
Source changes/disconnects invalidate previews. Assigned samples expire in ten
seconds; titles, pane order and foreground application never select the source.

Each shared panel view owns a stable observation ID. Multiple windows or native
tabs share one polling loop; repeated appearance and unknown/duplicate release
are harmless. Closing one view leaves surviving owners observing. Closing the
last visible view cancels the loop/read; reopening waits for that loop to retire
before starting another. Quit closes the model permanently and waits for owned
polling/read cleanup. The health header uses the same ten-second freshness rule
as the cards, including while no new sample arrives.

Working is blue pulse, idle white, done green, blocked amber, unknown/stale
neutral and unassigned off. Done means Herdr-reported done/unread (server seen
state), not task acceptance/universal unread. Blocked implies no structured
approval request or reason.

Preview serializes and fences generation, source, assignment and metadata around
fresh reads. It shows the exact identity and live-focus-unavailable message.
Cancellation/reassignment/source change/shutdown refuse delayed publication.

Metadata uses absolute executable/allowlisted argv, a new process group before
exec, stdin /dev/null, bounded concurrent pipe draining, three-second command
and eight-second shared operation deadlines. Overflow, cancellation, deadline
or exited roots retaining idle pipes trigger TERM/KILL only on the owned group.
Success requires the direct child reaped and its process group gone, including
descendants with redirected output. Detached/escaping providers are unsupported and not
invoked. Unconfirmed cleanup locks capacity until exit/reap evidence and explicit
Reconcile metadata cleanup. Quit is refused while supervision remains unresolved;
reconcile explicitly and Quit again. Quit never stops Herdr or other applications.

### Acceptance limits

Fixture coverage includes identities, config preservation on write failure,
socket framing/caps/timeouts/cancellation, inherited-child cleanup, shared
metadata deadlines, cancelled previews and demo replay. Cleanup-unconfirmed
lockout uses simulated supervision state; no destructive live reproduction.

Manual demo labels/colors, preview, disabled focus, reconnect, reorder and
coordinator mismatch were verified. A short nonpersistent source canary exposed
43 live agents (41 native identities) and an exact identity preview without
navigation or config writes. Two naturally occurring state transitions were not
observed: live transition acceptance remains unverified. Focus, hardware/LED
integration and runtime installation are deferred. A later bounded no-save
desktop pass also verified explicit metadata refresh and normal Quit. The assigned
agent stayed idle throughout; live transition acceptance remains unverified.

### Creator Micro personal slot convention

The agreed personal profile uses slot 1 for the Chief of Staff, slot 2 for the
selected project coordinator, slot 3 for releases, and slots 4–6 for explicitly
selected individual agents. Use coordinator references where appropriate and
direct-native assignments for selected workers. Unselected/missing roles remain
unavailable. This convention does not discover, create or take over coordinators.
The optional `slotLabels` array names all six cards and assignment-menu targets.
Omit it to retain generic Slot 1–6 labels. Labels are display guidance only: they
do not assign, discover or relink a session. Each label is nonempty, contains no
control characters, and is limited to 128 UTF-8 bytes.
The generic version-1 format retains six configurable slots; it contains no
four-fixed/two-dynamic split or hard-coded session IDs. Input owns the physical
profile, joystick navigation/tools, wheel and cheat sheet.

Optional mic feedback remains a capability assessment. The requested cue is a
slow red breathing pulse: confirmed capture when a supported observer supplies
that evidence, otherwise explicitly labeled button-held feedback after real
press/release verification. Native ChatGPT behavior stays unchanged. No VoiceInk
patch, shortcut synthesis or per-key writer is enabled. Per-key AG bindings and
controller handoff require review before replacing an existing microphone chord.

## Offline Herdr focus contract

`HerdrFocusController` models six deliberately accepted Herdr runtime assignments
(Chief of Staff, project coordinator, releases and three workers). It does not
select the original native Codex/Claude conversation. `HerdrFocusConfiguration`
is an ephemeral projection, not another settings file or agent registry. The
existing panel and hardware dispatch are not wired to this prototype.

Runtime, client and terminal-host ports prepare without effects, guard commit,
identify output commitment and retire both host focus queues before replacement.
Outcomes distinguish refusal, partial/unconfirmed selection and host selection
with identified output commitment; the latter is not GPU/pixel confirmation.
Default construction supplies unavailable ports. All functioning adapters live
in the test target and use in-memory state; no sockets or native focus adapter
are implemented. Production negotiation, authenticated host association and
native queue retirement remain separate feasibility gates.

Time inputs are nondecreasing monotonic milliseconds; requests have a maximum
30-second lifetime. The bounded in-memory transaction table retains refusals and
completed outcomes without eviction/re-execution, refuses when full, and marks
replays as historical. This is not durable or cross-process deduplication, nor
authentication of caller-supplied references. Owner reservations are contractual
requirements, not guarantees created by this controller. Unproved retirement
keeps upstream reservations and blocks new transactions until explicit
reconciliation; there is no automatic retry or rollback.

Run only the offline control suite, excluding the panel's real-socket tests:

    devenv shell -- ./swift/run-swift test --filter HerdrFocusContractTests
    devenv shell -- ./swift/run-swift test --filter BeckonControlTests

The deterministic fixtures exercise stable slot mapping, reused terminal IDs,
postcommit runtime/client replacement, both delayed host queues, partial
acknowledgements, replay and owner cleanup. They provide an emulator-like loop
without a virtual-pad app or device dependency. Micro Manager inspired the
shared mapping and explicit-unavailable patterns; no code or dependencies were
copied, and its list-order allocation and app-level Ghostty activation are not
used. Fixture success does not enable physical keys, LEDs or approval responses.
