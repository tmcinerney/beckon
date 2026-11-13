# Configuration client IPC

The daemon owns configuration and hardware. Clients send one UTF-8 JSON line per
Unix-domain socket connection and read one JSON-line reply. No HTTP wrapper,
direct HID access or TOML rewriting belongs in the Swift client.

Endpoint: `$XDG_RUNTIME_DIR/beckon.sock` when set, otherwise
`$XDG_STATE_HOME/beckon/beckon.sock`, otherwise
`$HOME/.local/state/beckon/beckon.sock`. A user-selected endpoint is allowed;
connection success means socket connectivity, **not hardware availability**.

Replies are `{"type":"ok","data":...}` or
`{"type":"error","message":"..."}`. Treat errors as failures, retain unsaved
edits, and do not infer success from sending bytes. Unknown request fields are
rejected. Older daemons may not support these operations; show an upgrade/error
state instead of attempting another device-control path.

## Read the model

```json
{"op":"config_model"}
```

Success data contains:

- `config_version`: 3
- `revision`: opaque effective-configuration content token; do not calculate it
- `bindings`: ordered configured binding ids, not agent assignments
- `sources`: configured source ids
- `devices`: `{id,path,definition}` records; definition contains `adapter`,
  `options`, `keys`, `actions`, `effects` as documented in configuration.md
- `display`: `{theme,themes}` with complete five-state treatments
- `capabilities`: `presentation_changes` (`theme_state`, `device_effect`,
  `device_channel`), `binding_release_guard: true`, and `device_channels`, a map
  from instance id to channel names. Creator Micro currently exposes `base` and
  `ring`; adapters without mutable channels expose an empty list.

Ids and counts are data. Do not label all controls agents, assume F1–F10, hardcode
six slots, or derive AG addresses in UI code. Adapter options are read-only in
this API. In particular, a ring configured to follow state colour uses its
configured ring colour only as a fallback; communicate that distinction.
Creator Micro's per-layer `zone_lighting` is structural: base/ring edits apply
only on layers declaring `"beckon"`; `"firmware"` layers retain their native
zone lighting and still receive configured per-key feedback.

## Mutate presentation

Return the last model revision with a typed change:

```json
{"op":"set_presentation","expected_revision":"TOKEN","change":{"kind":"theme_state","theme":"status","state":"working","treatment":{"color":"#123456","brightness":0.6,"motion":"breathe"}}}
```

Change variants:

1. `theme_state`: `theme`, `state`, `treatment`. State is exactly `idle`,
   `working`, `blocked`, `done` or `unknown`.
2. `device_effect`: `device`, `effect`, `effects`. The effects object has optional
   `idle`, `held`, `latched` treatments (absent/null means fall through). At least
   one must remain defined; the effect must already exist in device configuration.
3. `device_channel`: `device`, `channel`, `treatment`. Use advertised channel names.

Each treatment has all three fields: hex `color`, `brightness` in `[0,1]`, and
`motion` (`steady`, `breathe`, `pulse`, `flicker`). Invalid names, ranges, kinds,
structural fields or stale revisions fail without changing settings or memory.
Successful data is the updated complete configuration model, including revision.
Persisted changes affect only settings.toml, not structural files or the ledger.
The reply confirms saved/applied host configuration, not physical LED observation.

Saving with a stale revision fails. Refresh and let the user reconcile edits;
do not silently retry a stale write over another client's changes. Refreshing
presentation does not clear a held chord, restart a latch or change key ownership.

Theme selection alone remains `{"op":"set_theme","name":"status"}`;
its success data is `{"theme":"status"}`. Fetch the model again afterward.

## Bindings

`{"op":"status"}` returns `bindings` (assignment plus full agent record) and
`panes` (source-qualified identities, resolved titles, current binding and state).
Timestamp successful client receipts; show stale/unavailable state on failed reads.
Read `src/main.rs` / `src/core.rs` for the complete status DTO fields.

An explicit UI assignment sends the exact selected identity and configured key:

```json
{"op":"bind","key":"key3","source":"rex","session":"SOURCE_SCOPE","pane_id":"SOURCE_ID"}
```

The daemon refreshes before validating the target. Occupied keys fail rather than
stealing their owner. Omitting key enables CLI first-free selection, but a UI
assignment must specify its chosen key and all source/scope/id fields. Success
returns `{key,session,pane_id,changed}`. Refetch status after a mutation.

Guard UI release against a stale owner:

```json
{"op":"release_binding","key":"key3","reference":{"source":"rex","session":"SOURCE_SCOPE","pane_id":"SOURCE_ID"}}
```

All reference fields are required. A mismatched or no-longer-present owner fails;
another agent is never released based on an old UI card. Success data is
`{key,changed}`. `release_key` remains an explicit CLI operation; do not use it
to bypass the UI owner check.

The client must not focus, bind, release or reset anything merely by displaying a
screen or reconnecting. Mutations require explicit user actions. `reset_controls`
is separate from binding release and does not promise to stop an external app.

## Testing

Use isolated Unix socket fixtures and fake emitters. Tests cover model transport,
strict decoding, colour-update success and stale-revision failure, validation
without partial writes, persistence/reload, held-state preservation, and stale
binding-owner rejection. Do not mutate the live ledger or open HID in UI tests.
