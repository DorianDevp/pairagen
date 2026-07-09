local card = require("pair.card")
local config = require("pair.config")
local context = require("pair.context")
local prompt = require("pair.prompt")
local rpc = require("pair.rpc")
local state = require("pair.state")
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

  rpc.request("session/start", context.current(text, mode), function(message)
    if message.error then
      ui.notify(message.error.message, vim.log.levels.ERROR)

      return
    end

    state.session_id = message.result.session_id
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

  rpc.request("session/action", {
    session_id = state.session_id,
    action = action,
  }, function(message)
    if message.error then
      ui.notify(message.error.message, vim.log.levels.ERROR)

      return
    end

    card.show(message.result.card)
  end)
end

function M.stop()
  if not state.session_id then
    return
  end

  M.action("stop")
end

function M.backend()
  rpc.request("backend/list", {}, function(message)
    if message.error then
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
