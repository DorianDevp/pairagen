local config = require("pair.config")

local M = {}

function M.close(win)
  if win and vim.api.nvim_win_is_valid(win) then
    vim.api.nvim_win_close(win, true)
  end
end

function M.float(lines, opts)
  opts = opts or {}
  lines = M.lines(lines)

  local width = opts.width or config.values.card.max_width
  local height = opts.height or math.min(#lines, config.values.card.max_height)
  width = math.min(width, math.max(vim.o.columns - 4, 20))
  height = math.min(height, math.max(vim.o.lines - 4, 1))

  local row = opts.row or math.floor((vim.o.lines - height) / 2)
  local col = opts.col or math.floor((vim.o.columns - width) / 2)
  local buf = vim.api.nvim_create_buf(false, true)

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].bufhidden = "wipe"

  local enter = opts.enter ~= false
  local win = vim.api.nvim_open_win(buf, enter, {
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

function M.render(buf, win, lines, opts)
  opts = opts or {}
  lines = M.lines(lines)

  if not buf or not vim.api.nvim_buf_is_valid(buf) then
    return M.float(lines, opts)
  end

  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false

  if win and vim.api.nvim_win_is_valid(win) then
    M.resize(win, #lines, opts)

    return buf, win
  end

  return M.float(lines, opts)
end

function M.resize(win, line_count, opts)
  local width = opts.width or config.values.card.max_width
  local height = opts.height or math.min(line_count, config.values.card.max_height)
  width = math.min(width, math.max(vim.o.columns - 4, 20))
  height = math.min(height, math.max(vim.o.lines - 4, 1))

  pcall(vim.api.nvim_win_set_config, win, {
    relative = "editor",
    row = opts.row or math.floor((vim.o.lines - height) / 2),
    col = opts.col or math.floor((vim.o.columns - width) / 2),
    width = width,
    height = height,
  })
end

function M.focus(win)
  if vim.fn.mode():match("^[iR]") then
    vim.cmd("stopinsert")
  end

  if win and vim.api.nvim_win_is_valid(win) then
    vim.api.nvim_set_current_win(win)
  end
end

function M.lines(lines)
  local out = {}

  for _, line in ipairs(lines or {}) do
    local text = tostring(line)

    for part in (text .. "\n"):gmatch("([^\n]*)\n") do
      table.insert(out, part)
    end
  end

  if #out == 0 then
    return { "" }
  end

  return out
end

function M.notify(message, level)
  vim.notify(message, level or vim.log.levels.INFO, { title = "Pair" })
end

return M
