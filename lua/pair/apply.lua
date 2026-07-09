local M = {}

function M.patch(file_patch)
  local buf = M.buffer(file_patch.file)

  if not buf then
    vim.cmd("edit " .. vim.fn.fnameescape(file_patch.file))
    buf = vim.api.nvim_get_current_buf()
  end

  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local ok, next_lines = pcall(M.apply_diff, lines, file_patch.diff)

  if not ok then
    return false, next_lines
  end

  vim.api.nvim_buf_set_lines(buf, 0, -1, false, next_lines)

  return true, nil
end

function M.buffer(file)
  local target = vim.fn.fnamemodify(file, ":p")

  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and vim.fn.fnamemodify(vim.api.nvim_buf_get_name(buf), ":p") == target then
      return buf
    end
  end

  return nil
end

function M.apply_diff(source, diff)
  local output = vim.deepcopy(source)
  local offset = 0

  for line in diff:gmatch("[^\n]+") do
    local old_start = line:match("^@@ %-(%d+)")

    if old_start then
      offset = tonumber(old_start) - 1
    elseif line:sub(1, 1) == " " then
      offset = offset + 1
    elseif line:sub(1, 1) == "-" then
      local expected = line:sub(2)

      if output[offset + 1] ~= expected then
        error("Patch context mismatch")
      end

      table.remove(output, offset + 1)
    elseif line:sub(1, 1) == "+" then
      table.insert(output, offset + 1, line:sub(2))
      offset = offset + 1
    end
  end

  return output
end

return M
