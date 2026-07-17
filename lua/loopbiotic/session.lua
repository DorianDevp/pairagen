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
---@param opts? { update_model?: boolean, track_backend_error?: boolean }
--- update_model=false keeps state.backend_model untouched; track_backend_error=false marks a local result
function M.apply_turn_result(result, opts)
  opts = opts or {}
  local suppress_accept_summary = state.accept_continuation == true
    and type(result.card) == "table"
    and result.card.kind == "summary"

  state.token_usage = type(result.token_usage) == "table" and result.token_usage or nil
  state.turn_token_usage = type(result.turn_token_usage) == "table" and result.turn_token_usage or nil
  if opts.update_model ~= false and type(result.model) == "string" then
    state.backend_model = result.model
  end
  state.context_report = type(result.context_report) == "table" and result.context_report or nil
  log.event("context_optimization", state.context_report or {})
  log.event("agent_attempts", type(result.attempts) == "table" and result.attempts or {})
  if type(result.goal) == "table" then
    state.goal = result.goal
  end
  if opts.track_backend_error == false then
    state.last_backend_error = nil
    state.backend_preflight_error = nil
  else
    track_backend_errors(result.card)
  end
  if result.card and result.card.kind ~= "working" then
    state.cancelled_turn_id = nil
    state.accept_continuation = nil
  end
  if suppress_accept_summary then
    state.last_card = result.card
    state.card = nil
    require("loopbiotic.ui").close(state.card_win)
    state.card_win = nil
    return
  end
  if type(result.card) == "table" then
    require("loopbiotic.card").show(result.card)
  end
end

return M
