local card = require("pair.card")
local config = require("pair.config")
local context = require("pair.context")
local log = require("pair.log")
local navigation = require("pair.navigation")
local prompt = require("pair.prompt")
local rpc = require("pair.rpc")
local state = require("pair.state")
local status = require("pair.status")
local thinking = require("pair.thinking")
local ui = require("pair.ui")

local M = {}

rpc.on("agent/progress", function(progress)
  thinking.progress(progress)
end)

-- Mid-turn permission request: the agent can only continue once a different
-- file is open. On approval the same agent turn resumes with fresh context —
-- no card is shown and no extra request is spent.
rpc.on_request("editor/open_location", function(params, respond)
  local location = params.location or {}
  local file = location.file or "?"
  local question = string.format(
    "Pair agent wants to open %s:%s\n%s",
    vim.fn.fnamemodify(file, ":~:."),
    location.line or 1,
    params.reason or ""
  )

  if vim.fn.confirm(question, "&Open\n&Deny", 1, "Question") == 1 and navigation.open_location(location) then
    respond({ granted = true, context = context.session() })
  else
    respond({ granted = false })
  end
end)

function M.setup(opts)
  config.setup(opts)
  require("pair.commands").setup()
  require("pair.keymaps").setup()
end

function M.prompt(mode)
  prompt.open(mode or config.values.backend.mode)
end

function M.reply_prompt()
  if not state.session_id then
    ui.notify("No active session", vim.log.levels.WARN)

    return
  end

  prompt.reply()
end

function M.start(text, mode, source)
  if not text or text == "" then
    return
  end

  status.hide()
  local request_id = thinking.start("Thinking", nil)
  local params, captured = context.current(text, mode)

  if source then
    captured = source
    params = vim.deepcopy(source.value)
    params.prompt = text
    params.mode = mode or config.values.backend.mode
    params.context_policy = vim.deepcopy(config.values.context.optimization)
  end

  state.source_buf = captured.buf
  state.source_cursor = { params.cursor.line, math.max(params.cursor.column - 1, 0) }
  state.goal = {
    statement = text,
    completed_steps = {},
    known_observations = {},
  }
  state.workspace_hints = context.workspace_hints(text, params.cwd, captured.buf)
  params.hints = context.merge_hints(params.hints, state.workspace_hints)

  rpc.request("session/start", params, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      log.write("session start error", message.error)
      ui.notify(message.error.message, vim.log.levels.ERROR)

      return
    end

    state.session_id = message.result.session_id
    state.goal = message.result.goal or state.goal
    state.token_usage = message.result.token_usage
    state.turn_token_usage = message.result.turn_token_usage
    state.backend_model = message.result.model or state.backend_model
    state.context_report = message.result.context_report
    log.event("context_optimization", message.result.context_report or {})
    log.event("agent_attempts", message.result.attempts or {})
    card.show(message.result.card)
  end)
end

function M.action(action)
  if not state.session_id then
    ui.notify("No active session", vim.log.levels.WARN)

    return
  end

  if action == "apply" and state.card and state.card.kind == "patch" then
    require("pair.diff").show(state.card)

    return
  end

  if action == "open" then
    if navigation.from_card(state.card or {}) then
      ui.close(state.card_win)
      state.card_win = nil
      status.show()
    else
      ui.notify("No location on this card", vim.log.levels.WARN)
    end

    return
  end

  ui.notify("Pair: " .. action)
  status.hide()
  local session_id = state.session_id
  if action == "fix" and state.card then
    M.focus_card_location(state.card)
  end
  local action_context = context.session()
  local request_id = thinking.start("Thinking", session_id)

  rpc.request("session/action", {
    session_id = session_id,
    action = action,
    context = action_context,
  }, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      log.write("session action error", message.error)
      ui.notify(message.error.message, vim.log.levels.ERROR)

      return
    end

    if message.result.session_id ~= state.session_id then
      log.write("stale session action result", message.result)

      return
    end

    state.token_usage = message.result.token_usage
    state.turn_token_usage = message.result.turn_token_usage
    state.backend_model = message.result.model or state.backend_model
    state.context_report = message.result.context_report
    log.event("context_optimization", message.result.context_report or {})
    log.event("agent_attempts", message.result.attempts or {})
    state.goal = message.result.goal or state.goal
    card.show(message.result.card)
  end)
end

function M.focus_card_location(active_card)
  if not navigation.card_location(active_card) then
    return false
  end
  local source_win = context.buffer_window(state.source_buf)
  if source_win then
    vim.api.nvim_set_current_win(source_win)
  end
  ui.close(state.card_win)
  state.card_win = nil

  return navigation.from_card(active_card)
end

function M.reply(text)
  if not state.session_id then
    ui.notify("No active session", vim.log.levels.WARN)

    return
  end

  if not text or text == "" then
    return
  end

  status.hide()

  local session_id = state.session_id
  local request_id = thinking.start("Thinking", session_id)

  rpc.request("session/reply", {
    session_id = session_id,
    text = text,
    context = context.session(),
  }, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      log.write("session reply error", message.error)
      ui.notify(message.error.message, vim.log.levels.ERROR)

      return
    end

    if message.result.session_id ~= state.session_id then
      log.write("stale session reply result", message.result)

      return
    end

    state.token_usage = message.result.token_usage
    state.turn_token_usage = message.result.turn_token_usage
    state.backend_model = message.result.model or state.backend_model
    state.context_report = message.result.context_report
    log.event("context_optimization", message.result.context_report or {})
    log.event("agent_attempts", message.result.attempts or {})
    state.goal = message.result.goal or state.goal
    card.show(message.result.card)
  end)
end

function M.stop()
  if not state.session_id then
    return
  end

  M.action("stop")
end

function M.resume()
  status.hide()

  if state.card then
    card.show(state.card, { enter = true })

    return
  end

  if state.last_card then
    card.show(state.last_card, { enter = true })

    return
  end

  ui.notify("No Pair card to restore", vim.log.levels.WARN)
end

-- One-key continuation for a deny card that names a location: jump there, so
-- the next context capture sees that buffer, then retry the denied step.
function M.open_and_retry()
  local active_card = state.card
  if not (active_card and active_card.kind == "deny" and type(active_card.location) == "table") then
    ui.notify("No location on this card", vim.log.levels.WARN)
    return
  end

  if not navigation.open_location(active_card.location) then
    ui.notify("Could not open " .. tostring(active_card.location.file), vim.log.levels.ERROR)
    return
  end

  ui.close(state.card_win)
  state.card_win = nil
  M.action("retry")
end

function M.go_to()
  if state.card and state.card.kind == "patch" and require("pair.diff").focus_change() then
    return
  end

  if navigation.from_card(state.card or {}) then
    ui.close(state.card_win)
    state.card_win = nil
    status.show()
    return
  end

  if state.source_buf and vim.api.nvim_buf_is_valid(state.source_buf) then
    local win = context.buffer_window(state.source_buf)
    if win then
      vim.api.nvim_set_current_win(win)
      vim.api.nvim_win_set_cursor(win, state.source_cursor or { 1, 0 })
      vim.cmd("normal! zz")
      return
    end
  end

  ui.notify("No Pair location to open", vim.log.levels.WARN)
end

function M.hide()
  if not state.session_id then
    return
  end

  ui.close(state.card_win)
  state.card_win = nil
  status.show()
end

function M.reset()
  require("pair.diff").restore_source()
  thinking.stop(true)
  ui.close(state.prompt_win)
  ui.close(state.prompt_frame_win)
  ui.close(state.card_win)
  ui.close(state.thinking_win)
  status.hide()
  rpc.stop()

  state.session_id = nil
  state.source_buf = nil
  state.source_cursor = nil
  state.card = nil
  state.goal = nil
  state.last_card = nil
  state.prompt_win = nil
  state.prompt_buf = nil
  state.prompt_frame_win = nil
  state.prompt_frame_buf = nil
  state.card_win = nil
  state.card_buf = nil
  state.status_win = nil
  state.status_buf = nil
  state.diff_tab = nil
  state.diff_buf = nil
  state.diff_win = nil
  state.diff_source_buf = nil
  state.diff_source_tick = nil
  state.diff_first_row = nil
  state.token_usage = nil
  state.turn_token_usage = nil
  state.backend_model = nil
  state.context_report = nil
  state.workspace_hints = nil
  state.completion_notified_card = nil
  state.thinking_request_id = nil
  state.thinking_session_id = nil

  ui.notify("Pair reset")
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
    ui.notify("Pair agent: " .. config.agent())

    return config.agent()
  end

  config.agent(name)
  rpc.stop()
  ui.notify("Pair agent: " .. name)

  return name
end

function M.agents()
  return config.agent_names()
end

function M.model(name)
  if not name or name == "" then
    local model = config.model()

    ui.notify("Pair model: " .. (model or "default"))

    return model
  end

  if name == "default" or name == "none" then
    local _, saved, save_error = config.model("")
    rpc.stop()
    if save_error then
      ui.notify("Pair model: default (could not save: " .. save_error .. ")", vim.log.levels.WARN)
    else
      ui.notify("Pair model: default" .. (saved and " · saved" or ""))
    end

    return nil
  end

  local _, saved, save_error = config.model(name)
  rpc.stop()
  if save_error then
    ui.notify("Pair model: " .. name .. " (could not save: " .. save_error .. ")", vim.log.levels.WARN)
  else
    ui.notify("Pair model: " .. name .. (saved and " · saved" or ""))
  end

  return name
end

function M.models()
  return config.model_names()
end

return M
