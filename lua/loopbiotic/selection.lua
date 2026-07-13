local M = {}

function M.get()
  local mode = vim.fn.mode()

  if mode ~= "v" and mode ~= "V" and mode ~= "\22" then
    return nil
  end

  local start_pos = vim.fn.getpos("v")
  local end_pos = vim.fn.getpos(".")
  local start_line = math.min(start_pos[2], end_pos[2])
  local end_line = math.max(start_pos[2], end_pos[2])
  local lines = vim.api.nvim_buf_get_lines(0, start_line - 1, end_line, false)

  return {
    start = {
      line = start_line,
      column = start_pos[3],
    },
    ["end"] = {
      line = end_line,
      column = end_pos[3],
    },
    text = table.concat(lines, "\n"),
  }
end

return M
