# Beckon

Beckon binds selected Herdr agent panes to Glove80 keys for glanceable state
and direct navigation.

## Current gate

The initial executable registers global F13 and F14 hotkeys on macOS and logs
presses. It validates the navigation transport before Herdr, HID, or firmware
code is added.

Run it with:

```sh
devenv shell -- cargo run
```
