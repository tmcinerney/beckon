# Actual Rust producer-v3 fixtures — not synthetic Swift inputs

The ten `fixtures/*.json` files are BYTE-IDENTICAL copies of the immutable
`assets/backend-completion/producer-v3/tests/fixtures/` delivery for personal Beckon
task `I_kwDOT_7Thc8AAAABfYwBxA`. They include their original newline framing.
Do not pretty-print, normalize, hand-edit, or silently regenerate them.

`producer-manifest.sha256` is the exact original 16-entry manifest, SHA-256:
`8954064ff7869cbf0869f19dcf7c75fca619bf3b3a496a0b598c80ac723bded9`.
Its ten fixture entries identify the copied bytes. Its six source entries record
the immutable producer/test/serializer/engine provenance, not additional files
bundled here. The immutable delivery retains those source snapshots.

Producer source and replay:
- `tests/runtime_snapshot.rs` verifies exact serializer bytes with `assert_eq!`.
  Its real `Switches` engine plus fake emitter drives pending, hold_fired, posting
  denial/recovery and joined microphone held/latched phases. It also covers the
  base, invalid-ledger, stale-source/sample-age and unsupported-version replies.
- `src/runtime.rs` owns typed snapshot assembly and `reply_line`: real Serde JSON
  serialization plus one newline; unsupported versions use the real error envelope.
- `src/config.rs`, `src/device.rs`, `src/switches.rs`, and
  `src/display/runtime.rs` supply the configuration/action/effect/phase/evidence
  types. Exact source hashes are retained in the original manifest.
- Parent reports the 260-test backend full suite replayed these bytes in both its
  active and isolated reconstruction. Swift tests do not rerun or replace that
  producer proof, and never set `BECKON_UPDATE_FIXTURES`.

SwiftPM's `.copy` rule retains bytes and directory structure. Tests use
`Bundle.module`, verify the original manifest hash and every bundled fixture's
SHA-256, then use real Reply/RuntimeSnapshot decoding and model/projection paths.
Only the config_model bootstrap test envelope is derived by a Swift encoder from
the actual decoded configuration; it is NOT claimed as an exact Rust config_model
wire fixture. Every runtime/error response is the exact supplied producer bytes.

Existing SyntheticRuntimeFixture/ReviewFix transports remain explicitly separate
consumer-development scenarios. These actual fixtures prove software wire/data
compatibility only; no real HID/output/daemon/app, physical receiver, native UI or
VoiceOver acceptance is implied. Unknown hardware/permission evidence stays unknown.
