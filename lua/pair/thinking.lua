local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}
local uv = vim.uv or vim.loop

local frames = {
  "    .      \n  .   *    \n    o      ",
  "     .     \n  .   *    \n   o       ",
  "      .    \n  .   *    \n  o        ",
  "       .   \n  .   *    \n o         ",
  "        .  \n  .   *    \no          ",
  "         . \n  .   *   o\n           ",
  "          .\n  .   *  o \n           ",
  "           \n  .   * o .\n           ",
  "           \n  .   *o  .\n           ",
  "           \n  .  o*   .\n           ",
  "           \n  . o *   .\n           ",
  "           \n  o  *   . \n           ",
  "           \no    *   . \n           ",
  " o         \n     *   . \n           ",
  "  o        \n     *   . \n.          ",
  "   o       \n     *   . \n .         ",
  "    o      \n     *   . \n  .        ",
  "     o     \n     *   . \n   .       ",
  "      o    \n     *   . \n    .      ",
  "       o   \n     *   . \n     .     ",
  "        o  \n     *   . \n      .    ",
  "         o \n     *   . \n       .   ",
  "          o\n     *   . \n        .  ",
  "           \n     *   .o\n         . ",
}

function M.start(label)
  if not config.values.thinking.enabled then
    return
  end

  M.stop()

  local lines = M.lines(label or "Thinking", 1)
  local buf, win = ui.float(lines, {
    width = 34,
    height = #lines,
    border = config.values.card.border,
    row = math.floor(vim.o.lines * 0.3),
  })

  state.thinking_buf = buf
  state.thinking_win = win
  state.thinking_frame = 1

  vim.keymap.set("n", "q", function()
    M.stop()
  end, { buffer = buf, nowait = true, silent = true })

  vim.bo[buf].modifiable = false

  state.thinking_timer = uv.new_timer()
  state.thinking_timer:start(0, config.values.thinking.interval, vim.schedule_wrap(function()
    M.tick(label or "Thinking")
  end))
end

function M.tick(label)
  if not state.thinking_buf or not vim.api.nvim_buf_is_valid(state.thinking_buf) then
    M.stop()

    return
  end

  state.thinking_frame = ((state.thinking_frame or 1) % #frames) + 1

  vim.bo[state.thinking_buf].modifiable = true
  vim.api.nvim_buf_set_lines(state.thinking_buf, 0, -1, false, M.lines(label, state.thinking_frame))
  vim.bo[state.thinking_buf].modifiable = false
end

function M.lines(label, index)
  local lines = {
    label,
    string.rep("-", 24),
  }

  for _, line in ipairs(vim.split(frames[index], "\n", { plain = true })) do
    table.insert(lines, "  " .. line)
  end

  table.insert(lines, "")
  table.insert(lines, "[q] Hide")

  return lines
end

function M.stop()
  if state.thinking_timer then
    if not state.thinking_timer:is_closing() then
      state.thinking_timer:stop()
      state.thinking_timer:close()
    end
  end

  state.thinking_timer = nil
  ui.close(state.thinking_win)
  state.thinking_win = nil
  state.thinking_buf = nil
end

return M
