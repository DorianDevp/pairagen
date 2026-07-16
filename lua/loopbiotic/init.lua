local card = require("loopbiotic.card")
local config = require("loopbiotic.config")
local context = require("loopbiotic.context")
local log = require("loopbiotic.log")
local navigation = require("loopbiotic.navigation")
local prompt = require("loopbiotic.prompt")
local rpc = require("loopbiotic.rpc")
local session = require("loopbiotic.session")
local state = require("loopbiotic.state")
local status = require("loopbiotic.status")
local thinking = require("loopbiotic.thinking")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

local M = {}

rpc.on("agent/progress", function(progress)
  thinking.progress(progress)
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

-- A running goal may need its next buffer. Opening a workspace file is
-- reversible and the resulting hunk still requires explicit acceptance, so
-- navigation itself does not interrupt the process with another prompt.
rpc.on_request("editor/open_location", function(params, respond)
  local location = params.location or {}
  local file = location.file or "?"
  local target = vim.fn.fnamemodify(file, ":p")
  local missing = vim.uv.fs_stat(target) == nil and vim.fn.bufloaded(target) == 0

  if M.workspace_location(file) and missing then
    respond({ granted = true, context = context.new_file(file) })
  elseif M.workspace_location(file) and navigation.open_location(location) then
    respond({ granted = true, context = context.session() })
  else
    respond({ granted = false })
  end
end)

function M.workspace_location(file)
  return util.in_workspace(file)
end

function M.setup(opts)
  config.setup(opts)
  require("loopbiotic.commands").setup()
  require("loopbiotic.keymaps").setup()
  local group = vim.api.nvim_create_augroup("LoopbioticCardTabFollow", { clear = true })
  vim.api.nvim_create_autocmd("TabEnter", {
    group = group,
    callback = function()
      vim.schedule(function()
        if state.card and state.session_id and not state.thinking_request_id then
          card.show(state.card)
        end
      end)
    end,
  })
end

function M.prompt(mode)
  prompt.open(mode or config.values.backend.mode)
end

function M.reply_prompt()
  if not state.session_id then
    ui.notify("No active session", vim.log.levels.WARN)

    return
  end
  if not M.require_actions_visible() then
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
    status = "active",
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
    session.apply_turn_result(message.result)
  end)
end

function M.action(action, opts)
  opts = opts or {}
  if not state.session_id then
    ui.notify("No active session", vim.log.levels.WARN)

    return
  end
  if not opts.allow_hidden and not M.require_actions_visible() then
    return
  end
  if not M.action_available(state.card, action) then
    ui.notify("Action is not available on this Loopbiotic card", vim.log.levels.WARN)
    return
  end

  if action == "why" then
    local diff = require("loopbiotic.diff")
    if diff.valid_preview() then
      diff.restore_source()
    end
  end

  if action == "apply" and state.card and state.card.kind == "patch" then
    require("loopbiotic.diff").show(state.card)

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

  if action == "run_check" then
    M.run_check()
    return
  end

  if not M.confirm_agent_turn(action) then
    return
  end

  ui.notify("Loopbiotic: " .. action)
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

    session.apply_turn_result(message.result)
  end)
end

function M.editor_check(files)
  local report = { checked_files = 0, errors = {} }
  local seen = {}

  for _, file in ipairs(files or {}) do
    local target = vim.fn.fnamemodify(file, ":p")
    local buf = vim.fn.bufnr(target)
    if buf >= 0 and vim.api.nvim_buf_is_loaded(buf) and not seen[buf] then
      seen[buf] = true
      report.checked_files = report.checked_files + 1
      for _, diagnostic in ipairs(vim.diagnostic.get(buf, { severity = vim.diagnostic.severity.ERROR })) do
        table.insert(report.errors, {
          file = vim.fn.fnamemodify(target, ":."),
          line = diagnostic.lnum + 1,
          message = context.truncate(diagnostic.message, config.values.context.max_diagnostic_length),
        })
      end
    end
  end

  return report
end

function M.run_check()
  local active = state.card or state.last_card or {}
  local report = M.editor_check(active.changed_files or {})
  log.event("editor_check", report)

  if #report.errors > 0 then
    local first = report.errors[1]
    ui.notify(
      string.format(
        "Loopbiotic check found %s error%s. First: %s:%s %s",
        #report.errors,
        #report.errors == 1 and "" or "s",
        first.file,
        first.line,
        first.message
      ),
      vim.log.levels.ERROR
    )
  elseif report.checked_files > 0 then
    ui.notify(
      string.format(
        "Loopbiotic check passed: no editor errors in %s changed buffer%s",
        report.checked_files,
        report.checked_files == 1 and "" or "s"
      )
    )
  else
    ui.notify("Loopbiotic check unavailable: no changed buffers are loaded", vim.log.levels.WARN)
  end

  return report
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

  if not M.confirm_agent_turn("reply") then
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

    session.apply_turn_result(message.result)
  end)
end

function M.token_budget_exceeded()
  local budget = tonumber(config.values.backend.token_budget) or 0
  local used = state.token_usage and tonumber(state.token_usage.total_tokens) or 0

  return budget > 0 and used >= budget, used, budget
end

function M.confirm_agent_turn(action)
  if action == "apply" or action == "open" or action == "resume_draft" or action == "stop" then
    return true
  end

  local exceeded, used, budget = M.token_budget_exceeded()
  if not exceeded then
    return true
  end

  local question =
    string.format("Loopbiotic session used %s tokens (budget %s).\nStart another agent turn?", used, budget)

  return vim.fn.confirm(question, "&Continue\n&Cancel", 2, "Warning") == 1
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

  ui.notify("No Loopbiotic card to restore", vim.log.levels.WARN)
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
  M.action("retry", { allow_hidden = true })
end

function M.go_to()
  if state.card and state.card.kind == "patch" and require("loopbiotic.diff").focus_change() then
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

  ui.notify("No Loopbiotic location to open", vim.log.levels.WARN)
end

function M.actions_visible()
  return not state.thinking_request_id
    and state.card_win
    and vim.api.nvim_win_is_valid(state.card_win)
    and vim.api.nvim_win_get_tabpage(state.card_win) == vim.api.nvim_get_current_tabpage()
end

function M.action_available(active_card, action)
  if type(active_card) ~= "table" or type(action) ~= "string" then
    return false
  end

  for _, available in ipairs(active_card.actions or active_card.next_actions or {}) do
    if available == action or (action == "apply" and type(available) == "table") then
      return true
    end
  end

  return false
end

function M.require_actions_visible()
  if M.actions_visible() then
    return true
  end

  ui.notify(
    "Loopbiotic actions are hidden; " .. tostring(config.values.keymaps.resume) .. " shows them",
    vim.log.levels.WARN
  )
  return false
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
  require("loopbiotic.diff").restore_source()
  thinking.stop(true)
  ui.close(state.prompt_win)
  ui.close(state.prompt_frame_win)
  ui.close(state.card_win)
  ui.close(state.thinking_win)
  status.hide()
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

  config.agent(name)
  rpc.stop()
  ui.notify("Loopbiotic agent: " .. name)

  return name
end

function M.agents()
  return config.agent_names()
end

function M.model(name)
  if not name or name == "" then
    local model = config.model()

    ui.notify("Loopbiotic model: " .. (model or "default"))

    return model
  end

  if name == "default" or name == "none" then
    local _, saved, save_error = config.model("")
    rpc.stop()
    if save_error then
      ui.notify("Loopbiotic model: default (could not save: " .. save_error .. ")", vim.log.levels.WARN)
    else
      ui.notify("Loopbiotic model: default" .. (saved and " · saved" or ""))
    end

    return nil
  end

  local _, saved, save_error = config.model(name)
  rpc.stop()
  if save_error then
    ui.notify("Loopbiotic model: " .. name .. " (could not save: " .. save_error .. ")", vim.log.levels.WARN)
  else
    ui.notify("Loopbiotic model: " .. name .. (saved and " · saved" or ""))
  end

  return name
end

function M.models()
  return config.model_names()
end

return M
