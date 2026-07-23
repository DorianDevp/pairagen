local config = require("loopbiotic.config")
local log = require("loopbiotic.log")
local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

-- The mid-turn location-permission gate. When the agent can only continue
-- from a file that is not the supplied buffer, the backend blocks its running
-- turn on an editor/open_location request until the editor answers. This
-- module presents that request in AgentWindow with explicit Accept / Deny and
-- owns the pending answer.
--
-- Every route out of the gate — Accept, Deny, Stop, interrupt, Reset, or a
-- superseding turn result — must resolve the pending request exactly once,
-- and BEFORE any other backend request is sent: the daemon defers unrelated
-- client requests while it waits for this answer.

local M = {}

local saved_keymaps = nil

local function respond_once(pending, result)
  if pending.resolved then
    return
  end
  pending.resolved = true
  pending.respond(result)
end

-- Accept / Deny must work from wherever the user is working, not only from a
-- focused AgentWindow, so the review keys are bound globally for exactly the
-- lifetime of the pending request. A pre-existing user mapping is restored.
local function bind_global()
  local keys = config.values.keymaps
  saved_keymaps = {}
  for lhs, callback in pairs({
    [keys.draft_accept or ""] = M.accept,
    [keys.draft_reject or ""] = M.deny,
  }) do
    if lhs ~= "" then
      table.insert(saved_keymaps, { lhs = lhs, previous = vim.fn.maparg(lhs, "n", false, true) })
      vim.keymap.set("n", lhs, util.guard("permission " .. lhs, callback), { silent = true, nowait = true })
    end
  end
end

local function unbind_global()
  for _, entry in ipairs(saved_keymaps or {}) do
    pcall(vim.keymap.del, "n", entry.lhs)
    if type(entry.previous) == "table" and not vim.tbl_isempty(entry.previous) and entry.previous.buffer == 0 then
      pcall(vim.fn.mapset, "n", false, entry.previous)
    end
  end
  saved_keymaps = nil
end

function M.pending()
  return state.permission ~= nil
end

-- Resolve the pending request as denied without user interaction: session
-- Stop, prompt interruption, Reset, and a superseding turn result (for
-- example the daemon-side wait expiring) all mean the user will not answer
-- this request anymore. A response the daemon no longer waits for is dropped
-- there as stale.
---@param reason string logged, never user-facing
---@return boolean settled whether a request was pending
function M.settle(reason)
  local pending = state.permission
  if not pending then
    return false
  end
  state.permission = nil
  unbind_global()
  respond_once(pending, { granted = false })
  log.write("location permission settled as denied", { file = pending.file, reason = reason })
  return true
end

-- Entry point for the editor/open_location backend request.
---@param params table { session_id, reason, location = { file, line, column } }
---@param respond fun(result: table) must be called exactly once
function M.request(params, respond)
  -- Only one request can wait at a time; a newer one means the backend gave
  -- up on the previous answer.
  M.settle("superseded by a newer request")

  local location = type(params.location) == "table" and params.location or {}
  local file = type(location.file) == "string" and location.file or ""
  local target = file ~= "" and vim.fn.fnamemodify(file, ":p") or ""

  if target == "" or not util.in_workspace(target) then
    -- The workspace boundary is not the user's decision to make.
    respond({ granted = false })
    return
  end

  state.permission = {
    respond = respond,
    resolved = false,
    session_id = params.session_id,
    reason = type(params.reason) == "string" and params.reason or "",
    location = location,
    file = file,
    missing = vim.uv.fs_stat(target) == nil and vim.fn.bufloaded(target) == 0,
  }
  bind_global()
  M.show()
end

-- Grant the request: open the location as a deliberate, user-authorized
-- navigation, capture fresh context for that buffer, and let the same turn
-- continue. A missing workspace file is a new-file lead; it is granted as
-- empty context without navigating anywhere.
function M.accept()
  local pending = state.permission
  if not pending then
    return
  end
  state.permission = nil
  unbind_global()

  local context = require("loopbiotic.context")
  if pending.missing then
    respond_once(pending, { granted = true, context = context.new_file(pending.file) })
  elseif require("loopbiotic.navigation").open_location(pending.location) then
    respond_once(pending, { granted = true, context = context.session() })
  else
    -- The location no longer opens (deleted since the proposal, invalid
    -- target); an Accept that cannot deliver context must not pretend it did.
    respond_once(pending, { granted = false })
    ui.notify("The requested location could not be opened", vim.log.levels.WARN)
  end

  -- The turn keeps running on the backend; put the in-progress View back so
  -- the gate does not linger as stale content.
  if state.card and state.card.kind == "working" then
    require("loopbiotic.card").show(state.card)
  end
end

-- Refuse the request. The backend converts the refusal into a deny card
-- (Retry / Edit prompt / Stop) without another model turn; that card arrives
-- through the ordinary turn-result path and replaces this View.
function M.deny()
  local pending = state.permission
  if not pending then
    return
  end
  state.permission = nil
  unbind_global()
  respond_once(pending, { granted = false })
end

function M.lines()
  local pending = state.permission
  if not pending then
    return {}
  end
  local keys = config.values.keymaps
  local lines = {
    "Agent asks to open a file",
    "",
    string.format("Open  %s:%s", vim.fn.fnamemodify(pending.file, ":~:."), pending.location.line or 1),
  }
  if pending.reason ~= "" then
    table.insert(lines, "")
    for line in (pending.reason .. "\n"):gmatch("([^\n]*)\n") do
      table.insert(lines, line)
    end
  end
  table.insert(lines, "")
  table.insert(lines, string.format("[%s] Accept and open   [%s] Deny", keys.draft_accept, keys.draft_reject))
  return lines
end

function M.show()
  local pending = state.permission
  if not pending then
    return
  end
  local lines = M.lines()
  local width = math.min(58, config.values.card.max_width)
  local height = 0
  for _, line in ipairs(lines) do
    height = height + math.max(math.ceil(vim.fn.strdisplaywidth(line) / width), 1)
  end
  local cursor = state.source_cursor or { 1, 0 }
  surfaces.render_agent(lines, {
    view = "permission",
    -- The backend turn is still live behind this gate: scope must keep
    -- treating the session as working so a prompt remains an interrupt.
    working = true,
    wrap = true,
    enter = false,
    window = {
      width = width,
      height = height,
      border = config.values.card.border,
      anchor = ui.buffer_anchor(state.source_buf, cursor[1], cursor[2]),
      title = " Loopbiotic: Permission ",
    },
    bind = function(buf)
      for index, line in ipairs(lines) do
        local group = line:match("^%[") and "LoopbioticAction" or line:match("^Open  ") and "LoopbioticMuted"
        if group then
          vim.api.nvim_buf_add_highlight(buf, -1, group, index - 1, 0, -1)
        end
      end
      local keys = config.values.keymaps
      vim.keymap.set("n", keys.draft_accept, M.accept, { buffer = buf, nowait = true, silent = true })
      vim.keymap.set("n", keys.draft_reject, M.deny, { buffer = buf, nowait = true, silent = true })
    end,
  })
end

-- Error boundary: reached from an RPC callback and global keymaps; a bug here
-- must log and notify instead of leaving the daemon blocked forever.
M.request = util.guard("permission.request", M.request)
M.accept = util.guard("permission.accept", M.accept)
M.deny = util.guard("permission.deny", M.deny)

return M
