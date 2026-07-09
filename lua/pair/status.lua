local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}

function M.show()
  if not state.session_id then
    return
  end

  local text = "Pair " .. config.agent() .. " active - <leader>pr show"
  local width = math.min(#text + 2, math.max(vim.o.columns - 4, 20))
  local row = math.max(vim.o.lines - 4, 0)
  local col = math.max(vim.o.columns - width - 2, 0)
  local buf, win = ui.render(state.status_buf, state.status_win, { text }, {
    width = width,
    height = 1,
    row = row,
    col = col,
    border = "single",
    enter = false,
  })

  state.status_buf = buf
  state.status_win = win

  vim.keymap.set("n", "r", function()
    require("pair").resume()
  end, { buffer = buf, nowait = true, silent = true })
end

function M.hide()
  ui.close(state.status_win)

  state.status_win = nil
end

return M
