local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}

function M.open(mode)
  ui.close(state.prompt_win)

  local buf, win = ui.float({ "" }, {
    width = 60,
    height = 1,
    border = config.values.prompt.border,
    row = math.floor(vim.o.lines * 0.3),
  })

  state.prompt_buf = buf
  state.prompt_win = win

  vim.bo[buf].buftype = "prompt"
  vim.fn.prompt_setprompt(buf, "Pair> ")
  vim.cmd("startinsert")

  vim.keymap.set("i", "<CR>", function()
    local line = vim.api.nvim_get_current_line():gsub("^Pair>%s*", "")
    vim.cmd("stopinsert")
    ui.close(win)
    require("pair").start(line, mode)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "q", function()
    ui.close(win)
  end, { buffer = buf, nowait = true, silent = true })
end

return M
