local config = require("pair.config")

local M = {}

function M.setup()
  local key = config.values.keymaps.prompt

  if key and key ~= "" then
    vim.keymap.set({ "n", "v" }, key, function()
      require("pair").prompt()
    end, { silent = true })
  end
end

return M
