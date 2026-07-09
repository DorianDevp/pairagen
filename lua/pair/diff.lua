local apply = require("pair.apply")
local config = require("pair.config")
local log = require("pair.log")
local rpc = require("pair.rpc")
local state = require("pair.state")
local thinking = require("pair.thinking")
local ui = require("pair.ui")

local M = {}

function M.show(card)
  if config.values.diff.layout == "tab" then
    vim.cmd("tabnew")
  else
    vim.cmd("new")
  end

  state.diff_tab = vim.api.nvim_get_current_tabpage()

  local buf = vim.api.nvim_get_current_buf()
  local lines = M.lines(card)

  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "diff"

  vim.api.nvim_buf_set_name(buf, "Pair patch")
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)

  vim.keymap.set("n", "a", M.accept, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "q", M.reject, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "r", function()
    require("pair").action("retry")
  end, { buffer = buf, nowait = true, silent = true })
end

function M.lines(card)
  local lines = {
    "Patch",
    string.rep("-", 32),
    card.explanation or card.title,
    "",
  }

  for _, patch in ipairs(card.patches or {}) do
    table.insert(lines, "File: " .. patch.file)
    table.insert(lines, "")

    for line in patch.diff:gmatch("[^\n]+") do
      table.insert(lines, line)
    end

    table.insert(lines, "")
  end

  table.insert(lines, "[a] Apply  [r] Retry  [q] Reject")

  return lines
end

function M.accept()
  local card = state.card
  local patch_ids = {}
  local changed_files = {}

  for _, patch in ipairs(card.patches or {}) do
    local ok, error = apply.patch(patch)

    if not ok then
      ui.notify(error, vim.log.levels.ERROR)
      M.send(false, patch_ids, changed_files, error)

      return
    end

    table.insert(patch_ids, patch.id)
    table.insert(changed_files, patch.file)
  end

  M.send(true, patch_ids, changed_files, nil)
end

function M.reject()
  local card = state.card
  local patch_ids = {}

  for _, patch in ipairs(card.patches or {}) do
    table.insert(patch_ids, patch.id)
  end

  M.send(false, patch_ids, {}, nil)
end

function M.send(accepted, patch_ids, changed_files, error)
  local session_id = state.session_id
  local request_id = thinking.start("Applying", session_id)

  rpc.request("patch/apply_result", {
    session_id = session_id,
    card_id = state.card and state.card.id or "",
    accepted = accepted,
    patch_ids = patch_ids,
    changed_files = changed_files,
    error = error,
  }, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      log.write("patch apply error", message.error)
      ui.notify(message.error.message, vim.log.levels.ERROR)

      return
    end

    if message.result.session_id ~= state.session_id then
      log.write("stale patch result", message.result)

      return
    end

    state.token_usage = message.result.token_usage
    require("pair.card").show(message.result.card)
  end)
end

return M
