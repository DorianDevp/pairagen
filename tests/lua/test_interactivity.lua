return function(t)
  local card = require("loopbiotic.card")
  local config = require("loopbiotic.config")
  local diff = require("loopbiotic.diff")
  local scope = require("loopbiotic.scope")
  local state = require("loopbiotic.state")
  local surfaces = require("loopbiotic.surfaces")

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
    require("loopbiotic.thinking").stop(false)
    surfaces.close_all()
    vim.cmd("silent! tabonly")
    state.reset()
  end

  local function source()
    local buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, buf)
    state.source_buf = buf
    state.source_cursor = { 1, 0 }
    return buf
  end

  t.test("AgentWindow is a singleton and response rendering does not steal focus", function()
    cleanup()
    local source_buf = source()
    state.session_id = "s_singleton"
    card.show({ id = "one", kind = "finding", title = "One", finding = "First", actions = {} })
    local first = surfaces.snapshot().agent
    t.eq(vim.api.nvim_get_current_buf(), source_buf, "async render preserves source focus")

    card.show({ id = "two", kind = "finding", title = "Two", finding = "Second", actions = {} })
    local second = surfaces.snapshot().agent
    t.eq(second.buf, first.buf, "same AgentWindow buffer is reused")
    t.eq(second.win, first.win, "same AgentWindow frame is reused")
    require("loopbiotic").resume()
    t.eq(vim.api.nvim_get_current_win(), second.win, "resume focuses AgentWindow")
    cleanup()
  end)

  t.test("submitted prompt exists before the first stable Working render", function()
    cleanup()
    local source_buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(0, source_buf)
    vim.api.nvim_buf_set_lines(source_buf, 0, -1, false, { "local answer = 42" })
    local captured = require("loopbiotic.context").capture(nil, { skip_lsp = true })
    state.source_buf = nil
    state.source_cursor = nil

    local rpc = require("loopbiotic.rpc")
    local context = require("loopbiotic.context")
    local thinking = require("loopbiotic.thinking")
    local ui = require("loopbiotic.ui")
    local original_request = rpc.request
    local original_workspace_hints = context.workspace_hints
    local original_render = surfaces.render_agent
    local first_render
    local sent
    local events = {}

    rpc.request = function(method, params)
      table.insert(events, method)
      sent = { method = method, params = params }
    end
    context.workspace_hints = function()
      return {}
    end
    surfaces.render_agent = function(lines, opts)
      table.insert(events, "AgentWindow:working")
      if not first_render then
        first_render = {
          source_buf = state.source_buf,
          goal = state.goal and state.goal.statement,
          anchor = vim.deepcopy(opts.window.anchor),
        }
      end
      return original_render(lines, opts)
    end

    local ok, err = pcall(require("loopbiotic").submit_prompt, "Explain answer", "investigate", captured)
    rpc.request = original_request
    context.workspace_hints = original_workspace_hints
    surfaces.render_agent = original_render
    if not ok then
      cleanup()
      error(err, 0)
    end

    t.eq(first_render.source_buf, source_buf, "source precedes Working")
    t.eq(first_render.goal, "Explain answer", "prompt precedes Working")
    t.eq(type(first_render.anchor), "table", "first render has its source anchor")
    t.eq(sent.method, "session/start")
    t.eq(sent.params.prompt, "Explain answer")
    t.eq(events, { "AgentWindow:working", "session/start" }, "action -> reaction -> transport")

    local agent = surfaces.snapshot().agent
    local before = vim.api.nvim_win_get_config(agent.win)
    thinking.tick(state.thinking_request_id)
    local after = vim.api.nvim_win_get_config(agent.win)
    t.eq(
      { ui.number(after.row), ui.number(after.col) },
      { ui.number(before.row), ui.number(before.col) },
      "progress render keeps the initial geometry"
    )
    cleanup()
  end)

  t.test("working AgentWindow has no Reply or Cancel action", function()
    cleanup()
    source()
    state.session_id = "s_working"
    card.show({
      id = "working",
      kind = "working",
      turn_id = "turn",
      title = "Working",
      phase = "drafting",
      message = "Still working",
      actions = { "cancel_turn", "stop" },
    })
    local agent = surfaces.snapshot().agent
    t.eq(scope.allows("reply"), false, "reply is out of scope")
    t.eq(mapped(agent.buf, "m"), false, "no local Reply")
    t.eq(mapped(agent.buf, "c"), false, "no local Cancel")
    t.eq(mapped(agent.buf, "h"), false, "no hidden Wrap alias")
    t.eq(mapped(agent.buf, "q"), true, "Quit remains local")
    cleanup()
  end)

  t.test("out-of-scope pm is a silent no-op while the agent works", function()
    cleanup()
    source()
    state.session_id = "s_scope"
    card.show({ id = "working", kind = "working", turn_id = "turn", message = "Busy", actions = {} })
    local calls = 0
    t.eq(scope.run("reply", function()
      calls = calls + 1
    end), false)
    t.eq(calls, 0, "callback was not activated")
    cleanup()
  end)

  t.test("opening PromptWindow during work cancels the real turn and installs a submit barrier", function()
    cleanup()
    source()
    state.session_id = "s_interrupt"
    card.show({ id = "working", kind = "working", turn_id = "turn-1", message = "Busy", actions = {} })
    local rpc = require("loopbiotic.rpc")
    local original_request = rpc.request
    local cancellation
    rpc.request = function(method, params, callback)
      if method == "session/action" then
        cancellation = { params = params, callback = callback }
      end
    end
    local ok, err = pcall(require("loopbiotic").prompt)
    rpc.request = original_request
    if not ok then
      error(err, 0)
    end
    t.eq(cancellation.params.action, "cancel_turn")
    t.eq(state.cancelled_turn_id, "turn-1")
    t.eq(state.turn_barrier, true)
    t.eq(surfaces.prompt_open(), true)
    t.eq(surfaces.agent_view(), "interrupted")
    cancellation.callback({ result = { goal = { status = "paused" } } })
    t.eq(state.turn_barrier, false)
    t.eq(state.goal.status, "paused")
    cleanup()
  end)

  t.test("wrapped and off-tab AgentWindow retains one owner tab", function()
    cleanup()
    source()
    state.session_id = "s_tabs"
    card.show({ id = "one", kind = "finding", title = "One", finding = "First", actions = {} })
    local owner = vim.api.nvim_get_current_tabpage()
    require("loopbiotic").hide()
    t.eq(surfaces.agent_mode(), "wrapped")

    vim.cmd("tabnew")
    local foreign = vim.api.nvim_get_current_tabpage()
    card.show({ id = "two", kind = "finding", title = "Two", finding = "Updated off-tab", actions = {} })
    t.eq(surfaces.agent_owner_tab(), owner, "async update cannot migrate ownership")
    t.eq(vim.api.nvim_get_current_tabpage(), foreign)

    require("loopbiotic").resume()
    t.eq(vim.api.nvim_get_current_tabpage(), owner, "pr restores owner tab")
    t.eq(surfaces.agent_mode(), "visible", "pr unwraps")
    cleanup()
  end)

  t.test("PromptWindow can coexist with AgentWindow and close returns focus", function()
    cleanup()
    source()
    state.session_id = "s_prompt"
    card.show({ id = "one", kind = "finding", title = "One", finding = "First", actions = {} })
    require("loopbiotic.prompt").open_for({
      title = " Prompt test ",
      footer = " test ",
      return_to_agent = true,
      submit = function() end,
    })
    t.eq(surfaces.prompt_open(), true)
    t.eq(surfaces.agent_mode(), "visible")
    require("loopbiotic.prompt").close()
    t.eq(vim.api.nvim_get_current_win(), surfaces.snapshot().agent.win)
    cleanup()
  end)

  t.test("Reply PromptWindow submits one immutable selected mode", function()
    cleanup()
    source()
    state.session_id = "s_mode"
    local prompt = require("loopbiotic.prompt")
    prompt.reply("review")
    local prompt_buf = surfaces.snapshot().prompt.buf
    vim.api.nvim_buf_set_lines(prompt_buf, 0, -1, false, { "Check this contract" })
    local submitted

    prompt.submit(prompt_buf, function(text, mode)
      submitted = { text = text, mode = mode }
    end)

    t.eq(submitted, { text = "Check this contract", mode = "review" })
    t.eq(state.prompt_stash_mode, "review")
    cleanup()
  end)

  t.test("Review prints and binds only the mutation decision plus local navigation", function()
    cleanup()
    local draft = source()
    state.session_id = "s_review"
    state.diff_buf = draft
    state.diff_win = vim.api.nvim_get_current_win()
    state.diff_source_buf = vim.api.nvim_create_buf(false, true)
    state.diff_source_tick = vim.api.nvim_buf_get_changedtick(state.diff_source_buf)
    diff.controls({ id = "patch", kind = "patch", explanation = "One change", actions = { "retry", "why" } })
    local agent = surfaces.snapshot().agent
    t.eq(mapped(agent.buf, config.values.keymaps.draft_accept), true)
    t.eq(mapped(agent.buf, config.values.keymaps.draft_reject), true)
    t.eq(mapped(agent.buf, config.values.keymaps.go_to), true, "Review binds local navigation")
    t.eq(mapped(agent.buf, config.values.keymaps.details), false, "short explanation offers no details toggle")
    t.eq(mapped(agent.buf, "m"), false, "Review has no Reply before a decision")
    t.eq(mapped(agent.buf, "t"), false, "Review has no Retry")

    diff.controls({
      id = "patch",
      kind = "patch",
      explanation = string.rep("A long explanation that overflows the control line ", 3),
      actions = { "retry", "why" },
    })
    t.eq(mapped(agent.buf, config.values.keymaps.details), true, "overflowing explanation binds the details toggle")
    cleanup()
  end)

  t.test("Reject is token-free, restores source, pauses AgentWindow and opens Reply", function()
    cleanup()
    local source_buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(source_buf, 0, -1, false, { "accepted" })
    local draft = source()
    vim.api.nvim_buf_set_lines(draft, 0, -1, false, { "proposed" })
    state.session_id = "s_reject"
    state.card = { id = "card", kind = "patch", patches = { { id = "patch", file = "x" } } }
    state.goal = { statement = "goal", status = "active" }
    state.diff_buf = draft
    state.diff_win = vim.api.nvim_get_current_win()
    state.diff_source_buf = source_buf
    state.diff_source_tick = vim.api.nvim_buf_get_changedtick(source_buf)
    diff.controls(state.card)

    local rpc = require("loopbiotic.rpc")
    local prompt = require("loopbiotic.prompt")
    local original_request = rpc.request
    local original_reply = prompt.reply
    local sent
    local opened = false
    rpc.request = function(method, params)
      sent = { method = method, params = params }
    end
    prompt.reply = function()
      opened = true
    end
    local ok, err = pcall(diff.reject)
    rpc.request = original_request
    prompt.reply = original_reply
    if not ok then
      error(err, 0)
    end

    t.eq(sent.method, "patch/apply_result")
    t.eq(sent.params.accepted, false)
    t.eq(state.thinking_request_id, nil, "Reject starts no model phase")
    t.eq(state.goal.status, "paused")
    t.eq(surfaces.agent_view(), "paused")
    t.eq(opened, true, "Reply PromptWindow route opens")
    t.eq(vim.api.nvim_get_current_buf(), source_buf, "accepted source restored")
    cleanup()
  end)

  t.test("Stop closes both singleton surfaces before backend acknowledgement", function()
    cleanup()
    source()
    state.session_id = "s_stop"
    card.show({ id = "one", kind = "finding", title = "One", finding = "First", actions = {} })
    local rpc = require("loopbiotic.rpc")
    local original_request = rpc.request
    local sent
    rpc.request = function(method, params)
      sent = { method = method, params = params }
    end
    require("loopbiotic").stop()
    rpc.request = original_request
    t.eq(sent.method, "session/stop")
    t.eq(state.session_id, nil)
    t.eq(surfaces.agent_mode(), "closed")
    t.eq(surfaces.prompt_open(), false)
    cleanup()
  end)
end
