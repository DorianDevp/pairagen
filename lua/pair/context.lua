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
    buffer_text = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n"),
    diagnostics = M.diagnostics(file),
  }
end

function M.diagnostics(file)
  local items = {}
  local diagnostics = vim.diagnostic.get(0)

  for _, diagnostic in ipairs(diagnostics) do
    table.insert(items, {
      file = vim.fn.fnamemodify(file, ":."),
      line = diagnostic.lnum + 1,
      column = diagnostic.col + 1,
      severity = tostring(diagnostic.severity),
      message = diagnostic.message,
    })
  end

  return items
end

return M
