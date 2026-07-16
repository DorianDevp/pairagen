return function(t)
  local card = require("loopbiotic.card")
  local config = require("loopbiotic.config")
  local diff = require("loopbiotic.diff")
  local state = require("loopbiotic.state")
  local ui = require("loopbiotic.ui")

  local function mapped(buf, lhs)
    for _, mapping in ipairs(vim.api.nvim_buf_get_keymap(buf, "n")) do
      if mapping.lhs == lhs then
        return true
      end
    end
    local mapping = vim.api.nvim_buf_call(buf, function()
      return vim.fn.maparg(lhs, "n", false, true)
    end)
    return type(mapping) == "table" and mapping.buffer == 1
  end

  local function cleanup()
    ui.close(state.card_win)
    state.card_win = nil
    for _, buf in ipairs({ state.card_buf, state.diff_buf, state.source_buf }) do
      if buf and vim.api.nvim_buf_is_valid(buf) then
        pcall(vim.api.nvim_buf_delete, buf, { force = true })
      end
    end
    state.reset()
  end

  t.test("resume focuses the visible action window", function()
    state.reset()
    local source = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, source)
    state.session_id = "s_focus"
    state.source_buf = source
    state.source_cursor = { 1, 0 }

    card.show({
      id = "c_focus",
      kind = "finding",
      title = "A finding",
      finding = "Keep the editor interactive.",
      actions = { "follow", "goal", "stop" },
    })
    t.eq(vim.api.nvim_get_current_buf(), source, "card does not steal focus")

    require("loopbiotic").resume()

    t.eq(vim.api.nvim_get_current_win(), state.card_win, "resume enters card")
    t.eq(mapped(state.card_buf, "f"), true, "follow shortcut")
    t.eq(mapped(state.card_buf, "G"), true, "goal shortcut")
    cleanup()
  end)

  t.test("working card exposes a local cancel shortcut", function()
    state.reset()
    local source = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, source)
    state.session_id = "s_working"
    state.source_buf = source
    state.source_cursor = { 1, 0 }

    card.show({
      id = "c_working",
      kind = "working",
      turn_id = "t_1",
      title = "Agent still working",
      phase = "reviewing",
      message = "Reading one relevant block.",
      deadline_ms = 10000,
      elapsed_ms = 10000,
      actions = { "cancel_turn", "stop" },
    })

    t.eq(mapped(state.card_buf, "c"), true, "cancel shortcut")
    t.eq(mapped(state.card_buf, "q"), true, "stop shortcut")
    cleanup()
  end)

  t.test("draft control card binds every configured shortcut it prints", function()
    state.reset()
    local draft = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(draft, 0, -1, false, { "changed" })
    vim.api.nvim_win_set_buf(0, draft)
    state.session_id = "s_draft"
    state.diff_buf = draft
    state.diff_win = vim.api.nvim_get_current_win()
    state.diff_cursor = { 1, 0 }
    state.diff_first_row = 0
    state.goal = { statement = "Keep review interactive", completed_steps = {} }

    diff.controls({
      id = "c_patch",
      kind = "patch",
      title = "Small hunk",
      explanation = "Change one coherent block.",
      actions = { "apply", "why", "retry", "stop" },
    })

    local keys = config.values.keymaps
    for _, lhs in ipairs({
      keys.draft_accept,
      keys.draft_reject,
      keys.draft_retry,
      keys.why,
      keys.go_to,
    }) do
      t.eq(mapped(state.card_buf, lhs), true, "missing draft shortcut " .. lhs)
    end
    cleanup()
  end)
end
