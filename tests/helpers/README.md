# Rex list-helper contract tests

Run from the repository root, using Lua 5.1 (the version documented by Rex):

```sh
nix shell --no-update-lock-file --no-write-lock-file \
  github:NixOS/nixpkgs/07e1d92cdc0ed416cfa11ff3ca40d17e61cfba7a#lua5_1 \
  --command lua tests/helpers/rex_agents_test.lua examples/rex-agents.lua
```

That Nixpkgs revision is the repository's `flake.lock` input; it supplies Lua
5.1.5 without changing project dependencies, installing a global package, or
using an ambient interpreter. No Rust or Swift build is needed. An optional
second argument filters test names. A third argument `canonical` checks the
canonical variant's existing output shape (without `scope_title`) when reviewing
a **private candidate copy**, never the installed helper.

The harness loads the actual helper chunk, then supplies an isolated environment
with only mock `rex.call` and Lua primitives. It exposes no real Rex client,
network/process/filesystem API. Tests assert returned rows, failures, and exact
call arguments/order, not source text. Malformed responses are negative fixtures,
not assertions that the real server returns them.

## Conservative input contract

- Keep the existing helper's `sessions`, `blocks`, `windows`, and `records`
  interface. No new API method, discovery call, retry, filter or source limit.
- The [Lua API](https://www.superlogical.com/rex/docs/reference/lua.md) documents
  `rex.call` as returning a result table or `nil, error`, not throwing on ordinary
  failure. Tests cover both that result and thrown errors. Population/attach/view
  failures abort the helper instead of returning a successful subset/empty list.
- Only explicit recognized Idle records produce Idle. Successful **zero records**
  remains Unknown: the [program-status protocol](https://www.superlogical.com/rex/docs/build/program-status.md)
  does not equate absence of reports with an idle program. Nil/malformed records
  and unknown states also contribute Unknown. Error normalizes to Unknown; valid
  Blocked still outranks uncertainty. Canonical priority is unchanged.
- Explicit empty session/block collections return an empty **Lua table**, unchanged
  from the original helper. Rex documents empty tables as JSON objects, not arrays;
  these tests do not prove a wire-level `[]` or complete-empty Rust cohort. Rust
  rejects non-array command output, conservatively. JSON object/array distinctions
  (and dropped null array entries) already lost by Rex's Lua conversion cannot be
  recovered here; no new array-tag/serialization API is invented.

These are offline source-contract tests, not installed-helper, current-server
schema/version, real-session completeness, deployment or hardware acceptance.
