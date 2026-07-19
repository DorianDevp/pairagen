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
        require("loopbiotic.scope").run("prompt", function()
          require("loopbiotic").prompt()
        end)
      end),
      { silent = true }
    )
  end

  M.call(keys.reply, "reply", "reply_prompt")
  M.call(keys.stop, "stop", "stop")
  M.call(keys.hide, "hide", "hide")
  M.call(keys.resume, "resume", "resume")
  M.call(keys.go_to, "go_to", "go_to")
  M.call(keys.reset, "reset", "reset")
end

function M.call(key, action, name)
  if not key or key == "" then
    return
  end

  vim.keymap.set(
    "n",
    key,
    util.guard("keymap " .. action, function()
      require("loopbiotic.scope").run(action, function()
        require("loopbiotic")[name]()
      end)
    end),
    { silent = true }
  )
end

return M
