local M = {}

function M.setup()
  vim.api.nvim_create_user_command("Loopbiotic", function()
    require("loopbiotic").prompt()
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticReply", function()
    require("loopbiotic").reply_prompt()
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticFix", function()
    M.action_or_prompt("fix", "fix")
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticWhy", function()
    M.action_or_prompt("why", "explain")
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticFollow", function()
    require("loopbiotic").action("follow")
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticOther", function()
    require("loopbiotic").action("other_lead")
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticAssess", function()
    require("loopbiotic").action("next")
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticNext", function()
    require("loopbiotic").action("next")
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticStop", function()
    require("loopbiotic").stop()
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticHide", function()
    require("loopbiotic").hide()
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticResume", function()
    require("loopbiotic").resume()
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticReset", function()
    require("loopbiotic").reset()
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticLog", function()
    local log = require("loopbiotic.log")
    if require("loopbiotic.config").values.logging.enabled == false then
      print("Loopbiotic logging is disabled")
    else
      print(log.path())
    end
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticLogClear", function()
    require("loopbiotic.log").clear()
    print("Loopbiotic session logs cleared")
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticBackend", function()
    require("loopbiotic").backend()
  end, { force = true })

  vim.api.nvim_create_user_command("LoopbioticAgent", function(opts)
    require("loopbiotic").agent(opts.args)
  end, {
    nargs = "?",
    complete = function()
      return require("loopbiotic").agents()
    end,
    force = true,
  })

  vim.api.nvim_create_user_command("LoopbioticModel", function(opts)
    require("loopbiotic").model(opts.args)
  end, {
    nargs = "?",
    complete = function()
      return require("loopbiotic").models()
    end,
    force = true,
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
