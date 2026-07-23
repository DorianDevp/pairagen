return function(t)
  local positions = require("loopbiotic.positions")
  local state = require("loopbiotic.state")
  local surfaces = require("loopbiotic.surfaces")

  local function collect()
    local events = {}
    local unsubscribe = positions.subscribe(function(event)
      table.insert(events, vim.deepcopy(event))
    end)
    return events, unsubscribe
  end

  local function open_float(row, col)
    local buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, { "one", "two", "three" })
    local win = vim.api.nvim_open_win(buf, false, {
      relative = "editor",
      row = row,
      col = col,
      width = 20,
      height = 3,
      style = "minimal",
    })
    return win
  end

  local function close_extra_windows()
    for _, win in ipairs(vim.api.nvim_list_wins()) do
      if #vim.api.nvim_list_wins() > 1 then
        pcall(vim.api.nvim_win_close, win, true)
      end
    end
  end

  t.test("positions.track emits an opened snapshot with geometry and cursor", function()
    positions.reset()
    local events = collect()
    local win = open_float(2, 4)

    positions.track("agent", win)

    t.eq(#events, 1, "event count")
    t.eq(events[1].kind, "opened")
    t.eq(events[1].role, "agent")
    t.eq(events[1].win, win)
    t.eq(events[1].geometry.row, 2, "screen row")
    t.eq(events[1].geometry.width, 20, "width")
    t.eq(events[1].geometry.cursor, { line = 1, col = 0 }, "cursor")

    positions.reset()
    close_extra_windows()
  end)

  t.test("positions.emit is silent while geometry is unchanged", function()
    positions.reset()
    local events = collect()
    local win = open_float(2, 4)
    positions.track("agent", win)

    positions.emit(win)
    positions.emit(win)

    t.eq(#events, 1, "only the opened emission")

    positions.reset()
    close_extra_windows()
  end)

  t.test("moving a tracked float emits a position change", function()
    positions.reset()
    local events = collect()
    local win = open_float(2, 4)
    positions.track("agent", win)

    vim.api.nvim_win_set_config(win, { relative = "editor", row = 6, col = 10 })
    positions.emit(win)

    t.eq(#events, 2, "opened + changed")
    t.eq(events[2].kind, "changed")
    t.eq(events[2].changed, { "position" })
    t.eq(events[2].geometry.row, 6, "new screen row")
    t.eq(events[2].geometry.col, 10, "new screen col")

    positions.reset()
    close_extra_windows()
  end)

  t.test("cursor movement inside a tracked window emits a cursor change", function()
    positions.reset()
    local events = collect()
    local win = open_float(2, 4)
    positions.track("prompt", win)

    vim.api.nvim_win_set_cursor(win, { 3, 1 })
    positions.emit(win)

    t.eq(#events, 2, "opened + changed")
    t.eq(events[2].changed, { "cursor" })
    t.eq(events[2].geometry.cursor, { line = 3, col = 1 }, "cursor")

    positions.reset()
    close_extra_windows()
  end)

  t.test("closing a tracked window emits closed and retires the subject", function()
    positions.reset()
    positions.setup()
    local events = collect()
    local win = open_float(2, 4)
    positions.track("agent", win)

    vim.api.nvim_win_close(win, true)

    t.eq(events[#events].kind, "closed")
    t.eq(events[#events].win, win)
    positions.emit(win)
    t.eq(events[#events].kind, "closed", "no emission after close")

    positions.reset()
    close_extra_windows()
  end)

  t.test("unsubscribed observers stop receiving emissions", function()
    positions.reset()
    local events, unsubscribe = collect()
    local win = open_float(2, 4)
    positions.track("agent", win)
    t.eq(#events, 1)

    unsubscribe()
    vim.api.nvim_win_set_config(win, { relative = "editor", row = 8, col = 2 })
    positions.emit(win)

    t.eq(#events, 1, "no emission after unsubscribe")

    positions.reset()
    close_extra_windows()
  end)

  t.test("cursor emissions on two splits stay bound to the tracked split", function()
    positions.reset()
    close_extra_windows()
    local first = vim.api.nvim_get_current_win()
    vim.cmd("split")
    local second = vim.api.nvim_get_current_win()
    vim.api.nvim_buf_set_lines(vim.api.nvim_win_get_buf(second), 0, -1, false, { "alpha", "beta" })

    local events = collect()
    positions.track("agent", second)
    vim.api.nvim_win_set_cursor(second, { 2, 0 })
    positions.emit(second)
    positions.emit(first)

    t.eq(#events, 2, "opened + changed, nothing for the untracked first split")
    for _, event in ipairs(events) do
      t.eq(event.win, second, "emission window")
    end

    positions.reset()
    close_extra_windows()
  end)

  t.test("surfaces windows act as subjects across open, relayout, and close", function()
    positions.reset()
    positions.setup()
    close_extra_windows()
    state.reset()
    local events = collect()

    local spec = {
      row = 4,
      col = 6,
      outer_width = 40,
      outer_height = 6,
      inner_width = 36,
      inner_height = 4,
      padding_x = 2,
      padding_y = 1,
      title = " Prompt ",
      footer = " mode ",
    }
    surfaces.open_prompt(spec)

    local roles = {}
    for _, event in ipairs(events) do
      roles[event.role] = event.kind
    end
    t.eq(roles.prompt_frame, "opened", "frame tracked")
    t.eq(roles.prompt, "opened", "content tracked")

    local before = #events
    local moved = vim.deepcopy(spec)
    moved.row = 8
    moved.col = 10
    surfaces.relayout_prompt(moved)

    local seen_position_change = false
    for index = before + 1, #events do
      if events[index].kind == "changed" and vim.tbl_contains(events[index].changed, "position") then
        seen_position_change = true
      end
    end
    t.eq(seen_position_change, true, "relayout emitted a position change")

    surfaces.close_prompt({ focus_agent = false })
    local closed = {}
    for _, event in ipairs(events) do
      if event.kind == "closed" then
        closed[event.role] = true
      end
    end
    t.eq(closed.prompt_frame, true, "frame closed emission")
    t.eq(closed.prompt, true, "content closed emission")

    positions.reset()
    state.reset()
    close_extra_windows()
  end)
end
