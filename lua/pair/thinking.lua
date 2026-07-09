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
    width = 56,
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
    width = 56,
    height = 17,
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
  local width = 41
  local height = 13
  local center_x = 21
  local center_y = 7
  local phase = ((index - 1) / frame_count) * math.pi * 2
  local grid = M.grid(width, height)

  M.stars(grid, index)

  for step = 1, 84 do
    if step < 48 or step % 3 == 0 then
      M.arm(grid, center_x, center_y, step, phase)
    end
  end

  M.core(grid, center_x, center_y)

  return M.grid_lines(grid)
end

function M.arm(grid, center_x, center_y, step, phase)
    local radius = 0.095 * step
    local angle = 0.44 * step + phase

    M.put_spiral(grid, center_x, center_y, radius, angle, M.char(step, angle))
    M.put_spiral(
      grid,
      center_x,
      center_y,
      radius * 0.86,
      angle + math.pi,
      M.char(step + 10, angle + math.pi)
    )
end

function M.put_spiral(grid, center_x, center_y, radius, angle, char)
  local x = math.floor(center_x + math.cos(angle) * radius * 2.25 + 0.5)
  local y = math.floor(center_y + math.sin(angle) * radius * 0.9 + 0.5)

  M.put(grid, x, y, char)
end

function M.stars(grid, index)
  local stars = {
    { 3, 2 },
    { 38, 2 },
  }

  for star_index, star in ipairs(stars) do
    if ((index + star_index) % 4) ~= 0 then
      M.put(grid, star[1], star[2], ".")
    end
  end
end

function M.core(grid, x, y)
  M.put(grid, x - 1, y, "o")
  M.put(grid, x, y, "@")
  M.put(grid, x + 1, y, "o")
  M.put(grid, x, y - 1, "o")
  M.put(grid, x, y + 1, "o")
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

  if M.weight(grid[y][x]) > M.weight(char) then
    return
  end

  grid[y][x] = char
end

function M.char(step, angle)
  if step < 10 then
    return "o"
  end

  if step < 16 then
    return "*"
  end

  return M.stroke(angle + math.pi * 0.5)
end

function M.stroke(angle)
  local x = math.cos(angle)
  local y = math.sin(angle)

  if math.abs(x) > math.abs(y) * 2 then
    return "-"
  end

  if math.abs(y) > math.abs(x) * 2 then
    return "|"
  end

  if x * y > 0 then
    return "\\"
  end

  return "/"
end

function M.weight(char)
  local weights = {
    [" "] = 0,
    ["."] = 1,
    [","] = 2,
    [":"] = 3,
    ["*"] = 4,
    ["o"] = 5,
    ["@"] = 6,
    ["-"] = 3,
    ["|"] = 3,
    ["/"] = 3,
    ["\\"] = 3,
  }

  return weights[char] or 0
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
