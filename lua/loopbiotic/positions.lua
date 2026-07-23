-- Observable window geometry. Tracked windows are subjects: whenever their
-- screen position, size, or cursor changes, a snapshot is emitted to every
-- subscribed observer. The plugin root subscribes once and forwards each
-- emission into the JSONL session trace, so geometry history stays bound to
-- the session like every other event.

local M = {}

local observers = {}
local tracked = {}

local function snapshot(win)
  if not (win and vim.api.nvim_win_is_valid(win)) then
    return nil
  end
  local position = vim.api.nvim_win_get_position(win)
  local geometry = {
    row = position[1],
    col = position[2],
    width = vim.api.nvim_win_get_width(win),
    height = vim.api.nvim_win_get_height(win),
  }
  local ok, cursor = pcall(vim.api.nvim_win_get_cursor, win)
  if ok then
    geometry.cursor = { line = cursor[1], col = cursor[2] }
  end
  return geometry
end

local function notify(event)
  for _, observer in ipairs(observers) do
    pcall(observer, event)
  end
end

local function changed_fields(last, current)
  last = last or {}
  local changed = {}
  if current.row ~= last.row or current.col ~= last.col then
    table.insert(changed, "position")
  end
  if current.width ~= last.width or current.height ~= last.height then
    table.insert(changed, "size")
  end
  local last_cursor = last.cursor or {}
  local cursor = current.cursor or {}
  if cursor.line ~= last_cursor.line or cursor.col ~= last_cursor.col then
    table.insert(changed, "cursor")
  end
  return changed
end

---@param observer fun(event: table)
---@return fun() unsubscribe
function M.subscribe(observer)
  table.insert(observers, observer)
  return function()
    for index, existing in ipairs(observers) do
      if existing == observer then
        table.remove(observers, index)
        return
      end
    end
  end
end

-- Register a window as a subject. Re-tracking an already known window only
-- re-emits its current geometry, so render paths that reuse a handle can call
-- this unconditionally.
function M.track(role, win)
  if tracked[win] then
    M.emit(win)
    return
  end
  local geometry = snapshot(win)
  if not geometry then
    return
  end
  tracked[win] = { role = role, last = geometry }
  notify({ kind = "opened", role = role, win = win, geometry = geometry })
end

-- Emit the window's geometry when anything changed since the last emission.
function M.emit(win)
  local entry = tracked[win]
  if not entry then
    return
  end
  local geometry = snapshot(win)
  if not geometry then
    return
  end
  local changed = changed_fields(entry.last, geometry)
  if #changed == 0 then
    return
  end
  entry.last = geometry
  notify({ kind = "changed", role = entry.role, win = win, changed = changed, geometry = geometry })
end

function M.emit_all()
  for win in pairs(tracked) do
    M.emit(win)
  end
end

function M.closed(win)
  local entry = tracked[win]
  if not entry then
    return
  end
  tracked[win] = nil
  notify({ kind = "closed", role = entry.role, win = win, geometry = entry.last })
end

function M.setup()
  local group = vim.api.nvim_create_augroup("LoopbioticPositions", { clear = true })
  -- The tracked set is a handful of plugin windows, so a full re-scan with
  -- change deduplication is cheaper than decoding per-event window payloads.
  vim.api.nvim_create_autocmd({ "WinResized", "WinScrolled" }, {
    group = group,
    callback = function()
      M.emit_all()
    end,
  })
  vim.api.nvim_create_autocmd({ "CursorMoved", "CursorMovedI" }, {
    group = group,
    callback = function()
      M.emit(vim.api.nvim_get_current_win())
    end,
  })
  vim.api.nvim_create_autocmd("WinClosed", {
    group = group,
    callback = function(args)
      local win = tonumber(args.match)
      if win then
        M.closed(win)
      end
    end,
  })
end

-- Drop every subject and observer; test isolation only.
function M.reset()
  observers = {}
  tracked = {}
end

return M
