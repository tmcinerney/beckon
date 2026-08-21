#!/usr/bin/env bash
# Focus the sole visible Ghostty window through OmniWM before Beckon selects a
# Herdr pane. This intentionally queries each time: OmniWM window IDs expire
# whenever OmniWM restarts.

set -euo pipefail

window_id="$(
  omniwmctl query windows --app Ghostty --format json |
    jq -er '
      [
        .result.payload.windows[]
        | select(.app.bundleId == "com.mitchellh.ghostty")
        | select(.isVisible and (.isAppHidden | not))
      ]
      | if length == 1 then .[0].id
        elif length == 0 then error("no visible Ghostty window")
        else error("more than one visible Ghostty window")
        end
    '
)"

exec omniwmctl window navigate "$window_id"
