local card = require("loopbiotic.card")
local config = require("loopbiotic.config")
local context = require("loopbiotic.context")
local log = require("loopbiotic.log")
local navigation = require("loopbiotic.navigation")
local positions = require("loopbiotic.positions")
local prompt = require("loopbiotic.prompt")
local rpc = require("loopbiotic.rpc")
local session = require("loopbiotic.session")
local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
local thinking = require("loopbiotic.thinking")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

local M = {}

local function show_agent_error(message, has_session)
  -- A failed turn cannot answer a pending location request anymore.
  require("loopbiotic.permission").settle("agent error")
  local lines = { "Agent error", tostring(message or "The agent turn failed."), "" }
  table.insert(lines, has_session and "[m] Reply   [q] Quit" or "[p] Prompt")
  surfaces.render_agent(lines, {
    view = "error",
    working = false,
    enter = false,
    window = {
      width = 58,
      height = #lines,
      border = config.values.card.border,
      title = " Loopbiotic: Error ",
    },
    bind = function(buf)
      if has_session then
        vim.keymap.set("n", "m", M.reply_prompt, { buffer = buf, nowait = true, silent = true })
        vim.keymap.set("n", "q", M.stop, { buffer = buf, nowait = true, silent = true })
      else
        vim.keymap.set("n", "p", M.prompt, { buffer = buf, nowait = true, silent = true })
      end
    end,
  })
end

rpc.on("agent/progress", function(progress)
  if
    state.card
    and state.card.kind == "working"
    and progress.session_id == state.session_id
    and state.card.turn_id ~= state.cancelled_turn_id
  then
    state.card.phase = progress.phase or state.card.phase
    state.card.message = progress.message or state.card.message
    if type(progress.preview) == "table" then
      state.card.preview = progress.preview
    elseif progress.phase == "repairing" or progress.phase == "restarting" then
      state.card.preview = nil
    end
    card.show(state.card)
    return
  end
  thinking.progress(progress)
end)

rpc.on("agent/turn_ready", function(params)
  if params.session_id ~= state.session_id or params.turn_id == state.cancelled_turn_id then
    return
  end
  if not (state.card and state.card.kind == "working" and state.card.turn_id == params.turn_id) then
    return
  end
  if params.error then
    surfaces.set_agent_working(false)
    -- Retire this turn so a late agent/progress for it cannot re-render the
    -- working card over the error view.
    state.cancelled_turn_id = params.turn_id
    show_agent_error(params.error, true)
    return
  end
  if params.result then
    session.apply_turn_result(params.result)
  end
end)

rpc.on_request("editor/read_file", function(params, respond)
  local file = params.file or "?"
  if not M.workspace_location(file) then
    respond({ granted = false })
    return
  end

  local value = context.file(file)
  respond({ granted = value ~= nil, context = value })
end)

-- A running turn may need another buffer before it can produce its card.
-- That is a real permission request: AgentWindow presents the file and the
-- agent's reason with explicit Accept / Deny, and the turn stays blocked on
-- the backend until the user decides (or the daemon-side wait expires as
-- denied). Out-of-workspace targets are denied without asking.
rpc.on_request("editor/open_location", function(params, respond)
  require("loopbiotic.permission").request(params, respond)
end)

function M.workspace_location(file)
  return util.in_workspace(file)
end

local position_log_unsubscribe = nil

function M.setup(opts)
  config.setup(opts)
  surfaces.setup()
  positions.setup()
  -- Root is the single observer of window geometry: every emission lands in
  -- the JSONL session trace, stamped with the active backend session.
  if position_log_unsubscribe then
    position_log_unsubscribe()
  end
  position_log_unsubscribe = positions.subscribe(function(event)
    log.event("window_geometry", vim.tbl_extend("force", event, { session_id = state.session_id }))
  end)
  require("loopbiotic.commands").setup()
  require("loopbiotic.keymaps").setup()
end

function M.prompt(mode)
  if not require("loopbiotic.scope").allows("prompt") then
    return
  end
  if require("loopbiotic.scope").working() then
    M.interrupt_for_prompt()
  end
  prompt.open(mode or config.values.backend.mode)
end

function M.interrupt_for_prompt()
  -- The daemon defers other requests while a location permission waits, so
  -- the pending request must be answered before cancel_turn is sent.
  require("loopbiotic.permission").settle("interrupted for prompt")
  local session_id = state.session_id
  local active_card = state.card
  if active_card and active_card.kind == "working" then
    state.cancelled_turn_id = active_card.turn_id
  end
  state.turn_barrier = session_id ~= nil
  thinking.stop(false)
  surfaces.render_agent({
    "Work interrupted",
    "The running turn was cancelled before opening PromptWindow.",
    "",
    "[m] Reply   [q] Quit",
  }, {
    view = "interrupted",
    working = false,
    enter = false,
    window = {
      width = 58,
      height = 4,
      border = config.values.card.border,
      title = " Loopbiotic: Interrupted ",
    },
    bind = function(buf)
      vim.keymap.set("n", "m", function()
        require("loopbiotic.scope").run("reply", M.reply_prompt)
      end, { buffer = buf, nowait = true, silent = true })
      vim.keymap.set("n", "q", M.stop, { buffer = buf, nowait = true, silent = true })
    end,
  })

  if not session_id then
    -- session/start has no server-side id yet. Stopping the transport is the
    -- only real cancellation boundary; the next submit starts a fresh daemon.
    rpc.stop()
    state.turn_barrier = false
    return
  end

  rpc.request("session/action", {
    session_id = session_id,
    action = "cancel_turn",
  }, function(message)
    state.turn_barrier = false
    if message.error then
      log.write("turn interrupt error", message.error)
      return
    end
    if state.session_id == session_id and message.result then
      state.goal = message.result.goal or state.goal
    end
  end)
end

function M.reply_prompt()
  if not require("loopbiotic.scope").allows("reply") then
    return
  end

  prompt.reply()
end

local function send_session_start(params, request_id)
  rpc.request("session/start", params, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      -- state.prompt_stash still holds the composed text; the next
      -- prompt.open pre-fills it so nothing is lost to a broken backend.
      log.write("session start error", message.error)
      show_agent_error(message.error.message, false)

      return
    end

    state.prompt_stash = prompt.next_stash(state.prompt_stash, "start_ok")
    state.prompt_stash_mode = nil
    require("loopbiotic.widgets").clear()
    state.session_id = message.result.session_id
    session.apply_turn_result(message.result)
  end)
end

function M.submit_prompt(text, mode, source, selected_skills)
  if not text or text == "" then
    return
  end

  mode = prompt.normalize_mode(mode or config.values.backend.mode)

  local carried_context = require("loopbiotic.widgets").list()
  local carried_graph = state.call_hierarchy
  if state.session_id then
    M.stop()
    require("loopbiotic.skills").prepare((source and source.value.cwd) or vim.fn.getcwd())
    require("loopbiotic.skills").activate(selected_skills)
    state.call_hierarchy = carried_graph
    for _, ref in ipairs(carried_context) do
      require("loopbiotic.widgets").select(ref)
    end
  end

  local params, captured
  if source then
    captured = source
    params = vim.deepcopy(source.value)
    params.prompt = text
    params.mode = mode
    params.context_policy = vim.deepcopy(config.values.context.optimization)
  else
    params, captured = context.current(text, mode)
  end

  -- The user submission exists before AgentWindow reacts. Establish its source
  -- and intent first so the initial Working View has the same anchor as every
  -- subsequent progress render.
  state.session_mode = mode
  state.source_buf = captured.buf
  state.source_cursor = { params.cursor.line, math.max(params.cursor.column - 1, 0) }
  state.goal = {
    statement = text,
    completed_steps = {},
    known_observations = {},
    status = "idle",
  }

  local request_id = thinking.start("Thinking", nil)
  state.workspace_hints = context.workspace_hints(text, params.cwd, captured.buf)
  params.hints = context.merge_hints(params.hints, state.workspace_hints)
  params.project_signals = context.project_signals(captured.buf, params.cwd)
  require("loopbiotic.widgets").attach(params)
  require("loopbiotic.skills").attach(params, selected_skills)
  send_session_start(params, request_id)
end

local function send_session_reply(params, request_id, mode)
  rpc.request("session/reply", params, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      log.write("session reply error", message.error)
      show_agent_error(message.error.message, true)

      return
    end

    if message.result.session_id ~= state.session_id then
      log.write("stale session reply result", message.result)

      return
    end

    state.prompt_stash = prompt.next_stash(state.prompt_stash, "start_ok")
    state.prompt_stash_mode = nil
    state.session_mode = mode
    require("loopbiotic.widgets").clear()
    session.apply_turn_result(message.result)
  end)
end

function M.submit_reply(text, mode, selected_skills)
  if not state.session_id then
    ui.notify("No active session", vim.log.levels.WARN)

    return
  end

  if not text or text == "" or state.turn_barrier then
    return
  end

  if not M.confirm_agent_turn() then
    return
  end

  local diff = require("loopbiotic.diff")
  if diff.valid_preview() then
    diff.restore_source()
  end
  require("loopbiotic.fileops").clear()

  local session_id = state.session_id
  mode = prompt.normalize_mode(mode or state.session_mode or config.values.backend.mode)
  local params = {
    session_id = session_id,
    text = text,
    mode = mode,
    context = require("loopbiotic.widgets").attach(context.session(text)),
    skills = vim.deepcopy(selected_skills or require("loopbiotic.skills").snapshot()),
  }
  local request_id = thinking.start("Thinking", session_id)
  send_session_reply(params, request_id, mode)
end

function M.token_budget_exceeded()
  local budget = tonumber(config.values.backend.token_budget) or 0
  local used = state.token_usage and tonumber(state.token_usage.total_tokens) or 0

  return budget > 0 and used >= budget, used, budget
end

function M.confirm_agent_turn()
  local exceeded, used, budget = M.token_budget_exceeded()
  if not exceeded then
    return true
  end

  local question =
    string.format("Loopbiotic session used %s tokens (budget %s).\nStart another agent turn?", used, budget)

  return vim.fn.confirm(question, "&Continue\n&Cancel", 2, "Warning") == 1
end

function M.stop()
  local session_id = state.session_id
  if not session_id then
    return
  end

  -- Finishing a session is a local lifecycle action, not an agent turn.
  -- Tear down the UI immediately and notify the daemon in the background;
  -- never show Thinking or a redundant "Stopped" receipt. A pending location
  -- permission is answered first so the daemon can process session/stop.
  require("loopbiotic.permission").settle("session stopped")
  require("loopbiotic.diff").restore_source()
  require("loopbiotic.fileops").clear()
  thinking.stop(true)
  surfaces.close_all()

  state.session_id = nil
  state.source_buf = nil
  state.source_cursor = nil
  state.card = nil
  state.goal = nil
  state.token_usage = nil
  state.turn_token_usage = nil
  state.context_report = nil
  state.workspace_hints = nil
  state.call_hierarchy = nil
  state.card_flow_active = false
  state.session_mode = nil
  require("loopbiotic.widgets").clear()
  require("loopbiotic.skills").reset()
  state.details_card = nil
  state.details_expanded = false
  state.cancelled_turn_id = nil
  state.turn_barrier = false

  rpc.request("session/stop", {
    session_id = session_id,
    action = "stop",
  }, function(message)
    if message.error then
      log.write("session stop error", message.error)
    end
  end)
end

function M.resume()
  if not require("loopbiotic.scope").allows("resume") then
    return
  end
  surfaces.resume_agent()
end

function M.go_to()
  if not require("loopbiotic.scope").allows("go_to") then
    return
  end
  if state.card and state.card.kind == "patch" and require("loopbiotic.diff").focus_change() then
    return
  end

  if navigation.from_card(state.card or {}) then
    return
  end

  if state.source_buf and vim.api.nvim_buf_is_valid(state.source_buf) then
    local win = context.buffer_window(state.source_buf)
    if win then
      local cursor = state.source_cursor or { 1, 0 }
      vim.api.nvim_set_current_win(win)
      -- The buffer may have shrunk since the position was captured.
      vim.api.nvim_win_set_cursor(win, util.clamp_cursor(state.source_buf, cursor[1], cursor[2]))
      vim.cmd("normal! zz")
      return
    end
  end

  ui.notify("No Loopbiotic location to open", vim.log.levels.WARN)
end

function M.hide()
  if not require("loopbiotic.scope").allows("hide") then
    return
  end

  surfaces.wrap_agent()
end

function M.reset()
  require("loopbiotic.permission").settle("reset")
  require("loopbiotic.diff").restore_source()
  require("loopbiotic.fileops").clear()
  thinking.stop(true)
  surfaces.close_all()
  rpc.stop()
  state.reset()

  ui.notify("Loopbiotic reset")
end

function M.backend()
  rpc.request("backend/list", {}, function(message)
    if message.error then
      log.write("backend list error", message.error)
      ui.notify(message.error.message, vim.log.levels.ERROR)

      return
    end

    ui.notify(vim.inspect(message.result))
  end)
end

function M.agent(name)
  if not name or name == "" then
    ui.notify("Loopbiotic agent: " .. config.agent())

    return config.agent()
  end

  if state.session_id then
    ui.notify("Finish the active session before changing agent", vim.log.levels.WARN)
    return config.agent()
  end

  config.agent(name)
  rpc.stop()
  ui.notify("Loopbiotic agent: " .. name)

  return name
end

function M.agents()
  return config.agent_names()
end

-- Concrete model to display for the active agent: configured model, then
-- the backend/warmup identity, then the model reported after the last turn.
-- The word "default" is never displayed.
function M.model_display()
  local label = prompt.model_label(config.model(), state.agent_identity, state.backend_model)

  if label:find("model?", 1, true) == 1 then
    return (label:gsub("^model%?", "agent default (not yet resolved)"))
  end

  return label
end

function M.model(name)
  if not name or name == "" then
    local model = config.model()

    ui.notify("Loopbiotic model: " .. M.model_display())

    return model
  end

  if state.session_id then
    ui.notify("Finish the active session before changing model", vim.log.levels.WARN)
    return config.model()
  end

  -- "default" and "none" stay accepted as inputs: they clear the stored
  -- per-agent preference. Only the displayed name changes.
  if name == "default" or name == "none" then
    local _, saved, save_error = config.model("")
    rpc.stop()
    -- An explicit change makes the recorded per-phase actuals stale as
    -- next-turn predictions for the PromptWindow title.
    state.backend_models = vim.NIL
    local display = M.model_display()
    if save_error then
      ui.notify("Loopbiotic model: " .. display .. " (could not save: " .. save_error .. ")", vim.log.levels.WARN)
    else
      ui.notify("Loopbiotic model: " .. display .. (saved and " · saved" or ""))
    end

    return nil
  end

  local _, saved, save_error = config.model(name)
  rpc.stop()
  state.backend_models = vim.NIL
  if save_error then
    ui.notify("Loopbiotic model: " .. name .. " (could not save: " .. save_error .. ")", vim.log.levels.WARN)
  else
    ui.notify("Loopbiotic model: " .. name .. (saved and " · saved" or ""))
  end

  return name
end

-- The model for discovery turns (investigate/explain/review). Mirrors M.model
-- but targets the discovery phase, so the user can steer which model answers
-- non-patch turns independently of the patch-drafting model.
function M.discovery_model(name)
  if not name or name == "" then
    local model = config.discovery_model()
    ui.notify("Loopbiotic discovery model: " .. (model and model ~= "" and model or "agent default"))
    return model
  end

  if state.session_id then
    ui.notify("Finish the active session before changing the discovery model", vim.log.levels.WARN)
    return config.discovery_model()
  end

  -- "default"/"none" clear the stored preference back to the agent default.
  if name == "default" or name == "none" then
    local _, saved, save_error = config.discovery_model("")
    rpc.stop()
    state.backend_models = vim.NIL
    if save_error then
      ui.notify("Loopbiotic discovery model: agent default (could not save: " .. save_error .. ")", vim.log.levels.WARN)
    else
      ui.notify("Loopbiotic discovery model: agent default" .. (saved and " · saved" or ""))
    end
    return nil
  end

  local _, saved, save_error = config.discovery_model(name)
  rpc.stop()
  state.backend_models = vim.NIL
  if save_error then
    ui.notify("Loopbiotic discovery model: " .. name .. " (could not save: " .. save_error .. ")", vim.log.levels.WARN)
  else
    ui.notify("Loopbiotic discovery model: " .. name .. (saved and " · saved" or ""))
  end

  return name
end

function M.models()
  return config.model_names()
end

return M
