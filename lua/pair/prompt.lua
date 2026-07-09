local config = require("pair.config")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}

function M.open(mode)
  M.open_for({
    title = " Pair Prompt ",
    footer = " Ctrl-s submit  Esc normal  q close ",
    submit = function(text)
      require("pair").start(text, mode)
    end,
  })
end

function M.reply()
  M.open_for({
    title = " Pair Reply ",
    footer = " Ctrl-s send  Esc normal  q close ",
    submit = function(text)
      require("pair").reply(text)
    end,
  })
end

function M.open_for(opts)
  M.close()

  local size = M.size()
  local row = math.floor((vim.o.lines - size.outer_height) * 0.28)
  local col = math.floor((vim.o.columns - size.outer_width) / 2)
  local frame_buf = vim.api.nvim_create_buf(false, true)
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
  })

  state.prompt_frame_buf = frame_buf
  state.prompt_frame_win = frame_win

  vim.bo[frame_buf].bufhidden = "wipe"
  vim.bo[frame_buf].modifiable = false

  local buf = vim.api.nvim_create_buf(false, true)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    row = row + config.values.prompt.padding_y,
    col = col + config.values.prompt.padding_x,
    width = size.inner_width,
    height = size.inner_height,
    style = "minimal",
    border = "none",
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
  local outer_height = math.min(
    config.values.prompt.height,
    math.max(vim.o.lines - 6, 5)
  )
  local inner_width = math.max(outer_width - config.values.prompt.padding_x * 2, 20)
  local inner_height = math.max(outer_height - config.values.prompt.padding_y * 2, 1)

  return {
    outer_width = outer_width,
    outer_height = outer_height,
    inner_width = inner_width,
    inner_height = inner_height,
  }
end

function M.width()
  local configured = config.values.prompt.width or 96
  local limit = math.max(vim.o.columns - 8, 24)

  return math.min(configured, limit)
end

return M
