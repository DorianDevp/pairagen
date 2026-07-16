local config = require("loopbiotic.config")
local util = require("loopbiotic.util")

local M = {}

function M.setup()
  local keys = config.values.keymaps
  local key = keys.prompt

  if key and key ~= "" then
    -- Error boundary at registration: a bug behind a keymap is logged and
    -- reported instead of killing the session (same for action/call below).
    vim.keymap.set(
      { "n", "v" },
      key,
      util.guard("keymap prompt", function()
        require("loopbiotic").prompt()
      end),
      { silent = true }
    )
  end

  M.call(keys.reply, "reply_prompt")
  M.action(keys.follow, "follow")
  M.action(keys.why, "why")
  M.action(keys.fix, "fix")
  M.action(keys.goal, "goal")
  M.action(keys.cancel, "cancel_turn")
  M.action(keys.other_lead, "other_lead")
  M.action(keys.stop, "stop")
  M.call(keys.hide, "hide")
  M.call(keys.resume, "resume")
  M.call(keys.go_to, "go_to")
  M.call(keys.reset, "reset")
end

function M.action(key, action)
  if not key or key == "" then
    return
  end

  vim.keymap.set(
    "n",
    key,
    util.guard("keymap " .. action, function()
      require("loopbiotic").action(action)
    end),
    { silent = true }
  )
end

function M.call(key, name)
  if not key or key == "" then
    return
  end

  vim.keymap.set(
    "n",
    key,
    util.guard("keymap " .. name, function()
      require("loopbiotic")[name]()
    end),
    { silent = true }
  )
end

return M
