-- Small shared helpers with no dependencies on other loopbiotic modules.
local M = {}

-- The location a card points at, in priority order: an explicit next move,
-- then evidence, then a plain location.
---@param card LoopbioticCard|table
---@return table|nil location { file, line, column, ... } or nil
function M.card_location(card)
  if type(card.next_move) == "table" and card.next_move.kind == "open_location" then
    return card.next_move
  end
  if type(card.evidence) == "table" then
    return card.evidence
  end
  if type(card.location) == "table" then
    return card.location
  end

  return nil
end

-- Compact node label for one goal observation, e.g. "[H1*x2]".
---@param observation { kind?: string, active?: boolean, occurrences?: integer }
---@param index integer
---@return string
function M.observation_node(observation, index)
  local kind = observation.kind == "hypothesis" and "H" or observation.kind == "signal" and "S" or "F"
  local active = observation.active and "*" or "."
  local repeats = (observation.occurrences or 1) > 1 and "x" .. observation.occurrences or ""

  return string.format("[%s%d%s%s]", kind, index, active, repeats)
end

-- Clamp a 1-indexed line / 0-indexed column pair to positions that exist in
-- buf, so it is always safe to pass to nvim_win_set_cursor. Card locations
-- come from the agent and draft cursors are computed against post-apply
-- content, so both can point past the end of the real buffer (for example a
-- hunk that appends to a one-line barrel file).
---@param buf integer
---@param line integer|nil
---@param column integer|nil
---@return integer[] pos { line, column }
function M.clamp_cursor(buf, line, column)
  local count = vim.api.nvim_buf_line_count(buf)

  return { math.min(math.max(line or 1, 1), math.max(count, 1)), math.max(column or 0, 0) }
end

-- Whether file lies inside root (default: the current working directory).
-- Symlinks are resolved when the paths exist.
---@param file string|nil
---@param root string|nil
---@return boolean
function M.in_workspace(file, root)
  if type(file) ~= "string" or file == "" then
    return false
  end

  root = root or vim.fn.getcwd()
  root = vim.uv.fs_realpath(root) or vim.fs.normalize(vim.fn.fnamemodify(root, ":p"):gsub("/$", ""))
  local target = vim.fn.fnamemodify(file, ":p")
  target = vim.uv.fs_realpath(target) or vim.fs.normalize(target)

  return target == root or vim.startswith(target, root .. "/")
end

return M
