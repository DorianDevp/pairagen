local M = {}

function M.setup()
  vim.api.nvim_create_user_command("Pair", function()
    require("pair").prompt()
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

  vim.api.nvim_create_user_command("PairNext", function()
    require("pair").action("next")
  end, { force = true })

  vim.api.nvim_create_user_command("PairStop", function()
    require("pair").stop()
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
end

function M.action_or_prompt(action, mode)
  if require("pair.state").session_id then
    require("pair").action(action)

    return
  end

  require("pair").prompt(mode)
end

return M
