local config = require("pair.config")
local selection = require("pair.selection")

local M = {}

function M.current(prompt, mode)
  local source = M.capture()
  local value = vim.deepcopy(source.value)

  value.prompt = prompt
  value.mode = mode or "auto"

  return value, source
end

function M.session()
  return M.capture(require("pair.state").source_buf).value
end

function M.capture(preferred_buf)
  local buf = M.source_buffer(preferred_buf)
  local win = M.buffer_window(buf)
  local cursor = win and vim.api.nvim_win_get_cursor(win) or require("pair.state").source_cursor or { 1, 0 }
  local file = vim.api.nvim_buf_get_name(buf)
  local buffer_text, buffer_start_line = M.buffer_text(cursor[1], buf)
  local selected = nil

  if buf == vim.api.nvim_get_current_buf() then
    selected = selection.get()
  end

  return {
    buf = buf,
    value = {
      cwd = vim.fn.getcwd(),
      file = vim.fn.fnamemodify(file, ":."),
      cursor = {
        line = cursor[1],
        column = cursor[2] + 1,
      },
      selection = selected,
      buffer_text = buffer_text,
      buffer_start_line = buffer_start_line,
      diagnostics = M.diagnostics(file, buf),
    },
  }
end

function M.source_buffer(preferred_buf)
  if preferred_buf and vim.api.nvim_buf_is_valid(preferred_buf) and vim.bo[preferred_buf].buftype == "" then
    return preferred_buf
  end

  local current = vim.api.nvim_get_current_buf()
  if vim.bo[current].buftype == "" and vim.api.nvim_buf_get_name(current) ~= "" then
    return current
  end

  local remembered = require("pair.state").source_buf
  if remembered and vim.api.nvim_buf_is_valid(remembered) and vim.bo[remembered].buftype == "" then
    return remembered
  end

  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    local win_config = vim.api.nvim_win_get_config(win)
    local buf = vim.api.nvim_win_get_buf(win)

    if win_config.relative == "" and vim.bo[buf].buftype == "" and vim.api.nvim_buf_get_name(buf) ~= "" then
      return buf
    end
  end

  return current
end

function M.buffer_window(buf)
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    if vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == buf then
      return win
    end
  end

  return nil
end

function M.buffer_text(line, buf)
  buf = buf or vim.api.nvim_get_current_buf()
  local before = config.values.context.before
  local after = config.values.context.after
  local start = math.max(line - before - 1, 0)
  local finish = math.min(line + after, vim.api.nvim_buf_line_count(buf))
  local lines = vim.api.nvim_buf_get_lines(buf, start, finish, false)

  return table.concat(lines, "\n"), start + 1
end

function M.diagnostics(file, buf)
  local items = {}
  local diagnostics = vim.diagnostic.get(buf or 0)
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
