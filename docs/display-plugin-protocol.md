# Display plugin protocol

Beckon can send its hardware-neutral render plan to trusted external programs.
This allows a status light, menu bar app, stream deck, accessibility tool, or
other display to integrate without linking to Beckon's Rust implementation.

## Configure a plugin

Each entry supplies a stable ID and an argv array. Beckon executes the program
directly; it never evaluates the command through a shell.

```toml
[outputs]
adapters = ["glove80-usb"]

[[outputs.plugins]]
id = "status-log"
command = ["/absolute/path/to/display-plugin-log.py", "/tmp/beckon-display.log"]
```

Built-in adapters and external plugins share one ID namespace. IDs must use
lowercase kebab-case and must be unique. `adapters` may be empty, and built-in
adapters and external plugins may run together.

Plugins are trusted local executables running with the user's permissions.
Only configure programs you trust. Relative executable names use the daemon's
`PATH`; absolute paths are more predictable for launchd and declarative Nix
installations. Arguments are not expanded, so `~`, environment variables, and
shell syntax remain literal text.

## Transport and lifecycle

Beckon starts one long-running child for each configured plugin after the first
render snapshot is available. The child receives UTF-8, newline-delimited JSON
on standard input:

1. One `hello` message when the child starts.
2. One complete `render` snapshot for the current state.
3. A new complete snapshot whenever the resolved render plan changes.

Standard output is discarded. Standard error is inherited by `beckond` and
therefore appears in the daemon's error log. Version 1 has no response or
command channel. A successful write means the child process accepted bytes
through its operating-system pipe; it does not acknowledge that a physical
display rendered them.

A plugin process that exits, cannot start, or stops accepting input is treated
as a failed display only. Beckon reports the error, waits before restarting the
child, and resends the latest snapshot. The daemon's hotkey loop and its other
display adapters keep running. Pending updates are coalesced to the newest
complete snapshot instead of forming an unbounded queue.

When Beckon shuts down, it closes the stream and gives the child a brief period
to exit before terminating it. Plugins should exit normally when standard
input reaches EOF.

## Protocol version 1

Every message names the protocol and version. A plugin should reject an
unknown protocol or unsupported version and ignore unknown fields within a
supported version.

```json
{"type":"hello","protocol":"beckon.display","version":1,"plugin_id":"status-log"}
```

Render messages contain a monotonically increasing sequence number and a full
render plan:

```json
{
  "type": "render",
  "protocol": "beckon.display",
  "version": 1,
  "sequence": 12,
  "plan": {
    "keys": [
      {
        "key": "f1",
        "state": "blocked",
        "color": "#F38BA8",
        "brightness": 0.8,
        "motion": "pulse"
      },
      {
        "key": "f2",
        "state": null,
        "color": "#00C48C",
        "brightness": 0.0,
        "motion": "steady"
      }
    ],
    "treatments": {
      "idle": { "brightness": 0.2, "motion": "steady", "color": "#3BA0FF" },
      "working": { "brightness": 0.6, "motion": "breathe", "color": "#F9E2AF" },
      "blocked": { "brightness": 0.8, "motion": "pulse", "color": "#F38BA8" },
      "done": { "brightness": 0.8, "motion": "steady", "color": "#A6E3A1" },
      "unknown": { "brightness": 0.3, "motion": "flicker", "color": "#6C7086" }
    }
  }
}
```

The actual plan always contains all ten logical keys. `state: null` means the
key is unbound; Beckon currently resolves that to zero brightness. Colors use
`#RRGGBB`, brightness is between `0.0` and `1.0`, and motion is one of
`steady`, `breathe`, `pulse`, or `flicker`.

The render plan describes an effect, not animation frames. A display plugin is
responsible for implementing motion locally and replacing its whole prior
state when a new snapshot arrives. Sequence numbers may be repeated after a
child restart, and reset when the daemon restarts, so consumers must not treat
them as durable or exactly-once event IDs.

## Safety boundary

The protocol is display-only. Plugins receive resolved state but cannot use
this channel to create or release bindings, focus panes, send keys, approve
tools, or answer an agent prompt. Those operations remain behind Beckon's CLI,
daemon IPC, and explicit action configuration.

The protocol schema is implemented in `src/display_protocol.rs`. Breaking
wire-format changes require a new `version`; additive fields may be introduced
within version 1 because consumers are required to ignore unknown fields.
