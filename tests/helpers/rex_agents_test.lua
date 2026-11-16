-- Execute the real helper with only a mock Rex boundary and Lua primitives.
-- No Rex executable, network, process, socket, or helper filesystem API is exposed.
local helper = assert(arg[1], "pass a helper candidate path")
local filter = arg[2]
local canonical = arg[3] == "canonical"
local passed, failed = 0, 0

local function equal(actual, expected)
  if type(actual) ~= type(expected) then return false end
  if type(actual) ~= "table" then return actual == expected end
  for k, value in pairs(expected) do if not equal(actual[k], value) then return false end end
  for k in pairs(actual) do if expected[k] == nil then return false end end
  return true
end
local function expect(actual, expected, message)
  assert(equal(actual, expected), message or ("expected " .. tostring(expected) .. ", got " .. tostring(actual)))
end
local function run(overrides)
  local calls = {}
  local responses = {
    ["session.list"] = function() return {sessions = {{session_id = "session:opaque", label = "Work"}}} end,
    ["session.attach"] = function() return {} end,
    ["session.list_blocks"] = function() return {blocks = {{block_id = "block:abcdefgh-rest", label = "shell"}}} end,
    ["session.view"] = function() return {active_window_id = "window:one", windows = {{window_id = "window:one", focused_block_id = "block:abcdefgh-rest"}}} end,
    ["block.method"] = function() return {records = {{state = "idle"}}} end,
  }
  for method, fn in pairs(overrides or {}) do responses[method] = fn end
  local chunk = assert(loadfile(helper))
  setfenv(chunk, {
    rex = {call = function(method, args)
      calls[#calls + 1] = {method = method, args = args}
      return assert(responses[method], "unexpected Rex method: " .. tostring(method))(args)
    end},
    assert = assert, error = error, ipairs = ipairs, pairs = pairs, pcall = pcall,
    type = type, tostring = tostring,
  })
  return chunk(), calls
end
local function test(name, fn)
  if filter and not name:find(filter, 1, true) then return end
  local ok, err = pcall(fn)
  if ok then passed = passed + 1; print("PASS " .. name)
  else failed = failed + 1; print("FAIL " .. name .. ": " .. tostring(err)) end
end

test("status_failure_is_unknown", function()
  for _, response in ipairs({
    function() return nil, "query failed" end,
    function() error("query raised") end,
    function() return {records = {{state = "idle"}}}, "contradictory error" end,
    function() return {records = {{state = "idle"}}}, false end,
  }) do
    local rows = run({["block.method"] = response})
    expect(rows[1].state, "unknown", "a failed status query must not become idle")
  end
end)

test("status_malformed_and_zero_records_are_unknown", function()
  for _, status in ipairs({
    {}, false, 7, "invalid", {records = 7}, {records = "invalid"},
    {records = {}}, {records = {state = "idle"}},
    {records = {[2] = {state = "idle"}}},
    {records = {{state = "idle"}, extra = {state = "idle"}}},
    {records = {7}}, {records = {false}}, {records = {"idle"}},
    {records = {{}}}, {records = {{state = 7}}}, {records = {{state = false}}},
    {records = {{state = "future-state"}}},
    {records = {{state = "idle"}, {state = "future-state"}}},
    {records = {{state = "idle"}, {state = "clear"}}},
  }) do
    local rows = run({["block.method"] = function() return status end})
    expect(rows[1].state, "unknown", "malformed or absent state must contribute unknown, never idle")
  end
end)

test("population_failure_never_returns_a_successful_partial_or_empty_list", function()
  for _, method in ipairs({"session.list", "session.list_blocks", "session.attach"}) do
    for _, response in ipairs({
      function() return nil, "unavailable" end,
      function() return nil end,
      function() error("unavailable") end,
      function() return 7 end,
    }) do
      local ok = pcall(run, {[method] = response})
      expect(ok, false, method .. " failure must abort the whole helper")
    end
  end
  for _, spec in ipairs({{"session.list", "sessions"}, {"session.list_blocks", "blocks"}}) do
    for _, bad in ipairs({false, 7, "invalid", {named = {}}, {[2] = {}}, {7}, {{}},
      {{session_id = "", block_id = ""}}, {{session_id = 7, block_id = 7}},
      {{session_id = "duplicate", block_id = "duplicate"}, {session_id = "duplicate", block_id = "duplicate"}},
    }) do
      local ok = pcall(run, {[spec[1]] = function() return {[spec[2]] = bad} end})
      expect(ok, false, "invalid " .. spec[2] .. " must not be hidden by ipairs/defaults")
    end
    expect(pcall(run, {[spec[1]] = function() return {} end}), false, "missing population must fail")
  end
end)

test("explicit_states_keep_canonical_priority_and_blocked_outranks_uncertainty", function()
  for _, spec in ipairs({
    {"idle", "idle"}, {"done", "done"}, {"working", "working"},
    {"unknown", "unknown"}, {"error", "unknown"}, {"blocked", "blocked"},
  }) do
    local rows = run({["block.method"] = function() return {records = {{state = spec[1]}}} end})
    expect(rows[1].state, spec[2])
  end
  for _, spec in ipairs({
    {{{state = "idle"}, {state = "done"}}, "done"},
    {{{state = "done"}, {state = "working"}}, "working"},
    {{{state = "working"}, {}}, "unknown"},
    {{{state = "idle"}, {state = "future-state"}, {state = "blocked"}}, "blocked"},
    {{{state = "blocked"}, 7, {state = "error"}}, "blocked"},
  }) do
    local rows = run({["block.method"] = function() return {records = spec[1]} end})
    expect(rows[1].state, spec[2])
  end
end)

test("successful_empty_populations_are_not_query_failures_or_fabricated_idle", function()
  local rows, calls = run({["session.list"] = function() return {sessions = {}} end})
  expect(rows, {})
  expect(calls, {{method = "session.list", args = {}}}, "empty sessions must issue no session/block query")
  rows, calls = run({["session.list_blocks"] = function() return {blocks = {}} end})
  expect(rows, {})
  expect(#calls, 4, "existing per-session attach/list/view only; no status query for zero blocks")
end)

test("all_sessions_unbound_focus_labels_and_opaque_ids_are_preserved", function()
  local rows, calls = run({
    ["session.list"] = function() return {sessions = {
      {session_id = "session:one", label = "Engineering"}, {session_id = "session:two"},
    }} end,
    ["session.list_blocks"] = function(args)
      if args.session_id == "session:one" then return {blocks = {
        {block_id = "block:abcdefgh-one", label = "Editor", token = "existing-binding"},
        {block_id = "opaque-unbound"},
      }} end
      expect(args.session_id, "session:two")
      return {blocks = {{block_id = "block:abcdefgh-one", label = "shell"}}}
    end,
    ["session.view"] = function(args)
      if args.session_id == "session:one" then return {active_window_id = "w2", windows = {
        {window_id = "w1", focused_block_id = "opaque-unbound"},
        {window_id = "w2", focused_block_id = "block:abcdefgh-one"},
      }} end
      return {windows = {}}
    end,
    ["block.method"] = function(args)
      if args.session_id == "session:two" then return {records = {{state = "blocked"}}} end
      if args.block_id == "opaque-unbound" then return nil, "failed status" end
      return {records = {{state = "idle"}}}
    end,
  })
  local expected = {
    {scope = "session:one", id = "block:abcdefgh-one", state = "idle", title = "Engineering / Editor abcdefgh", focused = true},
    {scope = "session:one", id = "opaque-unbound", state = "unknown", title = "Engineering / block opaque-unbound", focused = false},
    {scope = "session:two", id = "block:abcdefgh-one", state = "blocked", title = "session:two / shell abcdefgh", focused = false},
  }
  if not canonical then expected[1].scope_title = "Engineering"; expected[2].scope_title = "Engineering" end
  expect(rows, expected, "complete output rows must preserve granularity/titles/focus without token filtering")
  expect(calls, {
    {method = "session.list", args = {}},
    {method = "session.attach", args = {session_id = "session:one"}},
    {method = "session.list_blocks", args = {session_id = "session:one"}},
    {method = "session.view", args = {session_id = "session:one"}},
    {method = "block.method", args = {session_id = "session:one", block_id = "block:abcdefgh-one", method = "program_status", args = {}}},
    {method = "block.method", args = {session_id = "session:one", block_id = "opaque-unbound", method = "program_status", args = {}}},
    {method = "session.attach", args = {session_id = "session:two"}},
    {method = "session.list_blocks", args = {session_id = "session:two"}},
    {method = "session.view", args = {session_id = "session:two"}},
    {method = "block.method", args = {session_id = "session:two", block_id = "block:abcdefgh-one", method = "program_status", args = {}}},
  }, "no new enumeration, probes, queries or methods")
end)

test("later_session_population_failure_discards_earlier_success", function()
  local ok = pcall(run, {
    ["session.list"] = function() return {sessions = {{session_id = "one"}, {session_id = "two"}}} end,
    ["session.list_blocks"] = function(args)
      if args.session_id == "two" then return nil, "unreachable second session" end
      return {blocks = {{block_id = "first"}}}
    end,
  })
  expect(ok, false, "earlier idle rows cannot be returned as a successful complete subset")
end)

test("view_failure_is_fail_closed_without_an_invented_focus", function()
  for _, response in ipairs({
    function() return nil, "view failed" end,
    function() return {} end,
    function() return {windows = 7} end,
  }) do
    expect(pcall(run, {["session.view"] = response}), false)
  end
end)

assert(passed + failed > 0, "test filter matched nothing")
print(string.format("RESULT %d passed, %d failed (%s)", passed, failed, helper))
if failed > 0 then os.exit(1) end
