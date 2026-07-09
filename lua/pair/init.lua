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

function M.setup(opts)
  config.setup(opts)
  require("pair.commands").setup()
  require("pair.keymaps").setup()
end

function M.prompt(mode)
  prompt.open(mode or config.values.backend.mode)
end

function M.start(text, mode)
  if not text or text == "" then
    return
  end

  status.hide()
  local request_id = thinking.start("Thinking", nil)

  rpc.request("session/start", context.current(text, mode), function(message)
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

  ui.notify("Pair: " .. action)
  status.hide()
  local session_id = state.session_id
  local request_id = thinking.start("Thinking", session_id)

  rpc.request("session/action", {
    session_id = session_id,
    action = action,
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
  thinking.stop(true)
  ui.close(state.prompt_win)
  ui.close(state.card_win)
  ui.close(state.thinking_win)
  status.hide()
  rpc.stop()

  state.session_id = nil
  state.card = nil
  state.last_card = nil
  state.prompt_win = nil
  state.prompt_buf = nil
  state.card_win = nil
  state.card_buf = nil
  state.status_win = nil
  state.status_buf = nil
  state.diff_tab = nil
  state.token_usage = nil
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

return M
