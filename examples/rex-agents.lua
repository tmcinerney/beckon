-- Candidate list helper: install/select only in an attended configuration update.
-- The existing refresh already obtains session/block labels; export the scope
-- label separately without an extra query or parsing the legacy composed title.
local rank = { idle = 1, done = 2, working = 3, unknown = 4, error = 4, blocked = 5 }
local sessions = (rex.call("session.list", {}) or {}).sessions or {}
local out = {}
for _, s in ipairs(sessions) do
  rex.call("session.attach", { session_id = s.session_id })
  local blocks = (rex.call("session.list_blocks", { session_id = s.session_id }) or {}).blocks or {}
  local view = rex.call("session.view", { session_id = s.session_id }) or {}
  local focused_block = nil
  for _, w in ipairs(view.windows or {}) do
    if w.window_id == view.active_window_id then focused_block = w.focused_block_id end
  end
  for _, b in ipairs(blocks) do
    local state = "idle"
    local ok, status = pcall(rex.call, "block.method", {
      session_id = s.session_id, block_id = b.block_id,
      method = "program_status", args = {},
    })
    if ok and status then
      for _, record in ipairs(status.records or {}) do
        local st = record.state or "unknown"
        if st == "error" then st = "unknown" end
        if (rank[st] or 0) > (rank[state] or 0) then state = st end
      end
    end
    local short = b.block_id:match("block:(........)") or b.block_id
    out[#out + 1] = {
      scope = s.session_id,
      scope_title = s.label,
      id = b.block_id,
      state = state,
      title = (s.label or s.session_id) .. " / " .. (b.label or "block") .. " " .. short,
      focused = (b.block_id == focused_block),
    }
  end
end
return out
