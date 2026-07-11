local config = require("pair.config")
local extmarks = require("pair.extmarks")
local state = require("pair.state")

local M = {}

function M.open_location(location)
  if type(location) ~= "table" then
    return false
  end

  local file = location.file
  local open = config.values.navigation.open

  if open == "tab" then
    vim.cmd("tabedit " .. vim.fn.fnameescape(file))
  elseif open == "split" then
    vim.cmd("split " .. vim.fn.fnameescape(file))
  elseif open == "vsplit" then
    vim.cmd("vsplit " .. vim.fn.fnameescape(file))
  else
    vim.cmd("edit " .. vim.fn.fnameescape(file))
  end

  local line = location.line or 1
  local column = location.column or 1

  vim.api.nvim_win_set_cursor(0, { line, math.max(column - 1, 0) })
  state.source_buf = vim.api.nvim_get_current_buf()
  state.source_cursor = { line, math.max(column - 1, 0) }
  extmarks.annotate(0, line, location.annotation)

  return true
end

function M.from_card(card)
  if type(card.next_move) == "table" and card.next_move.kind == "open_location" then
    return M.open_location(card.next_move)
  elseif type(card.evidence) == "table" then
    return M.open_location(card.evidence)
  elseif type(card.location) == "table" then
    return M.open_location(card.location)
  end

  return false
end

return M
