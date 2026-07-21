-- Regression tests for split-window layouts: closing a focused float must
-- return the cursor to the split it was opened from, and buffer anchors and
-- window lookups must target the split the user is working in — never
-- "the first window in the tab".

return function(t)
  local context = require("loopbiotic.context")
  local ui = require("loopbiotic.ui")

  -- Reset to one normal window, then split; returns { first, second } with
  -- the second (lower) split focused, like a user working in a lower split.
  local function two_splits()
    for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
      if vim.api.nvim_win_get_config(win).relative == "" then
        vim.api.nvim_set_current_win(win)
        break
      end
    end
    vim.cmd("silent only")
    local first = vim.api.nvim_get_current_win()
    vim.cmd("belowright split")
    return first, vim.api.nvim_get_current_win()
  end

  t.test("closing a focused float returns the cursor to the originating split", function()
    local first, second = two_splits()
    t.eq(vim.api.nvim_get_current_win(), second)

    local _, win = ui.open_frame({ "float" }, {})
    t.eq(vim.api.nvim_get_current_win(), win, "the float takes focus")

    ui.close(win)
    t.eq(vim.api.nvim_get_current_win(), second, "the cursor returns to the split the float was opened from")
    t.eq(vim.api.nvim_get_current_win() ~= first, true, "the cursor does not land in the first split")
    vim.cmd("silent only")
  end)

  t.test("buffer_anchor targets the split the user works in", function()
    local first, second = two_splits()
    local buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, { "line 1", "line 2", "line 3" })
    vim.api.nvim_win_set_buf(first, buf)
    vim.api.nvim_win_set_buf(second, buf)

    local anchor = ui.buffer_anchor(buf, 1, 0)
    local expected = vim.fn.screenpos(second, 1, 1)
    local other = vim.fn.screenpos(first, 1, 1)
    t.eq(expected.row ~= other.row, true, "the two splits render the line at different screen rows")
    t.eq(anchor, { row = expected.row, col = expected.col }, "the anchor uses the focused split")
    vim.cmd("silent only")
  end)

  t.test("buffer_window prefers the current split over the first one", function()
    local first, second = two_splits()
    local buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(first, buf)
    vim.api.nvim_win_set_buf(second, buf)

    t.eq(context.buffer_window(buf), second, "the focused split wins")
    vim.api.nvim_set_current_win(first)
    t.eq(context.buffer_window(buf), first, "following the focus, not window order")
    vim.cmd("silent only")
  end)
end
