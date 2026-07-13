local config = require("loopbiotic.config")

local M = {}
local resize_group = vim.api.nvim_create_augroup("LoopbioticFloatViewport", { clear = true })

function M.setup_highlights()
  local highlights = {
    LoopbioticNormal = { link = "NormalFloat" },
    LoopbioticBorder = { link = "FloatBorder" },
    LoopbioticTitle = { link = "Title" },
    LoopbioticMuted = { link = "Comment" },
    LoopbioticAction = { link = "Special" },
    LoopbioticGoal = { link = "Identifier" },
  }

  for name, value in pairs(highlights) do
    vim.api.nvim_set_hl(0, name, vim.tbl_extend("force", { default = true }, value))
  end
end

function M.close(win)
  if win and vim.api.nvim_win_is_valid(win) then
    vim.api.nvim_win_close(win, true)
  end
end

function M.float(lines, opts)
  opts = opts or {}
  M.setup_highlights()
  lines = M.lines(lines)
  local geometry = M.geometry(#lines, opts)
  local buf = vim.api.nvim_create_buf(false, true)

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].bufhidden = "wipe"

  local enter = opts.enter ~= false
  vim.bo[buf].modifiable = false

  local win = vim.api.nvim_open_win(buf, enter, {
    relative = "editor",
    row = geometry.row,
    col = geometry.col,
    width = geometry.width,
    height = geometry.height,
    style = "minimal",
    border = opts.border or config.values.card.border,
    title = opts.title,
    title_pos = opts.title and (opts.title_pos or "left") or nil,
    footer = opts.footer,
    footer_pos = opts.footer and (opts.footer_pos or "right") or nil,
    focusable = opts.focusable ~= false,
    zindex = opts.zindex or 60,
  })
  vim.wo[win].winhighlight = opts.winhighlight or "NormalFloat:LoopbioticNormal,FloatBorder:LoopbioticBorder"

  return buf, win
end

function M.render(buf, win, lines, opts)
  opts = opts or {}
  lines = M.lines(lines)

  if win
    and vim.api.nvim_win_is_valid(win)
    and vim.api.nvim_win_get_tabpage(win) ~= vim.api.nvim_get_current_tabpage()
  then
    M.close(win)
    win = nil
    buf = nil
  end

  if not buf or not vim.api.nvim_buf_is_valid(buf) then
    return M.float(lines, opts)
  end

  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false

  if win and vim.api.nvim_win_is_valid(win) then
    M.resize(win, #lines, opts)
    vim.wo[win].winhighlight = opts.winhighlight or "NormalFloat:LoopbioticNormal,FloatBorder:LoopbioticBorder"

    return buf, win
  end

  return M.float(lines, opts)
end

function M.resize(win, line_count, opts)
  local geometry = M.geometry(line_count, opts)
  local win_config = {
    relative = "editor",
    row = geometry.row,
    col = geometry.col,
    width = geometry.width,
    height = geometry.height,
  }
  if opts.title then
    win_config.title = opts.title
    win_config.title_pos = opts.title_pos or "left"
  end
  if opts.footer then
    win_config.footer = opts.footer
    win_config.footer_pos = opts.footer_pos or "right"
  end

  pcall(vim.api.nvim_win_set_config, win, win_config)
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
  local row = M.number(opts.row)
  local col = M.number(opts.col)

  if opts.anchor then
    row, col = M.near(opts.anchor, width, height, border_size, viewport)
  end

  return {
    row = M.clamp(row or default_row, 0, max_row),
    col = M.clamp(col or default_col, 0, max_col),
    width = width,
    height = height,
  }
end

function M.near(anchor, width, height, border_size, viewport)
  local cursor_row = M.clamp((anchor.row or 1) - 1, 0, viewport.height - 1)
  local cursor_col = M.clamp((anchor.col or 1) - 1, 0, viewport.width - 1)
  local total_width = width + border_size
  local total_height = height + border_size
  local row = cursor_row + 1
  local col = cursor_col + 2

  if row + total_height > viewport.height then
    row = cursor_row - total_height
  end
  if col + total_width > viewport.width then
    col = cursor_col - total_width - 1
  end

  return row, col
end

function M.buffer_anchor(buf, line, column)
  if not buf or not vim.api.nvim_buf_is_valid(buf) then
    return nil
  end

  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_get_buf(win) == buf then
      local position = vim.fn.screenpos(win, math.max(line or 1, 1), math.max((column or 0) + 1, 1))
      if position.row > 0 and position.col > 0 then
        return { row = position.row, col = position.col }
      end
    end
  end

  return nil
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
  vim.notify(message, level or vim.log.levels.INFO, { title = "Loopbiotic" })
end

return M
