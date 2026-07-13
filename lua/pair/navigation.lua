local config = require("pair.config")
local extmarks = require("pair.extmarks")
local state = require("pair.state")

local M = {}

function M.normal_window()
  local current = vim.api.nvim_get_current_win()
  local config_ok, win_config = pcall(vim.api.nvim_win_get_config, current)
  if config_ok and win_config.relative == "" then
    return current
  end

  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    local ok, candidate = pcall(vim.api.nvim_win_get_config, win)
    if ok and candidate.relative == "" then
      return win
    end
  end

  return current
end

function M.current_tab_window(buf)
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == buf then
      return win
    end
  end

  return nil
end

function M.any_window(buf)
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    if vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == buf then
      return win
    end
  end

  return nil
end

function M.open_location(location)
  if type(location) ~= "table" then
    return false
  end

  local file = location.file
  local open = config.values.navigation.open
  local target = vim.fn.fnamemodify(file, ":p")
  local target_buf
  local target_win

  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and vim.fn.fnamemodify(vim.api.nvim_buf_get_name(buf), ":p") == target then
      target_buf = buf
      break
    end
  end
  target_win = target_buf and M.current_tab_window(target_buf) or nil

  if target_win then
    vim.api.nvim_set_current_win(target_win)
  elseif target_buf and open == "current" then
    local win = M.normal_window()
    vim.api.nvim_set_current_win(win)
    vim.api.nvim_win_set_buf(win, target_buf)
  elseif open == "tab" then
    local existing = target_buf and M.any_window(target_buf) or nil
    if existing then
      vim.api.nvim_set_current_win(existing)
    else
      vim.cmd("tabedit " .. vim.fn.fnameescape(file))
    end
  elseif open == "split" then
    vim.api.nvim_set_current_win(M.normal_window())
    vim.cmd("split " .. vim.fn.fnameescape(file))
  elseif open == "vsplit" then
    vim.api.nvim_set_current_win(M.normal_window())
    vim.cmd("vsplit " .. vim.fn.fnameescape(file))
  else
    vim.api.nvim_set_current_win(M.normal_window())
    vim.cmd("edit " .. vim.fn.fnameescape(file))
  end

  local line = location.line or 1
  local column = location.column or 1

  vim.api.nvim_win_set_cursor(0, { line, math.max(column - 1, 0) })
  state.source_buf = vim.api.nvim_get_current_buf()
  state.source_cursor = { line, math.max(column - 1, 0) }
  extmarks.annotate(0, line, location.annotation)
  vim.cmd("normal! zz")

  return true
end

function M.from_card(card)
  return M.open_location(M.card_location(card))
end

function M.card_location(card)
  if type(card.next_move) == "table" and card.next_move.kind == "open_location" then
    return card.next_move
  elseif type(card.evidence) == "table" then
    return card.evidence
  elseif type(card.location) == "table" then
    return card.location
  end

  return nil
end

return M
