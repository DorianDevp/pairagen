local util = require("loopbiotic.util")

local M = {}

-- Register a user command with its callback behind the error boundary, so a
-- bug in a command path is logged and reported instead of killing a session.
local function command(name, callback, opts)
  opts = vim.tbl_extend("force", opts or {}, { force = true })
  vim.api.nvim_create_user_command(name, util.guard("command :" .. name, callback), opts)
end

function M.setup()
  command("Loopbiotic", function()
    require("loopbiotic").prompt()
  end)

  command("LoopbioticReply", function()
    require("loopbiotic").reply_prompt()
  end)

  command("LoopbioticFix", function()
    M.action_or_prompt("fix", "fix")
  end)

  command("LoopbioticWhy", function()
    M.action_or_prompt("why", "explain")
  end)

  command("LoopbioticFollow", function()
    require("loopbiotic").action("follow")
  end)

  command("LoopbioticOther", function()
    require("loopbiotic").action("other_lead")
  end)

  command("LoopbioticAssess", function()
    require("loopbiotic").action("next")
  end)

  command("LoopbioticNext", function()
    require("loopbiotic").action("next")
  end)

  command("LoopbioticStop", function()
    require("loopbiotic").stop()
  end)

  command("LoopbioticHide", function()
    require("loopbiotic").hide()
  end)

  command("LoopbioticResume", function()
    require("loopbiotic").resume()
  end)

  command("LoopbioticReset", function()
    require("loopbiotic").reset()
  end)

  command("LoopbioticLog", function()
    local log = require("loopbiotic.log")
    if require("loopbiotic.config").values.logging.enabled == false then
      print("Loopbiotic logging is disabled")
    else
      print(log.path())
    end
  end)

  command("LoopbioticLogClear", function()
    require("loopbiotic.log").clear()
    print("Loopbiotic session logs cleared")
  end)

  command("LoopbioticBackend", function()
    require("loopbiotic").backend()
  end)

  command("LoopbioticAgent", function(opts)
    require("loopbiotic").agent(opts.args)
  end, {
    nargs = "?",
    complete = function()
      return require("loopbiotic").agents()
    end,
  })

  command("LoopbioticModel", function(opts)
    require("loopbiotic").model(opts.args)
  end, {
    nargs = "?",
    complete = function()
      return require("loopbiotic").models()
    end,
  })
end

function M.action_or_prompt(action, mode)
  if require("loopbiotic.state").session_id then
    require("loopbiotic").action(action)

    return
  end

  require("loopbiotic").prompt(mode)
end

return M
