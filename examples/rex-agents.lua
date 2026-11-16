-- Candidate list helper: install/select only in an attended configuration update.
-- The existing refresh already obtains session/block labels; export the scope
-- label separately without an extra query or parsing the legacy composed title.
-- Reject sparse/keyed collections: ipairs alone can hide malformed population.
local function array(value)
  if type(value) ~= "table" then return nil end
  local count, last = 0, 0
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then return nil end
    count = count + 1
    if key > last then last = key end
  end
  if count ~= last then return nil end
  return value
end

local function call(method, args)
  local result, err = rex.call(method, args)
  assert(type(result) == "table" and err == nil, method .. ": unavailable or malformed result")
  return result
end

local function identity(row, key, seen)
  assert(type(row) == "table" and type(row[key]) == "string" and row[key] ~= "", "invalid " .. key)
  assert(not seen[row[key]], "duplicate " .. key)
  seen[row[key]] = true
end

local rank = { idle = 1, done = 2, working = 3, unknown = 4, error = 4, blocked = 5 }
-- AIDEV-NOTE: population failure must abort, never become a complete empty list.
local sessions = assert(array(call("session.list", {}).sessions), "invalid sessions")
local out = {}
local seen_sessions = {}
for _, s in ipairs(sessions) do
  identity(s, "session_id", seen_sessions)
  call("session.attach", { session_id = s.session_id })
  local blocks = assert(array(call("session.list_blocks", { session_id = s.session_id }).blocks), "invalid blocks")
  local view = call("session.view", { session_id = s.session_id })
  local focused_block = nil
  for _, w in ipairs(assert(array(view.windows), "invalid windows")) do
    if w.window_id == view.active_window_id then focused_block = w.focused_block_id end
  end
  local seen_blocks = {}
  for _, b in ipairs(blocks) do
    identity(b, "block_id", seen_blocks)
    local state = "unknown"
    local ok, status, status_error = pcall(rex.call, "block.method", {
      session_id = s.session_id, block_id = b.block_id,
      method = "program_status", args = {},
    })
    local records = ok and status_error == nil and type(status) == "table" and array(status.records)
    -- AIDEV-NOTE: no reports is not evidence of Idle; only explicit idle is.
    if records and #records > 0 then
      state = "idle"
      for _, record in ipairs(records) do
        local st = type(record) == "table" and record.state or "unknown"
        if type(st) ~= "string" or not rank[st] or st == "error" then st = "unknown" end
        if rank[st] > rank[state] then state = st end
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
