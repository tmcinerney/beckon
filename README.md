# Beckon

Beckon binds selected Herdr agent panes to Glove80 keys for glanceable state
and direct navigation.

## Current state

The current executable has the manual registration foundation:

- `beckon bind [--key f1]` binds `$HERDR_PANE_ID` through the local daemon.
- `beckon release` clears that binding.
- `beckon status` shows bindings and their current Herdr pane data.
- `beckon listen-keys` records the ten Beckon-layer key events.

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
terminal and window manager. LED state delivery is the next milestone.

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
