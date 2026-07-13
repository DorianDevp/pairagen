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
  if type(file) ~= "string" or file == "" then
    return false
  end

  local root = vim.uv.fs_realpath(vim.fn.getcwd()) or vim.fs.normalize(vim.fn.getcwd())
  local target = vim.fn.fnamemodify(file, ":p")
  target = vim.uv.fs_realpath(target) or vim.fs.normalize(target)

  return target == root or vim.startswith(target, root .. "/")
end

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

  if action == "why" then
    local diff = require("pair.diff")
    if diff.valid_preview() then
      diff.restore_source()
    end
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

  if action == "run_check" then
    M.run_check()
    return
  end

  if not M.confirm_agent_turn(action) then
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
    ui.notify(string.format(
      "Pair check found %s error%s. First: %s:%s %s",
      #report.errors,
      #report.errors == 1 and "" or "s",
      first.file,
      first.line,
      first.message
    ), vim.log.levels.ERROR)
  elseif report.checked_files > 0 then
    ui.notify(string.format(
      "Pair check passed: no editor errors in %s changed buffer%s",
      report.checked_files,
      report.checked_files == 1 and "" or "s"
    ))
  else
    ui.notify("Pair check unavailable: no changed buffers are loaded", vim.log.levels.WARN)
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

  local question = string.format(
    "Pair session used %s tokens (budget %s).\nStart another agent turn?",
    used,
    budget
  )

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
  state.completion_checked_card = nil
  state.details_card = nil
  state.details_expanded = false
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
