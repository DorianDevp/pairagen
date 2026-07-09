local config = require("pair.config")

local M = {}

function M.close(win)
  if win and vim.api.nvim_win_is_valid(win) then
    vim.api.nvim_win_close(win, true)
  end
end

function M.float(lines, opts)
  opts = opts or {}

  local width = opts.width or config.values.card.max_width
  local height = math.min(#lines, opts.height or config.values.card.max_height)
  local row = opts.row or math.floor((vim.o.lines - height) / 2)
  local col = opts.col or math.floor((vim.o.columns - width) / 2)
  local buf = vim.api.nvim_create_buf(false, true)

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].bufhidden = "wipe"

  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    row = row,
    col = col,
    width = width,
    height = height,
    style = "minimal",
    border = opts.border or config.values.card.border,
  })

  return buf, win
end

function M.notify(message, level)
  vim.notify(message, level or vim.log.levels.INFO, { title = "Pair" })
end

return M
