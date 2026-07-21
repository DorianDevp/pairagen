-- Small shared helpers with no load-time dependencies on other loopbiotic
-- modules (guard requires the log module lazily, when an error is reported).
local M = {}

-- Labels whose internal error has already been reported to the user this
-- session. Every error keeps being logged; the notification fires once.
local guard_notified = {}

local function pack(...)
  return { n = select("#", ...), ... }
end

-- Error boundary for UI entry points (RPC dispatch, card/diff rendering,
-- command and keymap callbacks). Returns a wrapped function that xpcalls fn:
-- on success it passes fn's results through; on an uncaught error it logs a
-- "client_error" event with the traceback, notifies once per label per
-- session, and returns nil — so a client-side bug cannot unwind into Neovim
-- and kill the surrounding daemon session.
---@param label string
---@param fn function
---@return function
function M.guard(label, fn)
  return function(...)
    local results = pack(xpcall(fn, debug.traceback, ...))
    if results[1] then
      return unpack(results, 2, results.n)
    end

    require("loopbiotic.log").event("client_error", {
      label = label,
      traceback = tostring(results[2]),
    })

    if not guard_notified[label] then
      guard_notified[label] = true
      vim.notify(
        string.format("Loopbiotic internal error in %s (see :LoopbioticLog); session preserved", label),
        vim.log.levels.ERROR,
        { title = "Loopbiotic" }
      )
    end

    return nil
  end
end

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

-- Neovim preserves JSON null as the truthy userdata vim.NIL. Protocol nulls
-- are optional values, so leaving them in decoded messages makes ordinary
-- `if value then` checks enter table/string code with a userdata instead.
---@param value any
---@return any
function M.normalize_json_nulls(value)
  if value == vim.NIL then
    return nil
  end
  if type(value) ~= "table" then
    return value
  end

  for key, item in pairs(value) do
    if item == vim.NIL then
      value[key] = nil
    else
      value[key] = M.normalize_json_nulls(item)
    end
  end

  return value
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

-- Workspace-relative form of a path, compatible with Neovim 0.10 where
-- vim.fs.relpath is not available yet. Returns nil outside the root.
---@param root string
---@param file string
---@return string|nil
function M.relative_path(root, file)
  root = vim.uv.fs_realpath(root) or vim.fs.normalize(vim.fn.fnamemodify(root, ":p"):gsub("/$", ""))
  local target = vim.uv.fs_realpath(file) or vim.fs.normalize(vim.fn.fnamemodify(file, ":p"))
  if target == root then
    return "."
  end
  if not vim.startswith(target, root .. "/") then
    return nil
  end
  return target:sub(#root + 2)
end

return M
