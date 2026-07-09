local config = require("pair.config")

local M = {}

function M.setup()
  local keys = config.values.keymaps
  local key = keys.prompt

  if key and key ~= "" then
    vim.keymap.set({ "n", "v" }, key, function()
      require("pair").prompt()
    end, { silent = true })
  end

  M.action(keys.follow, "follow")
  M.action(keys.why, "why")
  M.action(keys.fix, "fix")
  M.action(keys.other_lead, "other_lead")
  M.action(keys.stop, "stop")
end

function M.action(key, action)
  if not key or key == "" then
    return
  end

  vim.keymap.set("n", key, function()
    require("pair").action(action)
  end, { silent = true })
end

return M
