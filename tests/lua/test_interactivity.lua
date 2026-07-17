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

  t.test("stop finishes locally without starting Thinking or showing a receipt", function()
    state.reset()
    local loopbiotic = require("loopbiotic")
    local rpc = require("loopbiotic.rpc")
    local source = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, source)
    state.session_id = "s_stop"
    state.source_buf = source
    state.source_cursor = { 1, 0 }

    card.show({
      id = "c_stop",
      kind = "finding",
      title = "Ready",
      finding = "There is no reason to ask the model to stop.",
      actions = { "stop" },
    })

    local original_request = rpc.request
    local sent
    rpc.request = function(method, params)
      sent = { method = method, params = params }
      return 1
    end

    local ok, err = pcall(function()
      loopbiotic.action("stop")
      t.eq(sent.method, "session/stop", "local stop endpoint")
      t.eq(sent.params.session_id, "s_stop", "stopped session")
      t.eq(state.thinking_request_id, nil, "stop must not start Thinking")
      t.eq(state.session_id, nil, "session finishes immediately")
      t.eq(state.card, nil, "Stopped receipt stays hidden")
      t.eq(state.card_win, nil, "card closes immediately")
    end)

    rpc.request = original_request
    if vim.api.nvim_buf_is_valid(source) then
      vim.api.nvim_buf_delete(source, { force = true })
    end
    state.reset()
    if not ok then
      error(err, 0)
    end
  end)

  t.test("working card renders streamed preview without final-card actions", function()
    state.reset()
    local source = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, source)
    state.session_id = "s_preview"
    state.source_buf = source
    state.source_cursor = { 1, 0 }

    card.show({
      id = "c_preview",
      kind = "working",
      turn_id = "t_preview",
      title = "Agent still working",
      phase = "drafting",
      message = "Drafting a response",
      preview = {
        title = "Avoid the stale cache",
        body = "The current key survives a source change and reuses outdated context.",
      },
      deadline_ms = 10000,
      elapsed_ms = 12000,
      actions = { "cancel_turn", "stop" },
    })

    local rendered = table.concat(vim.api.nvim_buf_get_lines(state.card_buf, 0, -1, false), "\n")
    t.eq(rendered:find("Avoid the stale cache", 1, true) ~= nil, true, "preview title")
    t.eq(rendered:find("reuses outdated context", 1, true) ~= nil, true, "preview body")
    t.eq(mapped(state.card_buf, "c"), true, "cancel remains available")
    t.eq(mapped(state.card_buf, "a"), false, "apply stays unavailable")
    cleanup()
  end)

  t.test("thinking view updates repeated streaming previews", function()
    state.reset()
    local thinking = require("loopbiotic.thinking")
    local source = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, source)
    state.session_id = "s_thinking_preview"
    state.source_buf = source
    state.source_cursor = { 1, 0 }
    state.thinking_request_id = "request"
    state.thinking_session_id = state.session_id
    state.thinking_started_at = (vim.uv or vim.loop).hrtime()
    state.thinking_steps = {
      { phase = "drafting", message = "Drafting a response", current = true },
    }

    thinking.progress({
      session_id = state.session_id,
      phase = "drafting",
      message = "Drafting a response",
      preview = { title = "First title", body = "The first partial body." },
    })
    thinking.progress({
      session_id = state.session_id,
      phase = "drafting",
      message = "Drafting a response",
      preview = { title = "First title", body = "The first partial body now contains more evidence." },
    })

    local rendered = table.concat(vim.api.nvim_buf_get_lines(state.card_buf, 0, -1, false), "\n")
    t.eq(state.thinking_preview.body, "The first partial body now contains more evidence.", "latest preview state")
    t.eq(rendered:find("evidence", 1, true) ~= nil, true, "updated body rendered")
    t.eq(rendered:find("validating before actions", 1, true) ~= nil, true, "safety label")
    thinking.stop(true)
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

  t.test("reply restores the source and abandons the live draft preview", function()
    state.reset()
    local loopbiotic = require("loopbiotic")
    local rpc = require("loopbiotic.rpc")
    local source = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(source, 0, -1, false, { "original" })
    local source_tick = vim.api.nvim_buf_get_changedtick(source)
    local draft = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(draft, 0, -1, false, { "changed" })
    local draft_win = vim.api.nvim_get_current_win()
    vim.api.nvim_win_set_buf(draft_win, draft)
    local card_buf, card_win = ui.float({ "Draft controls" }, { enter = true })

    state.session_id = "s_reply_draft"
    state.source_buf = source
    state.source_cursor = { 1, 0 }
    state.card = { id = "c_patch", kind = "patch", actions = { "apply", "why", "retry", "stop" } }
    state.card_buf = card_buf
    state.card_win = card_win
    state.diff_buf = draft
    state.diff_win = draft_win
    state.diff_source_buf = source
    state.diff_source_tick = source_tick

    local previous_thinking = config.values.thinking.enabled
    local original_request = rpc.request
    local sent
    config.values.thinking.enabled = false
    rpc.request = function(method, params)
      sent = { method = method, params = params }
      return 1
    end

    local ok, err = pcall(function()
      loopbiotic.reply("Explain the tradeoff before changing this.")
      t.eq(sent.method, "session/reply", "reply request")
      t.eq(vim.api.nvim_get_current_buf(), source, "source restored")
      t.eq(vim.api.nvim_buf_is_valid(draft), false, "draft wiped")
      t.eq(state.diff_buf, nil, "preview state cleared")
      t.eq(state.card_win, nil, "draft controls closed")
    end)

    rpc.request = original_request
    config.values.thinking.enabled = previous_thinking
    if vim.api.nvim_buf_is_valid(source) then
      vim.api.nvim_buf_delete(source, { force = true })
    end
    state.reset()
    if not ok then
      error(err, 0)
    end
  end)

  t.test("a non-patch result restores any preview left by an async turn", function()
    state.reset()
    local source = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(source, 0, -1, false, { "original" })
    local draft = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(draft, 0, -1, false, { "changed" })
    local draft_win = vim.api.nvim_get_current_win()
    vim.api.nvim_win_set_buf(draft_win, draft)
    local old_card_buf, old_card_win = ui.float({ "Draft controls" }, { enter = true })

    state.session_id = "s_async_result"
    state.source_buf = source
    state.source_cursor = { 1, 0 }
    state.card_buf = old_card_buf
    state.card_win = old_card_win
    state.diff_buf = draft
    state.diff_win = draft_win
    state.diff_source_buf = source
    state.diff_source_tick = vim.api.nvim_buf_get_changedtick(source)

    card.show({
      id = "c_finding",
      kind = "finding",
      title = "Explain before editing",
      finding = "The pending draft was superseded by conversation.",
      actions = { "fix", "stop" },
    })

    t.eq(vim.api.nvim_get_current_buf(), source, "source restored")
    t.eq(vim.api.nvim_buf_is_valid(draft), false, "draft wiped")
    t.eq(state.diff_buf, nil, "preview state cleared")
    t.eq(vim.api.nvim_win_is_valid(state.card_win), true, "finding rendered")

    ui.close(state.card_win)
    state.card_win = nil
    if vim.api.nvim_buf_is_valid(source) then
      vim.api.nvim_buf_delete(source, { force = true })
    end
    state.reset()
  end)

  t.test("background-tab action floats are deferred instead of remotely freed", function()
    local origin_tab = vim.api.nvim_get_current_tabpage()
    local origin_win = vim.api.nvim_get_current_win()
    local draft = vim.api.nvim_create_buf(false, true)
    vim.bo[draft].buftype = "nofile"
    vim.bo[draft].bufhidden = "wipe"
    vim.api.nvim_win_set_buf(origin_win, draft)
    local old_buf, old_win = ui.float({ "Draft controls" }, { enter = true })

    vim.cmd("tabnew")
    local new_buf, new_win = ui.render(old_buf, old_win, { "Conversation" }, { enter = false })

    local ok, err = pcall(function()
      t.eq(vim.api.nvim_win_is_valid(old_win), true, "old float remains allocated")
      t.eq(vim.api.nvim_tabpage_get_win(origin_tab), old_win, "origin pointer remains valid")
      t.eq(vim.api.nvim_win_get_tabpage(new_win), vim.api.nvim_get_current_tabpage(), "new float follows tab")
    end)

    ui.close(new_win)
    vim.api.nvim_set_current_tabpage(origin_tab)
    ui.cleanup_deferred()
    t.eq(vim.api.nvim_win_is_valid(old_win), false, "old float closes on its own tab")
    t.eq(vim.api.nvim_tabpage_get_win(origin_tab), origin_win, "normal origin window restored")
    vim.cmd("tabonly")
    local replacement = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, replacement)
    for _, buf in ipairs({ draft, old_buf, new_buf }) do
      if vim.api.nvim_buf_is_valid(buf) then
        vim.api.nvim_buf_delete(buf, { force = true })
      end
    end

    if not ok then
      error(err, 0)
    end
  end)
end
