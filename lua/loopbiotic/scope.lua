local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")

local M = {}

function M.working()
  return state.turn_barrier == true or state.thinking_request_id ~= nil or surfaces.snapshot().agent.working == true
end

function M.allows(action)
  if action == "prompt" then
    return not surfaces.prompt_open() and not (surfaces.agent_view() == "review" and surfaces.agent_actionable())
  end
  if action == "reset" then
    return true
  end
  if action == "resume" then
    return state.session_id ~= nil and not surfaces.prompt_open() and surfaces.agent_mode() ~= "closed"
  end
  if action == "stop" or action == "quit" then
    return state.session_id ~= nil
  end
  if action == "hide" then
    return state.session_id ~= nil
      and not surfaces.prompt_open()
      and surfaces.agent_mode() == "visible"
      and surfaces.agent_owner_tab() == vim.api.nvim_get_current_tabpage()
  end

  if not state.session_id or M.working() or surfaces.prompt_open() or not surfaces.agent_actionable() then
    return false
  end

  if action == "reply" or action == "go_to" then
    return true
  end
  if action == "accept" or action == "reject" then
    return surfaces.agent_view() == "review"
      and (require("loopbiotic.diff").valid_preview() or require("loopbiotic.fileops").pending())
  end
  return false
end

-- Out-of-scope input is deliberately a no-op. Global mappings must not turn
-- invalid state into UI noise or accidentally contact the backend.
function M.run(action, callback)
  if not M.allows(action) then
    return false
  end
  callback()
  return true
end

return M
