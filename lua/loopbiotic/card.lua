local config = require("loopbiotic.config")
local navigation = require("loopbiotic.navigation")
local state = require("loopbiotic.state")
local status = require("loopbiotic.status")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

---@class LoopbioticCard a card proposed by the backend
---@field id string
---@field kind string "hypothesis" | "finding" | "patch" | "working" | "summary" | "error" | "deny" | "choice"
---@field title? string
---@field actions? (string|table)[] available actions; tables carry apply_patch payloads
---@field next_actions? (string|table)[] legacy name for actions
---@field location? table { file, line, column, annotation? }
---@field evidence? table location-shaped evidence (hypothesis cards)
---@field next_move? table { kind = "open_location", file, line, column }
---@field claim? string hypothesis cards
---@field finding? string finding cards
---@field annotation? string finding cards
---@field explanation? string patch cards
---@field patches? { id: string, file: string, diff: string }[] patch cards
---@field warnings? string[] patch cards
---@field changed_files? string[]
---@field summary? string summary cards
---@field message? string error cards
---@field reason? string deny cards
---@field question? string choice cards
---@field options? { id?: string, label?: string }[] choice cards
---@field preview? table non-actionable streamed { title, body? } for working cards

local M = {}

local labels = {
  reply = { "m", "Message", "reply" },
  follow = { "f", "Follow", "follow" },
  why = { "w", "Why", "why" },
  resume_draft = { "b", "Back to draft", nil },
  fix = { "x", "Draft", "fix" },
  goal = { "G", "Goal", "goal" },
  cancel_turn = { "c", "Cancel", "cancel" },
  other_lead = { "n", "Other", "other_lead" },
  apply = { "a", "Review", "draft_accept" },
  apply_patch = { "a", "Review", "draft_accept" },
  retry = { "r", "Retry", nil },
  edit_prompt = { "e", "Edit", nil },
  open = { "o", "Open", "go_to" },
  run_check = { "t", "Check", nil },
  stop = { "q", "Stop", "stop" },
}

function M.show(card, opts)
  opts = opts or {}
  local diff = require("loopbiotic.diff")
  if card.kind ~= "patch" and diff.valid_preview() then
    diff.restore_source()
  end
  if state.details_card ~= card then
    state.details_card = card
    state.details_expanded = false
  end
  state.card = card
  state.last_card = card
  status.hide()

  if card.kind ~= "patch" and state.navigated_card ~= card then
    state.navigated_card = card
    local location = M.location(card)
    if location then
      navigation.open_location(location)
    end
  end

  if card.kind == "patch" then
    if diff.valid_preview() then
      diff.controls(card, opts)
      return
    end
    if diff.show(card, opts) then
      return
    end
  end

  local lines = M.lines(card)
  local width = M.width(lines)
  local height = M.height(lines, width, state.details_expanded)
  local buf, win = ui.render(state.card_buf, state.card_win, lines, {
    width = width,
    height = height,
    anchor = M.anchor(card),
    enter = opts.enter == true,
    title = " Loopbiotic: " .. M.title(card.kind) .. " ",
    title_pos = "left",
  })

  state.card_buf = buf
  state.card_win = win
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true
  vim.wo[win].cursorline = false

  M.bind(buf, card)
  M.highlight(buf, lines, card)

  if card.kind == "summary" and card.title == "Goal complete" and state.completion_checked_card ~= card.id then
    state.completion_checked_card = card.id
    vim.defer_fn(function()
      if state.card == card then
        require("loopbiotic").run_check()
      end
    end, 300)
  end
end

function M.lines(card)
  local lines = {}
  M.goal(lines)

  if card.kind == "hypothesis" then
    M.add(lines, card.claim or card.title)
    M.signal(lines, type(card.evidence) == "table" and card.evidence.annotation)
  elseif card.kind == "finding" then
    M.add(lines, card.finding or card.title)
    M.signal(lines, card.annotation)
  elseif card.kind == "patch" then
    local explanation = card.explanation or card.title
    if not state.details_expanded then
      explanation = M.short(explanation, 58)
    end
    M.add(lines, explanation)
    for _, warning in ipairs(card.warnings or {}) do
      M.signal(lines, warning)
    end
    table.insert(lines, "")
    table.insert(lines, tostring(#(card.patches or {})) .. " file patch pending")
  elseif card.kind == "working" then
    if type(card.preview) == "table" and type(card.preview.title) == "string" then
      table.insert(lines, "Draft · validating before actions")
      table.insert(lines, "")
      M.add(lines, card.preview.title)
      if type(card.preview.body) == "string" and card.preview.body ~= "" then
        table.insert(lines, "")
        M.add(lines, card.preview.body)
      end
    else
      table.insert(lines, card.message or card.title or "Agent is still working")
    end
    table.insert(lines, "")
    table.insert(lines, string.format("Phase  %s", card.phase or "working"))
    table.insert(
      lines,
      string.format(
        "Budget %sms · elapsed %sms",
        tonumber(card.deadline_ms) or 0,
        tonumber(card.elapsed_ms) or tonumber(card.deadline_ms) or 0
      )
    )
  elseif card.kind == "summary" then
    if card.title ~= "Stopped" then
      table.insert(lines, "Status  Goal complete")
      table.insert(lines, "Next    Run checks, send a message, or stop")
      table.insert(lines, "")
    end
    M.add(lines, card.summary or card.title)
  elseif card.kind == "error" then
    M.add(lines, card.message or card.title)
    for _, warning in ipairs(card.warnings or {}) do
      M.signal(lines, warning)
    end
  elseif card.kind == "deny" then
    table.insert(lines, "Agent could not proceed")
    table.insert(lines, "")
    M.add(lines, card.reason or card.title)
  elseif card.kind == "choice" then
    M.add(lines, card.question or card.title)
    for index, option in ipairs(card.options or {}) do
      table.insert(lines, "")
      M.add(lines, string.format("%d. %s", index, option.label or option.id or ""))
    end
  end

  local location = M.location(card)
  if location then
    table.insert(lines, "")
    table.insert(lines, string.format("At  %s:%s", vim.fn.fnamemodify(location.file or "", ":~:."), location.line or 1))
  end

  M.tokens(lines)
  table.insert(lines, "")
  vim.list_extend(lines, M.actions(card))

  return lines
end

function M.has_action(card, expected)
  for _, action in ipairs(card.actions or card.next_actions or {}) do
    if action == expected then
      return true
    end
  end
  return false
end

function M.goal(lines)
  local goal = state.goal
  if not goal or not goal.statement or goal.statement == "" then
    return
  end

  local statement = state.details_expanded and M.one_line(goal.statement) or M.short(goal.statement, 54)
  table.insert(lines, "Goal  " .. statement)
  local completed = #(goal.completed_steps or {})
  if completed > 0 then
    table.insert(lines, string.format("Done  %d local step%s", completed, completed == 1 and "" or "s"))
  end
  if goal.status == "complete" then
    table.insert(lines, "State Goal complete")
  elseif goal.status == "paused" then
    table.insert(lines, "State Goal paused")
  elseif goal.next_step and goal.next_step ~= "" then
    table.insert(lines, "Now   " .. M.short(goal.next_step, 54))
  end
  M.observation_network(lines, goal.known_observations or {})
  table.insert(lines, "")
end

function M.observation_network(lines, observations)
  if #observations == 0 then
    return
  end

  local rows = { "Map   " }
  for index, observation in ipairs(observations) do
    local node = M.observation_node(observation, index)
    local separator = rows[#rows] == "Map   " and "" or "  "
    if #rows[#rows] + #separator + #node > 68 then
      table.insert(rows, "      " .. node)
    else
      rows[#rows] = rows[#rows] .. separator .. node
    end
  end
  for _, row in ipairs(rows) do
    table.insert(lines, row)
  end
end

function M.observation_node(observation, index)
  return util.observation_node(observation, index)
end

function M.short(text, limit)
  text = M.one_line(text)
  if vim.fn.strdisplaywidth(text) <= limit then
    return text
  end

  local count = vim.fn.strchars(text)
  local target = math.max(limit - 3, 0)
  while count > 0 and vim.fn.strdisplaywidth(vim.fn.strcharpart(text, 0, count)) > target do
    count = count - 1
  end

  return vim.fn.strcharpart(text, 0, count) .. "..."
end

function M.one_line(text)
  return tostring(text or ""):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
end

function M.goal_collapsible()
  local goal = state.goal
  return goal and goal.statement and vim.fn.strdisplaywidth(M.one_line(goal.statement)) > 54
end

function M.details_available(card)
  if M.goal_collapsible() then
    return true
  end

  local explanation = card and card.kind == "patch" and (card.explanation or card.title)
  return explanation and vim.fn.strdisplaywidth(M.one_line(explanation)) > 58
end

function M.toggle_details(card)
  if not card or not M.details_available(card) then
    return
  end

  state.details_expanded = not state.details_expanded
  M.show(card, { enter = true })
end

function M.location(card)
  return util.card_location(card)
end

function M.add(lines, text)
  text = tostring(text or "")

  for line in (text .. "\n"):gmatch("([^\n]*)\n") do
    table.insert(lines, line)
  end
end

function M.tokens(lines)
  local usage = state.turn_token_usage

  if not usage then
    return
  end

  table.insert(lines, "")

  local pricing = require("loopbiotic.pricing")
  local model = state.backend_model

  -- One usage row: input (with cached in parentheses) · output · cost.
  local function usage_line(label, u, estimated)
    local input = tonumber(u.input_tokens) or 0
    local cached = tonumber(u.cached_input_tokens) or 0
    local output = tonumber(u.output_tokens) or 0
    local text = string.format("%s in %s (%s cached) · out %s", label, input, cached, output)
    local cost = pricing.format(pricing.cost(u, model))
    if cost then
      text = text .. " · " .. cost
    end
    if estimated then
      text = text .. " · est"
    end
    table.insert(lines, text)
  end

  usage_line("Turn ", usage, usage.estimated)

  local total = state.token_usage
  if total and (tonumber(total.total_tokens) or 0) ~= (tonumber(usage.total_tokens) or 0) then
    usage_line("Total", total, total.estimated)
  end

  if total then
    local budget = tonumber(config.values.backend.token_budget) or 0
    local used = tonumber(total.total_tokens) or 0
    if budget > 0 then
      table.insert(lines, string.format("Budget %s/%s tokens", used, budget))
      if used >= budget then
        table.insert(lines, "Warning Session token budget exceeded")
      end
    end
  end

  local report = state.context_report
  if report and report.enabled then
    table.insert(
      lines,
      string.format(
        "Context %s/%s · %s fragments",
        report.used_tokens or 0,
        report.budget_tokens or 0,
        report.selected_count or 0
      )
    )
  end
end

function M.signal(lines, text)
  if not text or text == "" then
    return
  end

  table.insert(lines, "")
  table.insert(lines, "Signal:")
  M.add(lines, text)
end

function M.actions(card)
  local actions = card.actions or card.next_actions or {}
  local parts = {
    M.action_hint("reply", labels.reply),
    M.hint(config.values.keymaps.resume, "Focus"),
    M.hint(config.values.keymaps.hide, "Hide"),
  }

  if M.location(card) then
    table.insert(parts, M.hint(config.values.keymaps.go_to, "Go to line"))
  end

  if card.kind == "deny" and type(card.location) == "table" then
    table.insert(parts, M.hint("o", "Open & retry"))
  end

  if M.details_available(card) then
    local text = state.details_expanded and "Collapse details" or "Expand details"
    table.insert(parts, M.hint(config.values.keymaps.details or "z", text))
  end

  for _, action in ipairs(actions) do
    local name = type(action) == "table" and "apply_patch" or action
    local label = labels[name]

    if label and not (name == "open" and M.location(card)) then
      table.insert(parts, M.action_hint(name, label))
    end
  end

  local lines = { "" }
  for _, part in ipairs(parts) do
    local separator = lines[#lines] == "" and "" or "  "
    if #lines[#lines] + #separator + #part > 68 then
      table.insert(lines, part)
    else
      lines[#lines] = lines[#lines] .. separator .. part
    end
  end

  return lines
end

function M.action_hint(_, label)
  local key = label[3] and config.values.keymaps[label[3]] or label[1]
  return M.hint(key, label[2])
end

function M.hint(key, text)
  return string.format("[%s] %s", key or "?", text)
end

function M.bind(buf, card)
  local actions = card.actions or card.next_actions or {}

  vim.keymap.set("n", "h", function()
    require("loopbiotic").hide()
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "m", function()
    require("loopbiotic").reply_prompt()
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "g", function()
    require("loopbiotic").go_to()
  end, { buffer = buf, nowait = true, silent = true })

  if card.kind == "deny" and type(card.location) == "table" then
    vim.keymap.set("n", "o", function()
      require("loopbiotic").open_and_retry()
    end, { buffer = buf, nowait = true, silent = true })
  end
  local details_key = config.values.keymaps.details or "z"
  pcall(vim.keymap.del, "n", details_key, { buffer = buf })
  if M.details_available(card) then
    vim.keymap.set("n", details_key, function()
      M.toggle_details(card)
    end, { buffer = buf, nowait = true, silent = true })
  end

  for _, action in ipairs(actions) do
    local name = type(action) == "table" and "apply" or action
    local label = labels[name]

    if label then
      vim.keymap.set("n", label[1], function()
        require("loopbiotic").action(name)
      end, { buffer = buf, nowait = true, silent = true })
    end
  end
end

function M.anchor(card)
  local location = M.location(card)
  if location and location.file then
    local buf = require("loopbiotic.apply").buffer(location.file)
    local anchor = ui.buffer_anchor(buf, location.line, math.max((location.column or 1) - 1, 0))
    if anchor then
      return anchor
    end
  end

  local cursor = state.source_cursor or { 1, 0 }
  return ui.buffer_anchor(state.source_buf, cursor[1], cursor[2])
end

function M.highlight(buf, lines, card)
  vim.api.nvim_buf_clear_namespace(buf, -1, 0, -1)
  for index, line in ipairs(lines) do
    local group = line:match("^Goal") and "LoopbioticGoal"
      or line:match("^%[") and "LoopbioticAction"
      or line:match("^At  ") and "LoopbioticMuted"
      or line:match("tokens$") and "LoopbioticMuted"
      or card.kind == "error" and "DiagnosticError"
      or card.kind == "deny" and line == "Agent could not proceed" and "DiagnosticError"
    if group then
      vim.api.nvim_buf_add_highlight(buf, -1, group, index - 1, 0, -1)
    end
  end
end

function M.title(kind)
  return (kind or "card"):gsub("^%l", string.upper)
end

function M.width(lines)
  local width = 32

  for _, line in ipairs(lines) do
    width = math.max(width, #line + 2)
  end

  return math.min(width, config.values.card.max_width)
end

function M.height(lines, width, expanded)
  local height = 0
  for _, line in ipairs(lines) do
    height = height + math.max(math.ceil(vim.fn.strdisplaywidth(line) / math.max(width, 1)), 1)
  end

  if expanded then
    return height
  end

  return math.min(height, config.values.card.max_height)
end

-- Error boundary: card rendering is reached from RPC callbacks and keymaps;
-- a rendering bug must log and notify, not kill the surrounding session.
M.show = util.guard("card.show", M.show)

return M
