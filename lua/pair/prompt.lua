local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}

function M.open(mode)
  ui.close(state.prompt_win)

  local buf, win = ui.float({ "" }, {
    width = M.width(),
    height = config.values.prompt.height,
    border = config.values.prompt.border,
    row = math.floor(vim.o.lines * 0.3),
  })

  state.prompt_buf = buf
  state.prompt_win = win

  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "markdown"
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true
  vim.wo[win].cursorline = true

  vim.cmd("startinsert")

  vim.keymap.set({ "i", "n" }, "<C-s>", function()
    M.submit(buf, win, mode)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "<CR>", function()
    M.submit(buf, win, mode)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "q", function()
    ui.close(win)
  end, { buffer = buf, nowait = true, silent = true })
end

function M.submit(buf, win, mode)
  local text = M.text(buf)

  if text == "" then
    return
  end

  if vim.fn.mode():match("^[iR]") then
    vim.cmd("stopinsert")
  end

  ui.close(win)
  require("pair").start(text, mode)
end

function M.text(buf)
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)

  return vim.trim(table.concat(lines, "\n"))
end

function M.width()
  local configured = config.values.prompt.width or 88
  local limit = math.max(vim.o.columns - 8, 24)

  return math.min(configured, limit)
end

return M
