# Beckon

Beckon maps configured controls to agent bindings or keyboard actions, and renders
agent state and interaction feedback on connected devices. Sources include Rex
blocks and Herdr panes. Devices include the Creator Micro 2, Glove80 and ordinary
global keyboard shortcuts. Licensed under the [MIT License](LICENSE).

## Configuration v3

This is a clean break: v1/v2 configuration is rejected, not silently upgraded.
There are no implicit devices, source integrations, binding names or themes.

```text
~/.config/beckon/
  config.toml               # explicit binding order, sources and themes
  config.d/
    creator-micro.toml       # that device's keys, actions and effects together
    glove80.toml
    macbook.toml
  settings.toml             # machine-owned presentation overrides, written via IPC
~/.local/state/beckon/
  bindings.json             # agent identities; never configuration
  surfaces.json             # explicit terminal adoption records
```

`beckon init` creates the [root template](templates/config.toml), enabling nothing.
Copy only the [device templates](templates/) you want, declare their binding ids in
the root, and configure the sources they reference. Deleting a device file removes
that device's inputs/output on restart. References do not depend on file ordering.
`key1` is an example name, not a reserved enum or hardware keycode.

See [the configuration guide](docs/configuration.md) for the schema, grouping,
validation, action semantics, permissions and upgrade checklist.

## Typical workflow

```sh
beckon config check                 # offline validation; no device writes
beckon keys                         # configured addresses, bindings and actions
beckon daemon                       # normally run by Home Manager as beckond
beckon bind --key key3               # run inside the intended agent terminal
beckon status
beckon focus --key key3
beckon release --key key3
beckon preview --all-states --json   # render inspection; no device writes
beckon config show                  # daemon's effective typed model over IPC
beckon config theme status          # daemon writes only settings.toml
beckon reset --device creator-micro # release held output and clear inferred latch
```

An omitted `--key` selects the first free configured binding. An already-bound
agent keeps its key. A requested occupied key is never stolen. Binding ids are
device-independent; several devices can navigate the same binding.

Rex identity comes from the source's configured `REX_SESSION` / `REX_BLOCK`
variables, so registration is block-level. `activate` and `focus` are direct argv
commands, not shell strings; `$BECKON_AGENT_SOURCE`, `$BECKON_AGENT_SCOPE`,
`$BECKON_AGENT_ID`, `$BECKON_AGENT_STATE` and `$BECKON_AGENT_TITLE` are substituted.
`${HOME}`/`~/` work in configured command paths, including the executable.
For multiple Rex clients/windows, use a host-owned focus script that selects the
client/session before its block; focusing a block alone need not display its session.

Herdr is enabled only by `[herdr] enabled = true`. Its `sessions = "auto"` scans
reachable default/named sessions; a named array restricts it. Binding identity is
source + scope + id. Unobserved scopes keep dormant bindings. Herdr's `fkey` token
is a presentation mirror, not the authoritative ledger.

For adopted Ghostty windows/tabs, see [terminal surface control](docs/terminal-backend-architecture.md):
`beckon terminals`, `beckon adopt --session NAME --terminal HANDLE`, and
`beckon forget --session NAME`. The global focus helper and adopted Ghostty path
apply to Herdr; other sources use their own configured activate/focus commands.

## Creator Micro 2

The adapter owns layer tracking and firmware addresses. Core never interprets
`AG16` or a layer number. A keymap cell carrying an AG code can be used for
navigation, a generic chord action, or grouped feedback—not just vendor agent use.

The template joins bottom-left and bottom-centre explicitly: either key operates
one action; rolling between them does not re-trigger it. Held feedback is solid
red, inferred tap-toggle feedback breathes red, and idle returns to base rendering.
Bottom-right remains a normal `KC_ENT` key in the device keymap.

```sh
# Direct device diagnostics require beckond to be stopped (one HID reader/writer).
beckon creator-micro get keymap.json ./keymap.json
beckon creator-micro check           # check configured addresses against the keymap
beckon creator-micro verify keymap.json ./keymap.json
beckon creator-micro put keymap.json ./keymap.json
beckon creator-micro listen          # raw ordered notifications, including layers
```

Keep the keymap versioned outside Input and verify readback after every write.
Input's next save can overwrite it. Merely accepting a write does not prove an id
is addressable. [Hardware findings](docs/creator-micro-2-hardware.md) separate
measured behaviour from unknowns. USB is verified; Bluetooth RPC/lighting is not.

## Glove80 and MacBook

The Glove80 template keeps F16–F20 and Shift+F16–F20 as input, plus its existing
ten-position wired status protocol. MacBook uses ordinary F1–F10 (hold Fn/Globe
if macOS sends media keys). Both are configuration mappings, not core binding names.
Global shortcuts cannot distinguish which physical keyboard emitted a shortcut.
The wider Glove80 firmware/design review is deferred; no firmware changes are
required by this host-side configuration work.

Glove80 transport diagnostics remain `beckon hid list`, `probe`, `send` and
`send-malformed --confirm`. They target only its vendor status endpoint, not the
ordinary keyboard interface. Coordinate diagnostic writers with the daemon.

External displays use the [version 2 NDJSON protocol](docs/display-plugin-protocol.md),
with a [minimal logging example](examples/display-plugin-log.py).

## Nix / Home Manager

```nix
programs.beckon = {
  enable = true;
  settings = builtins.fromTOML (builtins.readFile ./beckon/config.toml);
  devices = {
    creator-micro = builtins.fromTOML (builtins.readFile ./beckon/creator-micro.toml);
    macbook = builtins.fromTOML (builtins.readFile ./beckon/macbook.toml);
  };
};
services.beckond.enable = true;
```

The supplied TOML must be complete and validated. Empty `settings` leaves the root
unmanaged; empty `devices` installs none. The mutable UI preferences stay separate.
Upgrade the binary, structural config and existing binding names together—do not
install v3 configuration while an older launchd service still owns the device.

## Development

```sh
nix develop --command cargo test --all-targets
nix develop --command cargo clippy --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
nix build
```

`devenv shell -- ...` is also available. Tests use fake emitters and pure render
plans; they do not synthesize real keystrokes or claim hardware validation.
`BECKON_LOG=debug` enables detailed daemon tracing. macOS Home Manager logs live in
`~/Library/Logs/beckon/`; permissions must belong to the actual running binary,
not merely the terminal that launched an earlier probe.
