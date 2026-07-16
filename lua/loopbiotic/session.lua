local log = require("loopbiotic.log")
local state = require("loopbiotic.state")

local M = {}

-- Guidance appended to the card and the notification when the same backend
-- error arrives twice in a row.
M.repeat_guidance = "Same backend error twice — retry is unlikely to help. "
  .. "Check :checkhealth loopbiotic and the agent CLI (auth/config)."

-- Whether the current backend-error message repeats the previous one
-- exactly. Pure: only non-empty, identical consecutive messages escalate.
---@param previous string|nil message of the previous backend error card
---@param current string|nil message of the error card that just arrived
---@return boolean escalate
function M.repeated_error(previous, current)
  return type(current) == "string" and current ~= "" and previous == current
end

-- The message text of an error card (message, else title), or nil for
-- non-error cards. vim.NIL and empty strings count as no message.
---@param card table|nil
---@return string|nil
local function error_message(card)
  if type(card) ~= "table" or card.kind ~= "error" then
    return nil
  end

  for _, value in ipairs({ card.message, card.title }) do
    if type(value) == "string" and value ~= "" then
      return value
    end
  end

  return nil
end

-- Track consecutive identical backend-error cards. On the second identical
-- error, escalate: append a warning line to the card and raise an ERROR
-- notification with guidance, since retrying clearly is not helping. Any
-- non-error card clears the tracking (and the warmup preflight failure).
---@param card table|nil the turn-result card, mutated on escalation
local function track_backend_errors(card)
  local message = error_message(card)

  if not message then
    state.last_backend_error = nil
    state.backend_preflight_error = nil
    return
  end

  if M.repeated_error(state.last_backend_error, message) then
    card.warnings = card.warnings or {}
    table.insert(card.warnings, M.repeat_guidance)
    require("loopbiotic.ui").notify(message .. "\n" .. M.repeat_guidance, vim.log.levels.ERROR)
  end

  state.last_backend_error = message
end

-- Apply the shared tail of a successful turn result (session/start,
-- session/action, session/reply, patch/apply_result): record usage and
-- reports, log them, adopt the updated goal, and show the resulting card.
-- Call-site-specific handling (thinking guards, stale-session checks,
-- session_id adoption) stays at the call sites.
---@param result table backend turn result
---@param opts? { update_model?: boolean } update_model=false keeps state.backend_model untouched
function M.apply_turn_result(result, opts)
  opts = opts or {}

  state.token_usage = result.token_usage
  state.turn_token_usage = result.turn_token_usage
  if opts.update_model ~= false then
    state.backend_model = result.model or state.backend_model
  end
  state.context_report = result.context_report
  log.event("context_optimization", result.context_report or {})
  log.event("agent_attempts", result.attempts or {})
  state.goal = result.goal or state.goal
  track_backend_errors(result.card)
  require("loopbiotic.card").show(result.card)
end

return M
