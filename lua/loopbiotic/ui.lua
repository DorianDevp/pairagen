local config = require("loopbiotic.config")

local M = {}
local resize_group = vim.api.nvim_create_augroup("LoopbioticFloatViewport", { clear = true })
local deferred_closes = {}

local function normal_window(tab)
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(tab)) do
    local ok, win_config = pcall(vim.api.nvim_win_get_config, win)
    if ok and win_config.relative == "" then
      return win
    end
  end

  return nil
end

-- Where the cursor should land when a focused float closes: the window the
-- user was in before entering the float, when that is still a valid normal
-- window of the same tab. Falling back to the tab's first window would drop
-- the cursor into the top-left split regardless of where the user works.
local function return_window(tab)
  local previous = vim.fn.win_getid(vim.fn.winnr("#"))
  if previous ~= 0 and vim.api.nvim_win_is_valid(previous) and vim.api.nvim_win_get_tabpage(previous) == tab then
    local ok, win_config = pcall(vim.api.nvim_win_get_config, previous)
    if ok and win_config.relative == "" then
      return previous
    end
  end

  return normal_window(tab)
end

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
  if not win then
    return true
  end
  if not vim.api.nvim_win_is_valid(win) then
    deferred_closes[win] = nil
    return true
  end

  local tab = vim.api.nvim_win_get_tabpage(win)
  if tab ~= vim.api.nvim_get_current_tabpage() then
    -- Closing a focused float from another tab can leave tp_curwin pointing
    -- at freed memory in Neovim 0.12. Hide it now and close it when that tab
    -- becomes current, after first selecting a normal editor window.
    deferred_closes[win] = true
    pcall(vim.api.nvim_win_set_config, win, { hide = true })
    return false
  end

  if vim.api.nvim_get_current_win() == win then
    local normal = return_window(tab)
    if not normal then
      return false
    end
    vim.api.nvim_set_current_win(normal)
  end

  deferred_closes[win] = nil
  return pcall(vim.api.nvim_win_close, win, true)
end

function M.cleanup_deferred()
  local current_tab = vim.api.nvim_get_current_tabpage()
  for win in pairs(deferred_closes) do
    if not vim.api.nvim_win_is_valid(win) then
      deferred_closes[win] = nil
    elseif vim.api.nvim_win_get_tabpage(win) == current_tab then
      M.close(win)
    end
  end
end

-- Low-level technical Frame constructor. Product code must enter through
-- surfaces.lua so PromptWindow and AgentWindow ownership cannot be bypassed.
function M.open_frame(lines, opts)
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

function M.render_frame(buf, win, lines, opts)
  opts = opts or {}
  lines = M.lines(lines)

  if
    win
    and vim.api.nvim_win_is_valid(win)
    and vim.api.nvim_win_get_tabpage(win) ~= vim.api.nvim_get_current_tabpage()
  then
    M.close(win)
    win = nil
    buf = nil
  end

  if not buf or not vim.api.nvim_buf_is_valid(buf) then
    return M.open_frame(lines, opts)
  end

  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false

  if win and vim.api.nvim_win_is_valid(win) then
    M.resize(win, #lines, opts)
    vim.wo[win].winhighlight = opts.winhighlight or "NormalFloat:LoopbioticNormal,FloatBorder:LoopbioticBorder"
    if opts.enter == true then
      M.focus(win)
    end

    return buf, win
  end

  return M.open_frame(lines, opts)
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

  if opts.anchor and opts.avoid_anchor_row then
    local gap = math.max(tonumber(opts.anchor_gap) or 0, 0)
    local cursor_row = M.clamp((opts.anchor.row or 1) - 1, 0, viewport.height - 1)
    local room_above = math.max(cursor_row - gap, 0)
    local room_below = math.max(viewport.height - cursor_row - 1 - gap, 0)
    local anchored_height = math.max(math.max(room_above, room_below) - border_size, 1)
    height = math.min(height, anchored_height)
  end

  local default_row = math.floor((viewport.height - height - border_size) / 2)
  local default_col = math.floor((viewport.width - width - border_size) / 2)
  local max_row = math.max(viewport.height - height - border_size, 0)
  local max_col = math.max(viewport.width - width - border_size, 0)
  local row = M.number(opts.row)
  local col = M.number(opts.col)

  if opts.anchor then
    row, col = M.near(opts.anchor, width, height, border_size, viewport, opts.anchor_gap)
  end

  return {
    row = M.clamp(row or default_row, 0, max_row),
    col = M.clamp(col or default_col, 0, max_col),
    width = width,
    height = height,
  }
end

function M.near(anchor, width, height, border_size, viewport, gap)
  local cursor_row = M.clamp((anchor.row or 1) - 1, 0, viewport.height - 1)
  local cursor_col = M.clamp((anchor.col or 1) - 1, 0, viewport.width - 1)
  local total_width = width + border_size
  local total_height = height + border_size
  gap = math.max(tonumber(gap) or 0, 0)
  local row = cursor_row + 1 + gap
  local col = cursor_col + 2

  if row + total_height > viewport.height then
    row = cursor_row - total_height - gap
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

  -- A buffer can be visible in several splits; anchor at the split the user
  -- is working in (current window, then the window they came from) before
  -- falling back to the tab's window order.
  local candidates = { vim.api.nvim_get_current_win(), vim.fn.win_getid(vim.fn.winnr("#")) }
  vim.list_extend(candidates, vim.api.nvim_tabpage_list_wins(0))
  local seen = {}
  for _, win in ipairs(candidates) do
    if win ~= 0 and not seen[win] and vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == buf then
      seen[win] = true
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
