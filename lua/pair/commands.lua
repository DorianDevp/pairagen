local M = {}

function M.setup()
  vim.api.nvim_create_user_command("Pair", function()
    require("pair").prompt()
  end, { force = true })

  vim.api.nvim_create_user_command("PairFix", function()
    require("pair").prompt("fix")
  end, { force = true })

  vim.api.nvim_create_user_command("PairWhy", function()
    require("pair").prompt("explain")
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

return M
