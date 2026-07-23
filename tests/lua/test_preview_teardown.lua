-- Regression tests for the vanished-review race seen in the field: the user
-- accepted hunk 1, the backend yielded hunk 2 (preview opened, cursor jumped
-- to the change — deliberately), then the late apply result arrived with a
-- "continuing in background" working card, tore the unresolved preview down
-- after ~2s and left the cursor stranded at the vanished change.

return function(t)
  local card = require("loopbiotic.card")
  local diff = require("loopbiotic.diff")
  local rpc = require("loopbiotic.rpc")
  local state = require("loopbiotic.state")
  local thinking = require("loopbiotic.thinking")

  local function reset_layout()
    for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
      if vim.api.nvim_win_get_config(win).relative == "" then
        vim.api.nvim_set_current_win(win)
        break
      end
    end
    vim.cmd("silent only")
  end

  local function patched_file()
    local lines = {}
    for index = 1, 60 do
      lines[index] = "line " .. index
    end
    local file = vim.fn.getcwd() .. "/.loopbiotic-test-" .. tostring(vim.uv.hrtime()) .. ".txt"
    vim.fn.writefile(lines, file)
    return file,
      {
        id = "c_agent_h2",
        kind = "patch",
        patches = { { id = "patch-2", file = file, diff = "@@ -40,1 +40,1 @@\n-line 40\n+patched 40\n" } },
      }
  end

  local function cleanup(file)
    diff.restore_source(nil, { focus = false })
    state.reset()
    reset_layout()
    vim.cmd("silent! %bwipeout!")
    vim.fn.delete(file)
  end

  t.test("preview arrival still jumps to the change", function()
    reset_layout()
    state.reset()
    local file, patch_card = patched_file()
    vim.cmd("edit " .. vim.fn.fnameescape(file))
    vim.api.nvim_win_set_cursor(0, { 28, 4 })

    t.eq(diff.show(patch_card), true, "preview opens")

    t.eq(vim.api.nvim_get_current_win(), state.diff_win, "review owns the editor")
    t.eq(vim.api.nvim_win_get_cursor(state.diff_win)[1] >= 40, true, "cursor is taken to the change")

    cleanup(file)
  end)

  t.test("a superseding non-patch card unwinds the preview to the pre-preview cursor", function()
    reset_layout()
    state.reset()
    local file, patch_card = patched_file()
    vim.cmd("edit " .. vim.fn.fnameescape(file))
    local source_buf = vim.api.nvim_get_current_buf()
    local win = vim.api.nvim_get_current_win()
    vim.api.nvim_win_set_cursor(win, { 28, 4 })
    t.eq(diff.show(patch_card), true, "preview opens")
    t.eq(vim.api.nvim_win_get_cursor(win)[1] >= 40, true, "the preview jumped to the change")

    card.show({ id = "c_working_4", kind = "working", title = "Agent still working" })

    t.eq(vim.api.nvim_win_get_buf(win), source_buf, "the source buffer is restored")
    t.eq(vim.api.nvim_win_get_cursor(win), { 28, 4 }, "the cursor returns to the pre-preview position")
    t.eq(state.diff_origin, nil, "the recorded origin is consumed")

    cleanup(file)
  end)

  t.test("teardown restores the split the user reviewed in, not the first split", function()
    reset_layout()
    state.reset()
    local file, patch_card = patched_file()
    local first = vim.api.nvim_get_current_win()
    vim.cmd("belowright split")
    local second = vim.api.nvim_get_current_win()
    vim.cmd("edit " .. vim.fn.fnameescape(file))
    local source_buf = vim.api.nvim_get_current_buf()
    vim.api.nvim_win_set_cursor(second, { 28, 4 })
    local first_cursor = vim.api.nvim_win_get_cursor(first)
    t.eq(diff.show(patch_card), true, "preview opens in the working split")
    t.eq(state.diff_win, second, "the preview uses the split the user works in")

    card.show({ id = "c_working_4", kind = "working", title = "Agent still working" })

    t.eq(vim.api.nvim_win_get_buf(second), source_buf, "the source returns to the working split")
    t.eq(vim.api.nvim_win_get_cursor(second), { 28, 4 }, "the working split cursor is restored")
    t.eq(vim.api.nvim_win_get_cursor(first), first_cursor, "the first split is untouched")

    cleanup(file)
  end)

  t.test("reject also returns the cursor to the pre-preview position", function()
    reset_layout()
    state.reset()
    local file, patch_card = patched_file()
    vim.cmd("edit " .. vim.fn.fnameescape(file))
    local win = vim.api.nvim_get_current_win()
    vim.api.nvim_win_set_cursor(win, { 28, 4 })
    t.eq(diff.show(patch_card), true, "preview opens")

    diff.restore_source()

    t.eq(vim.api.nvim_win_get_cursor(win), { 28, 4 }, "rejecting the proposal restores the user's place")

    cleanup(file)
  end)

  t.test("accept keeps placing the cursor at the applied change", function()
    reset_layout()
    state.reset()
    local file, patch_card = patched_file()
    vim.cmd("edit " .. vim.fn.fnameescape(file))
    local win = vim.api.nvim_get_current_win()
    vim.api.nvim_win_set_cursor(win, { 28, 4 })
    t.eq(diff.show(patch_card), true, "preview opens")

    diff.restore_source({ 40, 0 })

    t.eq(vim.api.nvim_win_get_cursor(win), { 40, 0 }, "an explicit decision cursor wins over the origin")

    cleanup(file)
  end)

  t.test("a late apply result does not tear down a newer unresolved review", function()
    reset_layout()
    state.reset()
    state.session_id = "session-1"

    -- Accept of hunk 1 goes out while hunk 1 is the current card.
    state.card = { id = "c_agent_h1", kind = "patch" }
    local captured
    local original_request = rpc.request
    local original_start = thinking.start
    local original_current = thinking.current
    local original_stop = thinking.stop
    rpc.request = function(_, params, callback)
      captured = { params = params, callback = callback }
    end
    thinking.start = function()
      return "request-1"
    end
    thinking.current = function()
      return true
    end
    thinking.stop = function() end

    local ok, err = pcall(function()
      diff.send_accept({ "patch-1" }, { "a.txt" })
      t.eq(captured.params.card_id, "c_agent_h1", "the accept names the accepted card")

      -- Before the response returns, the backend yields hunk 2 and its
      -- preview opens.
      local file, patch_card = patched_file()
      vim.cmd("edit " .. vim.fn.fnameescape(file))
      vim.api.nvim_win_set_cursor(0, { 28, 4 })
      t.eq(diff.show(patch_card), true, "the newer review opens")
      state.card = patch_card

      -- The late apply result carries a background working card.
      captured.callback({
        result = {
          session_id = "session-1",
          token_usage = { total_tokens = 30 },
          card = { id = "c_working_4", kind = "working", title = "Continuing in background" },
        },
      })

      t.eq(diff.valid_preview(), true, "the unresolved review survives")
      t.eq(state.card.id, "c_agent_h2", "the review card is still current")
      t.eq(state.token_usage.total_tokens, 30, "usage from the result is still recorded")
      t.eq(vim.api.nvim_win_get_cursor(state.diff_win)[1] >= 40, true, "the review cursor is untouched")

      cleanup(file)
    end)

    rpc.request = original_request
    thinking.start = original_start
    thinking.current = original_current
    thinking.stop = original_stop
    state.reset()
    if not ok then
      error(err, 0)
    end
  end)
end
