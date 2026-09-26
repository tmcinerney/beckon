# Terminal backend architecture: explicit surfaces, pluggable control

## Decision

Beckon can raise the terminal surface (window, tab, or split) that displays a
bound Herdr session before it focuses the pane. This is a **control plane for
terminal surfaces**, and it is deliberately small:

- The **terminal backend** is a pluggable adapter that can enumerate surfaces
  and focus one by an opaque handle. It knows nothing about Herdr, sessions,
  bindings, or layouts.
- The **association** between a session and the surface currently displaying it
  is a machine-local fact. It is recorded only by an explicit user action and
  lives in Beckon's state directory. Beckon never infers it, never repairs it,
  and never guesses when it is missing or stale.
- The **user's focus command** remains a first-class seam for machine-specific
  choreography (window managers, extra applications, offsets). It receives the
  resolved context, including the session and the adopted surface handle.

One production guideline drives all of this: different people run different
terminals, window managers, and layouts on different machines. Beckon ships
primitives — enumerate, focus, adopt, forget — and users compose workflows.
Nothing in this crate encodes a particular desktop arrangement.

## Boundaries

| Concern | Owner | Lives in |
| --- | --- | --- |
| Enumerate and focus terminal surfaces (opaque handles) | Terminal backend adapter | `src/terminal/`, selected by `[terminal] backend` |
| Which surface currently shows which session | Explicit user action; machine-local fact | `surfaces.json` in the state directory |
| When to raise a surface before Herdr pane focus | Navigation policy | daemon (`focus_key`) |
| Machine-specific desktop choreography | User script | `[focus] command` |
| Bindings, logical keys, LED rendering | Core | unchanged |

The backend interface is session-blind and terminal-generic:

```rust
pub trait TerminalBackend {
    fn id(&self) -> &str;
    fn list_surfaces(&self) -> Result<Vec<Surface>>;
    fn focus_surface(&self, handle: &SurfaceHandle) -> Result<()>;
}

pub struct Surface {
    pub handle: SurfaceHandle,  // opaque outside the backend
    pub title: String,          // live description for humans
    pub window: String,
    pub tab: String,
}
```

Handles are opaque strings: a Ghostty surface UUID, a WezTerm pane ID, a tmux
`%N`. Nothing above the backend boundary interprets one.

## Rules

1. **No inference, ever.** No title matching, no auto-adoption, no stale-handle
   repair. A recorded handle that no longer resolves logs a warning and
   navigation proceeds without the surface raise.
2. **Adoption is an explicit primitive, not a workflow.** `beckon terminals`
   lists live surfaces; `beckon adopt --session <name> --terminal <handle>`
   records one; `beckon forget --session <name>` removes it. Beckon makes no
   assumption about how many surfaces exist, what they show, or how they are
   arranged.
3. **Records live in state, not config.** Configuration is declarative intent
   and is often rendered by Home Manager; a CLI writing into it would be
   clobbered on the next switch. Machine-local observations belong in the state
   directory with the binding ledger. Each record names the backend that
   produced it, so changing `[terminal] backend` invalidates records honestly
   instead of silently mis-firing.
4. **No automatic surface creation.** Beckon never opens a window or tab as a
   side effect of a keypress. Creating surfaces is a desktop workflow decision;
   if it is ever automated it is an explicit one-shot command.
5. **The command seam stays first-class.** Users on unsupported terminals, or
   with desktops this model cannot express, keep their own scripts. The command
   now receives `BECKON_KEY`, `BECKON_PANE_ID`, `BECKON_HERDR_SESSION`, and
   `BECKON_TERMINAL_HANDLE` (when a handle is adopted).
6. **Primitives are scriptable.** `beckon terminals` and handle-addressed focus
   are usable from user scripts, so power users can compose their own logic
   without forking the library.

### Focus order

One keypress performs, in order: (1) backend surface focus when an adopted
handle exists, (2) `[focus] command`, (3) Herdr `pane.focus`. The command runs
last so machine-specific adjustments stay authoritative, and its failure is
still a navigation failure; a backend surface failure is a warning, never a
blocked navigation.

## State and configuration

```toml
[terminal]
# "none" (default) keeps the pre-backend behavior: only [focus] command runs.
# "ghostty-applescript" raises Ghostty windows/tabs by surface UUID.
backend = "none"
```

`surfaces.json` in the Beckon state directory:

```json
{
  "version": 1,
  "surfaces": [
    {
      "backend": "ghostty-applescript",
      "session": "agent-workspace",
      "handle": "BBD9B110-3F60-43A3-8C32-50A4D56F240E"
    }
  ]
}
```

Writers: `bindings.json` is written only by the daemon (it reconciles pane
lifecycle); `surfaces.json` is written only by the explicit `adopt`/
`forget` commands. Both are read by the daemon at use time.

## Terminal recipes for future backends

Every serious terminal exposes the same three primitives, so a backend is a
translation exercise rather than a research project:

- **Ghostty (shipped).** AppleScript/JXA dictionary: stable UUIDs per window,
  tab, and terminal surface; `focus` on a terminal raises the window, selects
  the tab, and focuses the surface; verified to cross window-manager workspaces.
- **WezTerm.** `WEZTERM_PANE` in the environment; `wezterm cli spawn` prints the
  new pane ID; `wezterm cli list --format json` enumerates; `wezterm cli
  activate-pane --pane-id` focuses and raises the window.
- **kitty.** `KITTY_WINDOW_ID`; `kitty @ ls` enumerates per-window pid, cmdline,
  and cwd (enabling PID-correlation adoption); `kitty @ focus-window --match
  id:N` focuses.
- **iTerm2.** `ITERM_SESSION_ID`; the Python API exposes per-session `tty` and
  `pid` variables and `async_activate`; user variables (OSC 1337) are a
  metadata channel. tmux control mode's persistent affinity is prior art for
  handle stores such as `surfaces.json`.
- **tmux.** `TMUX_PANE`; `list-panes -F '#{pane_id} #{pane_pid}'`; `select-pane`
  / `switch-client`.

Upstream asks that would remove the remaining gaps (not blockers): Ghostty
injecting a per-surface ID into the environment and exposing surface PID/TTY in
its dictionary.

## What this design does not do

- No title matching, no heuristics, no layout model.
- No automatic respawn or window creation.
- No window-manager knowledge outside the user's own command script.
- No assumptions that one session maps to one tab, one window, or any
  particular arrangement.

## Migration sequence

1. **Phase A (implemented):** session-qualified pane identity, session
   discovery, routing, `[herdr]` configuration.
2. **Phase B (this document):** `TerminalBackend` port with the Ghostty
   backend, `surfaces.json` handle store, `terminals`/`adopt`/`forget`
   commands, focus-context environment for the command seam.
3. **Phase C (future, only on demonstrated need):** explicit surface creation,
   workspace-level or pane-level targets beyond the session, additional
   backends, upstream Ghostty requests.

## Test plan

- Backend enumeration parses a scripted surface list (fake runner; no live
  terminal needed) and maps a missing-surface focus failure to a clear error.
- The handle store round-trips, rejects empty or duplicate-session records, and
  survives a backend change without mis-firing (records name their backend).
- Adoption validates the handle against the live surface list before writing.
- Focus order: an adopted handle focuses the surface; a missing record, a
  stale handle, and a changed backend all degrade to command + pane focus.
- The command seam receives `BECKON_KEY`, `BECKON_PANE_ID`,
  `BECKON_HERDR_SESSION`, and `BECKON_TERMINAL_HANDLE` when present.
- Configuration: `[terminal] backend` accepts only registry names; the default
  preserves pre-backend behavior.

Manual checks on a live desktop: `beckon terminals` lists real surfaces;
`beckon adopt` followed by a keypress selects the correct tab and raises the
window; `beckon forget` restores the command-only behavior.
