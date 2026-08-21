# Beckon

Beckon binds selected Herdr agent panes to Glove80 keys for glanceable state
and direct navigation.

## Current state

The current executable has the manual registration foundation:

- `beckon bind [--key f1]` binds `$HERDR_PANE_ID` through the local daemon.
  Repeating it without `--key` is idempotent: an already-bound pane keeps its
  existing key.
- `beckon release` clears the binding for the current pane; `beckon release --key f2`
  clears F2 from anywhere.
- `beckon status` shows bindings and their current Herdr pane data.
- `beckon listen-keys` records the ten Beckon-layer key events.
- `beckond` renders its live Herdr pane cache to the wired Glove80 status
  endpoint, retrying automatically after a keyboard reconnect.
- `beckon preview` shows the declarative LED plan without writing keyboard HID
  frames; `beckon preview --all-states` shows every supported state treatment.

Bindings are persisted in `$XDG_STATE_HOME/beckon/bindings.json` (falling back
to `~/.local/state/beckon`). Herdr's `fkey` pane token is a visible mirror for
the sidebar, not the source of truth: Herdr does not restore token metadata
after its server restarts.

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
```

The first press focuses the selected bound pane. A second press of the same key
within the window sends Herdr's logical `enter` key only when that exact pane is
still focused. This can confirm an agent or tool prompt, so enable it only when
that behavior is intended.
