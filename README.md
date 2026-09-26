# Beckon

Beckon binds selected Herdr agent panes to logical navigation keys and sends
their state to optional display integrations. Its current inputs support a
Glove80 and a MacBook function row; its first display integration adds
glanceable per-key state to a wired Glove80.

Licensed under the [MIT License](LICENSE).

## Current state

The current executable has the manual registration foundation:

- `beckon bind [--key f1]` binds `$HERDR_PANE_ID` through the local daemon.
  Repeating it without `--key` is idempotent: an already-bound pane keeps its
  existing key.
- `beckon release` clears the binding for the current pane; `beckon release --key f2`
  clears F2 from anywhere.
- `beckon status` shows bindings and their current Herdr pane data.
- `beckon listen-keys` records the ten Beckon-layer key events.
- `beckond` renders its live Herdr pane cache to zero or more independent
  display adapters. The optional Glove80 USB adapter reconnects automatically.
- `beckon preview` shows the declarative LED plan without writing keyboard HID
  frames; `beckon preview --all-states` shows every supported state treatment.

Bindings are persisted in `$XDG_STATE_HOME/beckon/bindings.json` (falling back
to `~/.local/state/beckon`). Herdr's `fkey` pane token is a visible mirror for
the sidebar, not the source of truth: Herdr does not restore token metadata
after its server restarts.

## Multiple Herdr sessions

Beckon manages every live Herdr session it finds: the default session's socket
plus each named session under `~/.config/herdr/sessions/<name>/herdr.sock`.
Stopped sessions leave their directories behind, so candidates are
liveness-checked and dead sockets are skipped. Bind, release, and status work
from any session's panes.

Pane identity is the pair `(session, pane_id)` because Herdr pane IDs are only
unique within one session. A pane ID that exists in exactly one managed session
resolves on its own; when the same ID exists in several, `beckon bind --session
<name>` (or `release --session`) selects one. Bindings for a session that is
not currently reachable are kept dormant rather than released, so restarting a
stopped Herdr session restores its keys instead of losing them.

```toml
[herdr]
# Pin the CLI for launchd shells whose PATH differs, point at a nonstandard
# default-session socket, or limit management to named sessions.
cli = "herdr"
socket = "/Users/you/.config/herdr/herdr.sock"
sessions = "auto"  # or ["default", "agent-workspace"]
```

The `BECKON_HERDR_SOCKET` environment variable still overrides the socket for
tests and one-off runs; `BECKON_HERDR_SESSIONS_DIR` overrides the discovery
directory.

## Terminal surface control

Navigation focuses the Herdr pane first; whether the terminal *window or tab*
displaying that session comes forward is a separate, explicitly configured
capability. With a backend enabled, Beckon can raise the adopted surface
before focusing the pane:

```toml
[terminal]
# "none" (default) keeps the focus command as the only surface mechanism.
backend = "ghostty-applescript"
```

Adoption is explicit and machine-local — Beckon never infers which surface
displays which session, never repairs a stale handle, and never opens windows
itself:

```sh
beckon terminals                      # list surfaces with opaque handles
beckon adopt --session agent-workspace --terminal BBD9B110-…
beckon forget --session agent-workspace
```

Adopted handles live in `surfaces.json` in the state directory (not in
configuration, which Home Manager may render), and each record names the
backend that produced it, so changing backends invalidates it honestly. A
missing record, a stale handle, or a changed backend warns and continues to
pane focus. Under launchd, the Ghostty backend needs an Automation (Apple
Events) grant on first use.

A keypress then runs, in order: the adopted surface raise, the `[focus]
command`, and the Herdr pane focus. The command receives `BECKON_KEY`,
`BECKON_PANE_ID`, `BECKON_HERDR_SESSION`, and `BECKON_TERMINAL_HANDLE` when a
handle was raised, so machine-specific scripts can compose instead of guess.
The design record is
[docs/terminal-backend-architecture.md](docs/terminal-backend-architecture.md).

## Optional MacBook function-key input

Beckon's logical slots (`f1` through `f10`) are separate from their physical
input source. The default `glove80` profile preserves the Glove80 Beckon layer:
F16-F20 and Shift+F16-Shift+F20. A MacBook can navigate the same bindings by
opting into its ordinary function-key row alongside the default Glove80 input:

```toml
[input]
profiles = ["glove80", "macbook-function-keys"]
```

On macOS Ventura or later, turn on **System Settings → Keyboard → Keyboard
Shortcuts → Function Keys → Use F1, F2, etc. keys as standard function keys**.
Without that setting, hold Fn/Globe while pressing a key. Fn/Globe then remains
the way to access the usual brightness, media, and volume controls. This is an
input-only profile: it focuses existing Beckon bindings, but does not provide
per-key status lighting. The Glove80 HID display can remain enabled separately.

When `[input]` is absent, Beckon enables only `glove80` for compatibility. An
existing single `profile = "macbook-function-keys"` setting remains supported.
Use `profiles` to enable both inputs. They intentionally target the same
logical bindings, so F1 on either keyboard focuses the pane bound to F1. Both
sources use macOS global shortcuts, so either works across apps, OmniWM spaces,
and a plugged-in desktop setup without switching modes.

## Optional display integrations

Inputs and displays are independent. The compatibility default enables the
wired Glove80 LED adapter when the `[outputs]` section is absent:

```toml
[outputs]
adapters = ["glove80-usb"]
```

The adapter is optional at runtime. Unplugging the keyboard is an expected
availability state: bindings, MacBook input, Herdr navigation, and other
display adapters continue working, and reconnecting the keyboard restores LED
updates. A navigation-only installation can explicitly select no displays:

```toml
[outputs]
adapters = []
```

Each built-in display implements the same hardware-neutral `DisplaySink`
boundary and receives a `RenderPlan`. Beckon fans out independently, so one
failed output cannot block another output or navigation. New built-in adapters
are added to the typed configuration registry.

External display plugins use a versioned, one-way NDJSON protocol rather than
an unstable Rust dynamic-library ABI. They run as supervised child processes,
receive complete render snapshots, and cannot block the hotkey loop or mutate
bindings. See [the display plugin protocol](docs/display-plugin-protocol.md)
and its [minimal logging example](examples/display-plugin-log.py).

## Declarative Nix configuration

The bundled Home Manager module can render the configuration rather than
managing `config.toml` by hand. Beckon owns the configuration version; set only
the fields you want to configure:

```nix
{
  programs.beckon = {
    enable = true;
    settings = {
      input.profiles = [ "glove80" "macbook-function-keys" ];
      outputs.adapters = [ "glove80-usb" ];
      outputs.plugins = [
        {
          id = "status-log";
          command = [ "/absolute/path/to/display-plugin-log.py" "/tmp/beckon-display.log" ];
        }
      ];
      focus.command = [ "/Users/you/.config/beckon/focus-ghostty" ];
    };
  };

  services.beckond.enable = true;
}
```

An empty `programs.beckon.settings` leaves configuration unmanaged, which is
the safe default for existing installations. The daemon logs the enabled input
profiles at startup; a registration conflict identifies the physical shortcut
and tells you that another application owns it.

## Input diagnostics

Beckond records the input receipt and final result in
`$XDG_STATE_HOME/beckon/key-events.log` (or
`~/.local/state/beckon/key-events.log`). For a complete per-stage trace while
diagnosing a shortcut, start the daemon with `BECKON_LOG=debug`. It writes
structured records for global-hotkey registration, event receipt, logical-key
routing, binding resolution, the configured terminal-focus command, and the
Herdr `pane.focus` request/response.

When Home Manager runs the service, those records go to its configured
`services.beckond.logDirectory` (by default,
`~/.local/state/beckon/logs/beckond.error.log`). A received raw macOS key event
that never produces a `received global hotkey event` record was consumed before
Beckon; use a temporary macOS Input Monitoring event tap to inspect that lower
layer. Beckon does not request that permission for normal navigation.

Create and validate the optional machine-specific configuration with:

```sh
devenv shell -- cargo run -- init
devenv shell -- cargo run -- config check
```

This creates `$XDG_CONFIG_HOME/beckon/config.toml` (falling back to
`~/.config/beckon/config.toml`) without overwriting an existing file.

For the current manual-bind spike, start the daemon then bind from a Herdr
pane:

```sh
devenv shell -- cargo run -- daemon
devenv shell -- cargo run -- bind --key f1
```

The daemon owns binding writes and global keypress navigation. If `focus.command`
is set, it runs before `herdr agent focus`; this is where a user integrates their
terminal and window manager. `preview` remains deliberately read-only: it
renders plans for inspection, not a simulated or real keyboard update.

## USB status transport diagnostic

The `v0.2.0-rc.1` Beckon firmware exposes a USB-only, status-only vendor HID
endpoint on the left (split-central) half. `beckond` uses the same strictly
selected endpoint for normal live status delivery. The diagnostic commands stay
explicit, for physical protocol checks only:

```sh
# Read-only discovery and open test.
devenv shell -- cargo run -- hid list
devenv shell -- cargo run -- hid probe

# A deliberate valid ten-key snapshot, F1 through F10.
devenv shell -- cargo run -- hid send --sequence 1 \
  --states idle,working,blocked,done,unknown,unbound,unbound,unbound,unbound,unbound

# Deliberately malformed 31-byte report; the firmware must ignore it.
devenv shell -- cargo run -- hid send-malformed --confirm
```

These commands select only Glove80 `16C0:27DB`, usage page `0xFF60`, usage
`0x61`. They require one matching wired endpoint and never target the ordinary
keyboard HID interface.
They do not change LEDs in this firmware increment; a successful valid write
only proves the status transport boundary.

For OmniWM and one visible Ghostty window, copy
`examples/omniwm-focus-ghostty.sh` to `~/.config/beckon/focus-ghostty`, make it
executable, then add this to `config.toml`:

```toml
[focus]
command = ["/Users/you/.config/beckon/focus-ghostty"]
```

Ghostty users with several windows or tabs can instead adopt surfaces per
session with the built-in terminal backend (see "Terminal surface control"
above) and let the focus command handle only the remaining details.

The script re-queries OmniWM on every press because its opaque window IDs are
session-scoped. It uses `window navigate`, which brings Ghostty to its workspace
rather than merely focusing it on an already-visible workspace.

## Optional repeat-press confirmation

By default Beckon never sends input to a pane. To deliberately enable a repeat
press that sends Enter, add this to `config.toml` and restart `beckond`:

```toml
[actions.confirm]
enabled = true
repeat_press_ms = 750
keys = ["enter"]
```

The first press focuses the selected bound pane. A second press of the same key
within the window sends Herdr's logical `enter` key only when that exact pane is
still focused. `keys` is passed to Herdr as logical key names, which Herdr
validates before writing anything. This can confirm an agent or tool prompt, so
enable it only when that behavior is intended.
