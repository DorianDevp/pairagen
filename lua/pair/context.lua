local config = require("pair.config")
local selection = require("pair.selection")

local M = {}

function M.current(prompt, mode)
  local cursor = vim.api.nvim_win_get_cursor(0)
  local file = vim.api.nvim_buf_get_name(0)
  local cwd = vim.fn.getcwd()

  return {
    cwd = cwd,
    file = vim.fn.fnamemodify(file, ":."),
    cursor = {
      line = cursor[1],
      column = cursor[2] + 1,
    },
    selection = selection.get(),
    prompt = prompt,
    mode = mode or "auto",
    buffer_text = M.buffer_text(cursor[1]),
    diagnostics = M.diagnostics(file),
  }
end

function M.buffer_text(line)
  local before = config.values.context.before
  local after = config.values.context.after
  local start = math.max(line - before - 1, 0)
  local finish = math.min(line + after, vim.api.nvim_buf_line_count(0))
  local lines = vim.api.nvim_buf_get_lines(0, start, finish, false)

  return table.concat(lines, "\n")
end

function M.diagnostics(file)
  local items = {}
  local diagnostics = vim.diagnostic.get(0)
  local limit = config.values.context.max_diagnostics

  for _, diagnostic in ipairs(diagnostics) do
    if #items >= limit then
      break
    end

    table.insert(items, {
      file = vim.fn.fnamemodify(file, ":."),
      line = diagnostic.lnum + 1,
      column = diagnostic.col + 1,
      severity = tostring(diagnostic.severity),
      message = M.truncate(diagnostic.message, config.values.context.max_diagnostic_length),
    })
  end

  return items
end

function M.truncate(text, limit)
  text = tostring(text or "")

  if #text <= limit then
    return text
  end

  return text:sub(1, limit) .. "..."
end

return M
