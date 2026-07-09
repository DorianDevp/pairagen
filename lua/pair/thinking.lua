local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}
local uv = vim.uv or vim.loop
local frames = { "|", "/", "-", "\\" }

function M.start(label, session_id)
  if not config.values.thinking.enabled then
    return nil
  end

  M.stop(false)

  local request_id = tostring(uv.hrtime())

  state.thinking_frame = 1
  state.thinking_request_id = request_id
  state.thinking_session_id = session_id

  local lines = M.lines(label or "Thinking", state.thinking_frame)
  local buf, win = ui.render(state.card_buf, state.card_win, lines, {
    width = 24,
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

  state.thinking_frame = ((state.thinking_frame or 1) % #frames) + 1

  ui.render(state.card_buf, state.card_win, M.lines(label, state.thinking_frame), {
    width = 24,
    height = 5,
    border = config.values.card.border,
    row = math.floor(vim.o.lines * 0.3),
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

function M.lines(label, index)
  return {
    label,
    string.rep("-", 18),
    frames[index] .. " loading",
    "",
    "[q] Hide",
  }
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
