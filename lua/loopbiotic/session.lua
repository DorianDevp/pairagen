local log = require("loopbiotic.log")
local state = require("loopbiotic.state")

local M = {}

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
  require("loopbiotic.card").show(result.card)
end

return M
