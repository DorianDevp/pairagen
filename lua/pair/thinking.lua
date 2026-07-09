local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}
local uv = vim.uv or vim.loop

local width = 60
local height = 18
local arms = 3
local density = 540
local radius = 8

function M.start(label, session_id)
  if not config.values.thinking.enabled then
    return nil
  end

  M.stop(false)

  local request_id = tostring(uv.hrtime())

  state.thinking_frame = 0
  state.thinking_request_id = request_id
  state.thinking_session_id = session_id

  local lines = M.lines(label or "Thinking", state.thinking_frame)
  local buf, win = ui.render(state.card_buf, state.card_win, lines, {
    width = width + 4,
    height = #lines,
    border = config.values.card.border,
    row = math.floor(vim.o.lines * 0.2),
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
    M.tick(label or "Thinking", request_id)
  end))

  return request_id
end

function M.tick(label, request_id)
  if not M.current(request_id) then
    M.stop(false)

    return
  end

  if not state.card_buf or not vim.api.nvim_buf_is_valid(state.card_buf) then
    M.stop(false)

    return
  end

  if not state.card_win or not vim.api.nvim_win_is_valid(state.card_win) then
    M.stop(false)

    return
  end

  state.thinking_frame = (state.thinking_frame or 0) + 1

  ui.render(state.card_buf, state.card_win, M.lines(label, state.thinking_frame), {
    width = width + 4,
    height = height + 5,
    border = config.values.card.border,
    row = math.floor(vim.o.lines * 0.2),
  })
end

function M.current(request_id)
  if request_id ~= state.thinking_request_id then
    return false
  end

  if state.thinking_session_id and state.session_id ~= state.thinking_session_id then
    return false
  end

  return true
end

function M.lines(label, frame)
  local lines = {
    label,
    string.rep("-", 18),
  }

  for _, line in ipairs(M.frame(frame)) do
    table.insert(lines, line)
  end

  table.insert(lines, "")
  table.insert(lines, "[q] Hide")

  return lines
end

function M.frame(frame)
  local buffer = M.buffer()
  local angle_offset = frame * 0.05

  for index = 1, density do
    local arm = index % arms
    local factor = index / density
    local distance = factor * radius
    local theta = factor * 5.0 + arm * ((2 * math.pi) / arms) + angle_offset
    local x = math.floor(width / 2 + math.cos(theta) * distance * 2)
    local y = math.floor(height / 2 + math.sin(theta) * distance)
    local char = M.char(distance)

    M.put(buffer, x, y, char)
  end

  return M.output(buffer)
end

function M.buffer()
  local buffer = {}

  for y = 1, height do
    buffer[y] = {}

    for x = 1, width do
      buffer[y][x] = " "
    end
  end

  return buffer
end

function M.char(distance)
  if distance < radius * 0.2 then
    return "@"
  end

  if distance < radius * 0.5 then
    return "#"
  end

  if distance < radius * 0.8 then
    return "*"
  end

  return "."
end

function M.put(buffer, x, y, char)
  if x < 1 or x > width or y < 1 or y > height then
    return
  end

  if M.weight(buffer[y][x]) > M.weight(char) then
    return
  end

  buffer[y][x] = char
end

function M.weight(char)
  local weights = {
    [" "] = 0,
    ["."] = 1,
    ["*"] = 2,
    ["#"] = 3,
    ["@"] = 4,
  }

  return weights[char] or 0
end

function M.output(buffer)
  local output = {}

  for y = 1, height do
    output[y] = "  " .. table.concat(buffer[y])
  end

  return output
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
  state.thinking_request_id = nil
  state.thinking_session_id = nil

  if close then
    ui.close(state.card_win)
    state.card_win = nil
  end
end

return M
