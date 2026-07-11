local config = require("pair.config")

local M = {}
local resize_group = vim.api.nvim_create_augroup("PairFloatViewport", { clear = true })

function M.close(win)
  if win and vim.api.nvim_win_is_valid(win) then
    vim.api.nvim_win_close(win, true)
  end
end

function M.float(lines, opts)
  opts = opts or {}
  lines = M.lines(lines)
  local geometry = M.geometry(#lines, opts)
  local buf = vim.api.nvim_create_buf(false, true)

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].bufhidden = "wipe"

  local enter = opts.enter ~= false
  local win = vim.api.nvim_open_win(buf, enter, {
    relative = "editor",
    row = geometry.row,
    col = geometry.col,
    width = geometry.width,
    height = geometry.height,
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
  local geometry = M.geometry(line_count, opts)

  pcall(vim.api.nvim_win_set_config, win, {
    relative = "editor",
    row = geometry.row,
    col = geometry.col,
    width = geometry.width,
    height = geometry.height,
  })
end

function M.geometry(line_count, opts)
  opts = opts or {}
  local viewport = M.viewport()
  local border = opts.border or config.values.card.border
  local border_size = M.has_border(border) and 2 or 0
  local max_width = math.max(viewport.width - border_size, 1)
  local max_height = math.max(viewport.height - border_size, 1)
  local width = math.min(opts.width or config.values.card.max_width, max_width)
  local height = math.min(opts.height or math.min(line_count, config.values.card.max_height), max_height)
  width = math.max(width, 1)
  height = math.max(height, 1)

  local default_row = math.floor((viewport.height - height - border_size) / 2)
  local default_col = math.floor((viewport.width - width - border_size) / 2)
  local max_row = math.max(viewport.height - height - border_size, 0)
  local max_col = math.max(viewport.width - width - border_size, 0)

  return {
    row = M.clamp(M.number(opts.row) or default_row, 0, max_row),
    col = M.clamp(M.number(opts.col) or default_col, 0, max_col),
    width = width,
    height = height,
  }
end

function M.viewport()
  return {
    width = math.max(vim.o.columns, 1),
    height = math.max(vim.o.lines - vim.o.cmdheight, 1),
  }
end

function M.has_border(border)
  if border == nil or border == "none" then
    return false
  end
  if type(border) == "table" then
    return #border > 0
  end

  return true
end

function M.number(value)
  if type(value) == "table" then
    return tonumber(value[2])
  end

  return tonumber(value)
end

function M.clamp(value, minimum, maximum)
  return math.max(minimum, math.min(value, maximum))
end

function M.keep_floats_visible()
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    local ok, win_config = pcall(vim.api.nvim_win_get_config, win)
    if ok and win_config.relative == "editor" then
      M.resize(win, win_config.height, {
        row = M.number(win_config.row),
        col = M.number(win_config.col),
        width = win_config.width,
        height = win_config.height,
        border = win_config.border,
      })
    end
  end
end

vim.api.nvim_create_autocmd("VimResized", {
  group = resize_group,
  callback = M.keep_floats_visible,
})

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
