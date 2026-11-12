# Config templates (v1 proposal)

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
| `devices/creator-micro.toml` | Creator Micro 2, six AG channels + ring + underlay |
| `devices/macbook.toml` | MacBook function-key input, ten slots |
| `devices/glove80.toml` | Glove80 input + ten LED channels |
| `sources/rex.toml` | Rex sessions as agents |
| `sources/herdr.toml` | Herdr panes as agents (sketch; adapter not built) |

The templates carry machine-local absolute paths. Those are examples; a real
deployment should interpolate them (Nix) or use `$HOME`-relative paths.
