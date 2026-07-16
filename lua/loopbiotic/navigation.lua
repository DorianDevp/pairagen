local config = require("loopbiotic.config")
local extmarks = require("loopbiotic.extmarks")
local state = require("loopbiotic.state")
local util = require("loopbiotic.util")

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

  -- Ex/tab commands issued from a focused float can leave that float as the
  -- originating tab's tp_curwin. If a later async render closes it from
  -- another tab, Neovim's tabline may dereference the freed window.
  local normal = M.normal_window()
  if normal ~= vim.api.nvim_get_current_win() then
    vim.api.nvim_set_current_win(normal)
  end

  if target_win then
    vim.api.nvim_set_current_win(target_win)
  elseif target_buf and open == "current" then
    vim.api.nvim_win_set_buf(normal, target_buf)
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

  local buf = vim.api.nvim_get_current_buf()
  local pos = util.clamp_cursor(buf, location.line, (location.column or 1) - 1)

  vim.api.nvim_win_set_cursor(0, pos)
  state.source_buf = buf
  state.source_cursor = pos
  extmarks.annotate(0, pos[1], location.annotation)
  vim.cmd("normal! zz")

  return true
end

function M.from_card(card)
  return M.open_location(M.card_location(card))
end

function M.card_location(card)
  return util.card_location(card)
end

return M
