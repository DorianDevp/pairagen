local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}
local uv = vim.uv or vim.loop
local frame_count = 24

function M.start(label)
  if not config.values.thinking.enabled then
    return
  end

  M.stop(false)

  state.thinking_frame = 1

  local lines = M.lines(label or "Thinking", state.thinking_frame)
  local buf, win = ui.render(state.card_buf, state.card_win, lines, {
    width = 38,
    height = #lines,
    border = config.values.card.border,
    row = math.floor(vim.o.lines * 0.3),
  })

  state.card_buf = buf
  state.card_win = win
  state.thinking_buf = buf
  state.thinking_win = win

  vim.keymap.set("n", "q", function()
    M.stop(true)
  end, { buffer = buf, nowait = true, silent = true })

  state.thinking_timer = uv.new_timer()
  state.thinking_timer:start(0, config.values.thinking.interval, vim.schedule_wrap(function()
    M.tick(label or "Thinking")
  end))
end

function M.tick(label)
  if not state.card_buf or not vim.api.nvim_buf_is_valid(state.card_buf) then
    M.stop(false)

    return
  end

  if not state.card_win or not vim.api.nvim_win_is_valid(state.card_win) then
    M.stop(false)

    return
  end

  state.thinking_frame = ((state.thinking_frame or 1) % frame_count) + 1

  ui.render(state.card_buf, state.card_win, M.lines(label, state.thinking_frame), {
    width = 38,
    height = 13,
    border = config.values.card.border,
    row = math.floor(vim.o.lines * 0.3),
  })
end

function M.lines(label, index)
  local lines = {
    label,
    string.rep("-", 24),
  }

  for _, line in ipairs(M.frame(index)) do
    table.insert(lines, line)
  end

  table.insert(lines, "")
  table.insert(lines, "[q] Hide")

  return lines
end

function M.frame(index)
  local width = 25
  local height = 9
  local center_x = 13
  local center_y = 5
  local phase = ((index - 1) / frame_count) * math.pi * 2
  local grid = M.grid(width, height)

  for arm = 0, 2 do
    for step = 1, 30 do
      local radius = step * 0.22
      local angle = step * 0.42 + phase + arm * math.pi * 0.66
      local x = math.floor(center_x + math.cos(angle) * radius * 1.7 + 0.5)
      local y = math.floor(center_y + math.sin(angle) * radius * 0.75 + 0.5)
      local char = M.char(step)

      M.put(grid, x, y, char)
    end
  end

  M.put(grid, center_x, center_y, "*")

  return M.grid_lines(grid)
end

function M.grid(width, height)
  local grid = {}

  for y = 1, height do
    grid[y] = {}

    for x = 1, width do
      grid[y][x] = " "
    end
  end

  return grid
end

function M.put(grid, x, y, char)
  if not grid[y] or not grid[y][x] then
    return
  end

  grid[y][x] = char
end

function M.char(step)
  local chars = { ".", "'", "o", "O", "@" }
  local index = math.min(#chars, math.floor(step / 7) + 1)

  return chars[index]
end

function M.grid_lines(grid)
  local lines = {}

  for _, row in ipairs(grid) do
    table.insert(lines, "  " .. table.concat(row))
  end

  return lines
end

function M.stop(close)
  if state.thinking_timer then
    if not state.thinking_timer:is_closing() then
      state.thinking_timer:stop()
      state.thinking_timer:close()
    end
  end

  state.thinking_timer = nil
  state.thinking_win = nil
  state.thinking_buf = nil

  if close then
    ui.close(state.card_win)
    state.card_win = nil
  end
end

return M
