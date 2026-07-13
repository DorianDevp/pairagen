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
  require("loopbiotic.state").source_buf = buf

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
  local hunk = M.parse_hunk(diff)
  if hunk.old_len == 0 and #source == 1 and source[1] == "" then
    source = {}
  end
  local output = vim.deepcopy(source)
  local offset = M.resolve_start(source, hunk)

  for _, line in ipairs(hunk.lines) do
    if line.kind == "context" then
      if output[offset + 1] ~= line.text then
        error("Patch context changed while applying")
      end
      offset = offset + 1
    elseif line.kind == "remove" then
      if output[offset + 1] ~= line.text then
        error("Patch context changed while applying")
      end
      table.remove(output, offset + 1)
    elseif line.kind == "add" then
      table.insert(output, offset + 1, line.text)
      offset = offset + 1
    end
  end

  return output
end

function M.parse_hunk(diff)
  local hunk = { old_start = nil, old_len = nil, lines = {}, old_lines = {} }

  for line in diff:gmatch("[^\n]+") do
    local old_start, old_len = line:match("^@@ %-(%d+),(%d+)")
    if not old_start then
      old_start = line:match("^@@ %-(%d+)")
      old_len = old_start and "1" or nil
    end

    if old_start then
      if hunk.old_start then
        error("Patch must contain exactly one hunk")
      end
      hunk.old_start = tonumber(old_start)
      hunk.old_len = tonumber(old_len)
    elseif hunk.old_start then
      local prefix = line:sub(1, 1)
      local text = line:sub(2)
      local kind = prefix == " " and "context" or prefix == "-" and "remove" or prefix == "+" and "add"

      if not kind then
        error("Invalid patch line")
      end

      table.insert(hunk.lines, { kind = kind, text = text })
      if kind ~= "add" then
        table.insert(hunk.old_lines, text)
      end
    end
  end

  if not hunk.old_start or (#hunk.old_lines == 0 and hunk.old_len ~= 0) then
    error("Patch has no source context")
  end

  return hunk
end

function M.resolve_start(source, hunk)
  if hunk.old_len == 0 then
    if #source == 0 or (#source == 1 and source[1] == "") then
      return 0
    end
    error("A patch without source context can only create an empty file")
  end

  local expected = hunk.old_start - 1
  if M.matches_at(source, expected, hunk.old_lines) then
    return expected
  end

  local matches = {}
  for start = 0, #source - #hunk.old_lines do
    if M.matches_at(source, start, hunk.old_lines) then
      table.insert(matches, start)
    end
  end

  if #matches == 0 then
    error("Patch context was not found in the current buffer")
  end
  if #matches > 1 then
    error("Patch context is ambiguous in the current buffer")
  end

  return matches[1]
end

function M.matches_at(source, start, expected)
  for index, line in ipairs(expected) do
    if source[start + index] ~= line then
      return false
    end
  end

  return true
end

return M
