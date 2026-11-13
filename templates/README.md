# Configuration templates

## Active v3 templates

Use the root-level `config.toml`, `creator-micro.toml`, `glove80.toml` and
`macbook.toml`. These are exercised by the configuration integration tests.
Copy the root to `~/.config/beckon/config.toml` and chosen device files to
`~/.config/beckon/config.d/`. Declare their global binding ids and source
integrations in the root before enabling them. See
[the configuration guide](../docs/configuration.md).

## Historical v1 sketches below

The `devices/`, `sources/` and `slots.toml` sketches below are retained as design
history, not deployable v3 configuration. Do not copy them to `config.d/`.
The implemented v3 schema supersedes this proposal.

### Original proposal (not runtime instructions)

**These files are not loaded by the current binary.** They are reference
definitions for the v1 fragment convention described in
[`docs/v1-design.md`](../docs/v1-design.md).

To enable an integration, copy or symlink a fragment into
`~/.config/beckon/conf.d/` and restart `beckond`. To disable it, remove the
file. An empty `conf.d/` is valid and simply does nothing.

## Convention

- Scan `conf.d/*.toml` in lexical filename order.
- Each file declares exactly one object: `kind = "device" | "source" | "theme" | "route"`
  plus a stable `id`.
- `deny_unknown_fields` per kind: a typo is a startup error naming the file and
  field, not a silently ignored key.
- Duplicate `(kind, id)` across files is a hard error. No cross-file merging.
- A cross-reference validation pass checks adapter names, channel/slot
  references, theme references and shortcut collisions. Failure refuses to
  start; a partially valid directory never yields partial behaviour.

## What is data, what is code

| Data (fragments) | Code (registered by name) |
|---|---|
| devices, ports, channels, slots, routes | transport adapters: `creator-micro-usb`, `macos-function-keys`, `glove80-usb`, process plugins |
| sources: list / focus / activate / identity commands | the command runner and source registry |
| themes and treatments | the render pipeline |

Adding a **device instance** is config-only. Adding a **new transport** still
needs Rust. That boundary is deliberate.

## Available templates

| File | What it enables |
|---|---|
| `slots.toml` | The one global slot namespace (`s1`..`s10`), opaque ids | 
| `devices/creator-micro.toml` | Creator Micro 2, six AG channels + ring + underlay |
| `devices/macbook.toml` | MacBook function-key input, ten slots |
| `devices/glove80.toml` | Glove80 input + ten LED channels |
| `sources/rex.toml` | Rex sessions as agents |
| `sources/herdr.toml` | Herdr panes as agents (sketch; adapter not built) |

The templates carry machine-local absolute paths. Those are examples; a real
deployment should interpolate them (Nix) or use `$HOME`-relative paths.
