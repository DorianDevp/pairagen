local card = require("pair.card")
local config = require("pair.config")
local context = require("pair.context")
local log = require("pair.log")
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
  end

  state.source_buf = captured.buf
  state.source_cursor = { params.cursor.line, math.max(params.cursor.column - 1, 0) }

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
    state.token_usage = message.result.token_usage
    state.turn_token_usage = message.result.turn_token_usage
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
  local request_id = thinking.start("Thinking", session_id)

  rpc.request("session/action", {
    session_id = session_id,
    action = action,
    context = context.session(),
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
    card.show(message.result.card)
  end)
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
    card.show(state.card)

    return
  end

  if state.last_card then
    card.show(state.last_card)

    return
  end

  ui.notify("No Pair card to restore", vim.log.levels.WARN)
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
  state.token_usage = nil
  state.turn_token_usage = nil
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
    config.model("")
    rpc.stop()
    ui.notify("Pair model: default")

    return nil
  end

  config.model(name)
  rpc.stop()
  ui.notify("Pair model: " .. name)

  return name
end

function M.models()
  return config.model_names()
end

return M
