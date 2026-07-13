local M = {}

function M.setup()
  vim.api.nvim_create_user_command("Pair", function()
    require("pair").prompt()
  end, { force = true })

  vim.api.nvim_create_user_command("PairReply", function()
    require("pair").reply_prompt()
  end, { force = true })

  vim.api.nvim_create_user_command("PairFix", function()
    M.action_or_prompt("fix", "fix")
  end, { force = true })

  vim.api.nvim_create_user_command("PairWhy", function()
    M.action_or_prompt("why", "explain")
  end, { force = true })

  vim.api.nvim_create_user_command("PairFollow", function()
    require("pair").action("follow")
  end, { force = true })

  vim.api.nvim_create_user_command("PairOther", function()
    require("pair").action("other_lead")
  end, { force = true })

  vim.api.nvim_create_user_command("PairAssess", function()
    require("pair").action("next")
  end, { force = true })

  vim.api.nvim_create_user_command("PairNext", function()
    require("pair").action("next")
  end, { force = true })

  vim.api.nvim_create_user_command("PairStop", function()
    require("pair").stop()
  end, { force = true })

  vim.api.nvim_create_user_command("PairHide", function()
    require("pair").hide()
  end, { force = true })

  vim.api.nvim_create_user_command("PairResume", function()
    require("pair").resume()
  end, { force = true })

  vim.api.nvim_create_user_command("PairReset", function()
    require("pair").reset()
  end, { force = true })

  vim.api.nvim_create_user_command("PairLog", function()
    local log = require("pair.log")
    if require("pair.config").values.logging.enabled == false then
      print("Pair logging is disabled")
    else
      print(log.path())
    end
  end, { force = true })

  vim.api.nvim_create_user_command("PairLogClear", function()
    require("pair.log").clear()
    print("Pair session logs cleared")
  end, { force = true })

  vim.api.nvim_create_user_command("PairBackend", function()
    require("pair").backend()
  end, { force = true })

  vim.api.nvim_create_user_command("PairAgent", function(opts)
    require("pair").agent(opts.args)
  end, {
    nargs = "?",
    complete = function()
      return require("pair").agents()
    end,
    force = true,
  })

  vim.api.nvim_create_user_command("PairModel", function(opts)
    require("pair").model(opts.args)
  end, {
    nargs = "?",
    complete = function()
      return require("pair").models()
    end,
    force = true,
  })
end

function M.action_or_prompt(action, mode)
  if require("pair.state").session_id then
    require("pair").action(action)

    return
  end

  require("pair").prompt(mode)
end

return M
