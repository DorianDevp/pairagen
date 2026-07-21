local config = require("loopbiotic.config")
local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

---@class LoopbioticCard a card proposed by the backend
---@field id string
---@field kind string "hypothesis" | "finding" | "patch" | "working" | "summary" | "error" | "deny" | "choice"
---@field title? string
---@field location? table { file, line, column, annotation? }
---@field evidence? table location-shaped evidence (hypothesis cards)
---@field next_move? table { kind = "open_location", file, line, column }
---@field claim? string hypothesis cards
---@field finding? string finding cards
---@field annotation? string finding cards
---@field flow_path? string[] ordered ids from the editor-resolved Flow graph
---@field explanation? string patch cards
---@field patches? { id: string, file: string, diff: string }[] patch cards
---@field file_ops? { id?: string, kind: string, from: string, to: string }[] patch cards proposing moves
---@field warnings? string[] patch cards
---@field changed_files? string[]
---@field summary? string summary cards
---@field message? string error cards
---@field reason? string deny cards
---@field question? string choice cards
---@field options? { id?: string, label?: string }[] choice cards
---@field preview? table non-actionable streamed { title, body? } for working cards

local M = {}

function M.show(card, opts)
  opts = opts or {}
  if state.card ~= card then
    state.card_flow_active = false
  end
  local diff = require("loopbiotic.diff")
  if card.kind ~= "patch" and diff.valid_preview() then
    -- Cleaning up a stale preview because a non-patch card (often a background
    -- progress tick) arrived must not steal the user's focus into the diff.
    diff.restore_source(nil, { focus = opts.enter == true })
  end
  if card.kind ~= "patch" and require("loopbiotic.fileops").pending() then
    require("loopbiotic.fileops").clear()
  end
  if state.details_card ~= card then
    state.details_card = card
    state.details_expanded = false
  end
  state.card = card
  surfaces.set_agent_working(card.kind == "working")

  if card.kind == "patch" then
    local fileops = require("loopbiotic.fileops")
    if fileops.present(card) then
      -- A patch card may carry file operations instead of diff hunks; they
      -- review through the same Accept/Reject gate as a patch. An invalid
      -- proposal falls through to the inert card rendering below.
      if fileops.show(card, opts) then
        return
      end
    else
      if diff.valid_preview() then
        diff.controls(card, opts)
        return
      end
      if diff.show(card, opts) then
        return
      end
    end
  end

  local lines = M.lines(card)
  local width = M.width(lines)
  local rendered_lines, rendered_width, flow_visible, flow_nowrap = M.workspace(lines, width, card)
  local selected_path = flow_visible and not state.card_flow_active and type(card.flow_path) == "table"
  local height = M.height(rendered_lines, rendered_width, state.details_expanded or selected_path)
  surfaces.render_agent(rendered_lines, {
    view = card.kind == "working" and "working" or "response",
    working = card.kind == "working",
    wrap = not flow_nowrap,
    cursorline = state.card_flow_active == true,
    enter = opts.enter == true,
    window = {
      width = rendered_width,
      height = height,
      anchor = M.anchor(card),
      title = " Loopbiotic: " .. M.title(card.kind) .. (flow_visible and " · Flow " or " "),
      title_pos = "left",
    },
    bind = function(active_buf, active_win)
      M.bind(active_buf, card)
      M.highlight(active_buf, rendered_lines, card)
      if state.card_flow_active and state.call_hierarchy then
        local flow_row = math.min(3 + (state.call_hierarchy.view_cursor or 1), math.max(#rendered_lines, 1))
        pcall(vim.api.nvim_win_set_cursor, active_win, { flow_row, 0 })
      end
    end,
  })
end

function M.workspace(lines, content_width, card)
  local graph = state.call_hierarchy
  local flow_options = config.values.flow or {}
  if flow_options.enabled == false or not graph then
    return lines, content_width, false, false
  end
  local widget = require("loopbiotic.widgets").validate({
    id = "flow:" .. tostring(card and card.id or "response"),
    kind = "flow",
    version = 1,
    title = "Flow",
    data = { graph = graph },
    provenance = "lsp",
    intents = { "navigate", "expand", "select_context", "inspect" },
  })
  if not widget then
    return lines, content_width, false, false
  end
  local has_path = type(card) == "table" and type(card.flow_path) == "table" and #card.flow_path > 0
  if not has_path and not state.card_flow_active then
    return lines, content_width, false, false
  end
  local viewport = ui.viewport()
  local wide = viewport.width >= (flow_options.responsive_split or 120)
  local flow_width = math.min(flow_options.panel_width or 52, math.max(viewport.width - content_width - 5, 24))
  local flow = require("loopbiotic.flow")
  local flow_lines
  if state.card_flow_active then
    flow_lines = flow.lines(graph, flow_width - 2)
  else
    local resolved_ids
    flow_lines, resolved_ids = flow.path_lines(graph, card.flow_path, flow_width - 2)
    if #resolved_ids == 0 then
      return lines, content_width, false, false
    end
  end

  if not wide then
    if state.card_flow_active then
      return flow_lines, math.min(flow_width, math.max(viewport.width - 2, 1)), true, true
    end
    local stacked = vim.deepcopy(lines)
    table.insert(stacked, "")
    table.insert(stacked, string.rep("─", math.min(math.max(content_width, 8), 32)))
    vim.list_extend(stacked, flow_lines)
    return stacked, math.max(content_width, math.min(flow_width, math.max(viewport.width - 2, 1))), true, false
  end

  local width = math.min(content_width + flow_width + 1, math.max(viewport.width - 2, 1))
  local left_width = math.max(width - flow_width - 1, 16)
  local combined = {}
  for index = 1, math.max(#lines, #flow_lines) do
    local left = M.short(lines[index] or "", left_width)
    local padding = string.rep(" ", math.max(left_width - vim.fn.strdisplaywidth(left), 0))
    table.insert(combined, left .. padding .. "│" .. (flow_lines[index] or ""))
  end
  return combined, width, true, true
end

function M.refresh_flow(graph)
  if graph ~= state.call_hierarchy or not state.card then
    return
  end
  if surfaces.agent_mode() ~= "closed" then
    M.show(state.card, { enter = false, flow_refresh = true })
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
      table.insert(lines, "Next    Reply or quit")
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
  local parts = { M.hint(config.values.keymaps.hide, "Wrap"), M.hint("q", "Quit") }
  if card.kind ~= "working" then
    table.insert(parts, 1, M.hint("m", "Reply"))
  end

  if card.kind ~= "working" and state.call_hierarchy and (config.values.flow or {}).enabled ~= false then
    table.insert(parts, M.hint(config.values.keymaps.flow or "F", state.card_flow_active and "Context" or "Flow"))
  end

  if type(card.flow_path) == "table" and state.call_hierarchy then
    local _, path_ids = require("loopbiotic.flow").path_lines(state.call_hierarchy, card.flow_path, 80)
    if #path_ids > 0 then
      table.insert(parts, M.hint("1-" .. math.min(#path_ids, 9), "Open path node"))
    end
  end

  if M.location(card) then
    table.insert(parts, M.hint(config.values.keymaps.go_to, "Go to line"))
  end

  if M.details_available(card) then
    local text = state.details_expanded and "Collapse details" or "Expand details"
    table.insert(parts, M.hint(config.values.keymaps.details or "z", text))
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

function M.hint(key, text)
  return string.format("[%s] %s", key or "?", text)
end

function M.bind(buf, card)
  if card.kind == "working" then
    vim.keymap.set("n", "q", require("loopbiotic").stop, { buffer = buf, nowait = true, silent = true })
    return
  end

  if state.call_hierarchy and (config.values.flow or {}).enabled ~= false then
    local flow_key = config.values.keymaps.flow or "F"
    if flow_key ~= "" then
      vim.keymap.set("n", flow_key, function()
        state.card_flow_active = not state.card_flow_active
        M.show(card, { enter = true })
      end, { buffer = buf, nowait = true, silent = true })
    end
  end

  if state.card_flow_active and state.call_hierarchy and (config.values.flow or {}).enabled ~= false then
    local flow = require("loopbiotic.flow")
    -- Route graph notifications back to the card so in-session Flow navigation
    -- (move/expand) actually redraws; the prompt only owns this listener while
    -- its own window is open (and clears it on close).
    flow.set_listener(state.call_hierarchy, M.refresh_flow)
    vim.keymap.set("n", "j", function()
      flow.move(state.call_hierarchy, 1)
    end, { buffer = buf, nowait = true, silent = true })
    vim.keymap.set("n", "k", function()
      flow.move(state.call_hierarchy, -1)
    end, { buffer = buf, nowait = true, silent = true })
    vim.keymap.set("n", "h", function()
      flow.collapse(state.call_hierarchy)
    end, { buffer = buf, nowait = true, silent = true })
    vim.keymap.set("n", "l", function()
      flow.expand_current(state.call_hierarchy)
    end, { buffer = buf, nowait = true, silent = true })
    vim.keymap.set("n", "u", function()
      flow.toggle_uses(state.call_hierarchy)
    end, { buffer = buf, nowait = true, silent = true })
    vim.keymap.set("n", "<CR>", function()
      flow.open_current(state.call_hierarchy)
    end, { buffer = buf, nowait = true, silent = true })
    vim.keymap.set("n", "s", function()
      local ref = require("loopbiotic.widgets").flow_ref(state.call_hierarchy)
      if ref then
        require("loopbiotic.widgets").toggle(ref)
        M.show(card, { enter = true })
      end
    end, { buffer = buf, nowait = true, silent = true })
    vim.keymap.set("n", "R", function()
      state.call_hierarchy = flow.root_here(state.call_hierarchy)
      M.refresh_flow(state.call_hierarchy)
    end, { buffer = buf, nowait = true, silent = true })
    return
  end

  -- The AgentWindow buffer is reused across renders, so Flow-mode bindings
  -- from a previous render must be cleared or they hijack normal card
  -- navigation (j/k scroll, <CR>, etc.) for the rest of the session.
  for _, key in ipairs({ "j", "k", "h", "l", "u", "<CR>", "s", "R" }) do
    pcall(vim.keymap.del, "n", key, { buffer = buf })
  end
  -- Leaving Flow mode: stop routing graph notifications to the card, unless
  -- the prompt window is open and owns the listener for its own rendering.
  if state.call_hierarchy and not surfaces.prompt_open() then
    require("loopbiotic.flow").set_listener(state.call_hierarchy, nil)
  end

  if type(card.flow_path) == "table" and state.call_hierarchy then
    local flow = require("loopbiotic.flow")
    local _, path_ids = flow.path_lines(state.call_hierarchy, card.flow_path, 80)
    for index, node_id in ipairs(path_ids) do
      if index > 9 then
        break
      end
      vim.keymap.set("n", tostring(index), function()
        flow.open_node(state.call_hierarchy, node_id)
      end, { buffer = buf, nowait = true, silent = true })
    end
  end

  vim.keymap.set("n", "m", function()
    require("loopbiotic.scope").run("reply", require("loopbiotic").reply_prompt)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "q", function()
    require("loopbiotic").stop()
  end, { buffer = buf, nowait = true, silent = true })
  local details_key = config.values.keymaps.details or "z"
  pcall(vim.keymap.del, "n", details_key, { buffer = buf })
  if M.details_available(card) then
    vim.keymap.set("n", details_key, function()
      M.toggle_details(card)
    end, { buffer = buf, nowait = true, silent = true })
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
