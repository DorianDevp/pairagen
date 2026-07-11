local config = require("pair.config")
local navigation = require("pair.navigation")
local state = require("pair.state")
local status = require("pair.status")
local ui = require("pair.ui")

local M = {}

local labels = {
  reply = { "m", "Message" },
  follow = { "f", "Follow" },
  why = { "w", "Why" },
  fix = { "x", "Fix" },
  other_lead = { "n", "Other" },
  apply = { "a", "Apply" },
  apply_patch = { "a", "Apply" },
  retry = { "r", "Retry" },
  edit_prompt = { "e", "Edit" },
  open = { "o", "Open" },
  run_check = { "t", "Check" },
  next = { "n", "Next patch" },
  stop = { "q", "Stop" },
}

function M.show(card)
  state.card = card
  state.last_card = card
  status.hide()

  local lines = M.lines(card)
  local width = M.width(lines)
  local height = M.height(lines, width)
  local buf, win = ui.render(state.card_buf, state.card_win, lines, {
    width = width,
    height = height,
  })

  state.card_buf = buf
  state.card_win = win
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true

  M.bind(buf, card)
  ui.focus(win)

  if card.kind == "patch" then
    require("pair.diff").show(card)
  end
end

function M.lines(card)
  local lines = {
    M.title(card.kind),
    string.rep("-", 32),
  }
  vim.list_extend(lines, M.actions(card))
  table.insert(lines, "")
  M.goal(lines)

  if card.kind == "hypothesis" then
    M.add(lines, card.claim or card.title)
    M.signal(lines, type(card.evidence) == "table" and card.evidence.annotation)
  elseif card.kind == "finding" then
    M.add(lines, card.finding or card.title)
    M.signal(lines, card.annotation)
  elseif card.kind == "patch" then
    M.add(lines, card.explanation or card.title)
    for _, warning in ipairs(card.warnings or {}) do
      M.signal(lines, warning)
    end
    table.insert(lines, "")
    table.insert(lines, tostring(#(card.patches or {})) .. " file patch pending")
  elseif card.kind == "summary" then
    M.add(lines, card.summary or card.title)
  elseif card.kind == "error" then
    M.add(lines, card.message or card.title)
  elseif card.kind == "choice" then
    M.add(lines, card.question or card.title)
  end

  local location = M.location(card)
  if location then
    table.insert(lines, "")
    table.insert(lines, string.format("Location: %s:%s", location.file or "", location.line or 1))
  end

  M.tokens(lines)

  return lines
end

function M.goal(lines)
  local goal = state.goal
  if not goal or not goal.statement or goal.statement == "" then
    return
  end

  table.insert(lines, "Active goal: " .. M.short(goal.statement, 56))
  local completed = #(goal.completed_steps or {})
  if completed > 0 then
    table.insert(lines, string.format("Progress: %d accepted local step%s", completed, completed == 1 and "" or "s"))
  end
  M.observation_network(lines, goal.known_observations or {})
  table.insert(lines, "")
end

function M.observation_network(lines, observations)
  if #observations == 0 then
    return
  end

  local rows = { "Context: " }
  for index, observation in ipairs(observations) do
    local node = M.observation_node(observation, index)
    local separator = rows[#rows] == "Context: " and "" or "--"
    if #rows[#rows] + #separator + #node > 68 then
      table.insert(rows, "         " .. node)
    else
      rows[#rows] = rows[#rows] .. separator .. node
    end
  end
  for _, row in ipairs(rows) do
    table.insert(lines, row)
  end
end

function M.observation_node(observation, index)
  local kind = observation.kind == "hypothesis" and "H" or observation.kind == "signal" and "S" or "F"
  local active = observation.active and "*" or "."
  local repeats = (observation.occurrences or 1) > 1 and "x" .. observation.occurrences or ""

  return string.format("[%s%d%s%s]", kind, index, active, repeats)
end

function M.short(text, limit)
  text = M.one_line(text)
  if #text <= limit then
    return text
  end

  return text:sub(1, limit - 3) .. "..."
end

function M.one_line(text)
  return tostring(text or ""):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
end

function M.location(card)
  if type(card.next_move) == "table" and card.next_move.kind == "open_location" then
    return card.next_move
  end
  if type(card.evidence) == "table" then
    return card.evidence
  end
  if type(card.location) == "table" then
    return card.location
  end

  return nil
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
  table.insert(lines, string.format(
    "Turn: in %s / out %s / total %s%s",
    usage.input_tokens or 0,
    usage.output_tokens or 0,
    usage.total_tokens or 0,
    usage.estimated and " est" or ""
  ))

  local total = state.token_usage
  if total and total.total_tokens ~= usage.total_tokens then
    table.insert(lines, string.format("Session total: %s", total.total_tokens or 0))
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
  local parts = { "[m] Message", "[h] Hide" }

  for _, action in ipairs(actions) do
    local name = type(action) == "table" and "apply_patch" or action
    local label = labels[name]

    if label then
      table.insert(parts, "[" .. label[1] .. "] " .. label[2])
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

function M.bind(buf, card)
  local actions = card.actions or card.next_actions or {}

  vim.keymap.set("n", "h", function()
    require("pair").hide()
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "m", function()
    require("pair").reply_prompt()
  end, { buffer = buf, nowait = true, silent = true })

  for _, action in ipairs(actions) do
    local name = type(action) == "table" and "apply" or action
    local label = labels[name]

    if label then
      vim.keymap.set("n", label[1], function()
        require("pair").action(name)
      end, { buffer = buf, nowait = true, silent = true })
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

function M.height(lines, width)
  local height = 0
  for _, line in ipairs(lines) do
    height = height + math.max(math.ceil(vim.fn.strdisplaywidth(line) / math.max(width, 1)), 1)
  end

  return math.min(height, config.values.card.max_height)
end

return M
