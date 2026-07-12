local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}

function M.open(mode)
  local source = require("pair.context").capture()

  M.open_for({
    title = M.title("Prompt"),
    footer = " Ctrl-s submit  Esc normal  q close ",
    submit = function(text)
      require("pair").start(text, mode, source)
    end,
  })
end

function M.reply()
  M.open_for({
    title = M.title("Reply"),
    footer = " Ctrl-s send  Esc normal  q close ",
    submit = function(text)
      require("pair").reply(text)
    end,
  })
end

function M.title(kind)
  local agent = config.agent()
  local model = config.model()
  local active = model and model ~= "" and (agent .. " / " .. model) or (agent .. " / default")

  return string.format(" Pair %s · %s ", kind, active)
end

function M.open_for(opts)
  M.close()

  local size = M.size()
  local position = M.position(size)
  local row = position.row
  local col = position.col
  local frame_buf = vim.api.nvim_create_buf(false, true)
  local zindex = config.values.prompt.zindex or 200
  local frame_win = vim.api.nvim_open_win(frame_buf, false, {
    relative = "editor",
    row = row,
    col = col,
    width = size.outer_width,
    height = size.outer_height,
    style = "minimal",
    border = config.values.prompt.border,
    title = opts.title,
    title_pos = "left",
    footer = opts.footer,
    footer_pos = "right",
    zindex = zindex,
  })

  state.prompt_frame_buf = frame_buf
  state.prompt_frame_win = frame_win

  vim.bo[frame_buf].bufhidden = "wipe"
  vim.bo[frame_buf].modifiable = false

  local buf = vim.api.nvim_create_buf(false, true)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    row = row + size.padding_y,
    col = col + size.padding_x,
    width = size.inner_width,
    height = size.inner_height,
    style = "minimal",
    border = "none",
    zindex = zindex + 1,
  })

  state.prompt_buf = buf
  state.prompt_win = win

  M.prepare(buf, win)
  M.bind(buf, opts.submit)

  vim.cmd("startinsert")
end

function M.prepare(buf, win)
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "markdown"
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true
  vim.wo[win].cursorline = true
  vim.wo[win].number = false
  vim.wo[win].relativenumber = false
  vim.wo[win].signcolumn = "no"
end

function M.bind(buf, submit)
  vim.keymap.set({ "i", "n" }, "<C-s>", function()
    M.submit(buf, submit)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "<CR>", function()
    M.submit(buf, submit)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "q", function()
    M.close()
  end, { buffer = buf, nowait = true, silent = true })
end

function M.submit(buf, submit)
  local text = M.text(buf)

  if text == "" then
    return
  end

  if vim.fn.mode():match("^[iR]") then
    vim.cmd("stopinsert")
  end

  M.close()
  submit(text)
end

function M.close()
  ui.close(state.prompt_win)
  ui.close(state.prompt_frame_win)

  state.prompt_win = nil
  state.prompt_buf = nil
  state.prompt_frame_win = nil
  state.prompt_frame_buf = nil
end

function M.text(buf)
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)

  return vim.trim(table.concat(lines, "\n"))
end

function M.size()
  local outer_width = M.width()
  local viewport = ui.viewport()
  local outer_height = math.min(config.values.prompt.height, math.max(viewport.height - 2, 1))
  local padding_x = math.min(config.values.prompt.padding_x, math.floor((outer_width - 1) / 2))
  local padding_y = math.min(config.values.prompt.padding_y, math.floor((outer_height - 1) / 2))
  local inner_width = math.max(outer_width - padding_x * 2, 1)
  local inner_height = math.max(outer_height - padding_y * 2, 1)

  return {
    outer_width = outer_width,
    outer_height = outer_height,
    inner_width = inner_width,
    inner_height = inner_height,
    padding_x = padding_x,
    padding_y = padding_y,
  }
end

function M.position(size)
  local viewport = ui.viewport()
  local cursor = M.cursor_screen_position()
  local total_width = size.outer_width + 2
  local total_height = size.outer_height + 2
  local max_row = math.max(viewport.height - total_height, 0)
  local max_col = math.max(viewport.width - total_width, 0)
  local below = cursor.row + 1
  local above = cursor.row - total_height
  local row

  if below <= max_row then
    row = below
  elseif above >= 0 then
    row = above
  else
    row = ui.clamp(below, 0, max_row)
  end

  return {
    row = ui.clamp(row, 0, max_row),
    col = ui.clamp(cursor.col - math.floor(total_width / 2), 0, max_col),
  }
end

function M.cursor_screen_position()
  local win = vim.api.nvim_get_current_win()
  local cursor = vim.api.nvim_win_get_cursor(win)
  local position = vim.fn.screenpos(win, cursor[1], cursor[2] + 1)

  if position.row == 0 or position.col == 0 then
    local viewport = ui.viewport()
    return {
      row = math.floor(viewport.height / 2),
      col = math.floor(viewport.width / 2),
    }
  end

  return {
    row = position.row - 1,
    col = position.col - 1,
  }
end

function M.width()
  local configured = config.values.prompt.width or 96
  local limit = math.max(ui.viewport().width - 2, 1)

  return math.min(configured, limit)
end

return M
