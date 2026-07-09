local config = require("pair.config")
local navigation = require("pair.navigation")
local state = require("pair.state")
local ui = require("pair.ui")

local M = {}

local labels = {
  follow = { "f", "Follow" },
  why = { "w", "Why" },
  fix = { "x", "Fix" },
  other_lead = { "n", "Other" },
  apply = { "a", "Apply" },
  retry = { "r", "Retry" },
  edit_prompt = { "e", "Edit" },
  open = { "o", "Open" },
  run_check = { "t", "Check" },
  next = { "n", "Next" },
  stop = { "q", "Stop" },
}

function M.show(card)
  state.card = card
  navigation.from_card(card)

  ui.close(state.card_win)

  local lines = M.lines(card)
  local width = M.width(lines)
  local buf, win = ui.float(lines, {
    width = width,
    height = math.min(#lines, config.values.card.max_height),
  })

  state.card_buf = buf
  state.card_win = win

  M.bind(buf, card)

  if card.kind == "patch" then
    require("pair.diff").show(card)
  end
end

function M.lines(card)
  local lines = {
    M.title(card.kind),
    string.rep("-", 32),
  }

  if card.kind == "hypothesis" then
    M.add(lines, card.claim or card.title)
    M.signal(lines, type(card.evidence) == "table" and card.evidence.annotation)
  elseif card.kind == "finding" then
    M.add(lines, card.finding or card.title)
    M.signal(lines, card.annotation)
  elseif card.kind == "patch" then
    M.add(lines, card.explanation or card.title)
    table.insert(lines, "")
    table.insert(lines, tostring(#(card.patches or {})) .. " file patch pending")
  elseif card.kind == "summary" then
    M.add(lines, card.summary or card.title)
  elseif card.kind == "error" then
    M.add(lines, card.message or card.title)
  elseif card.kind == "choice" then
    M.add(lines, card.question or card.title)
  end

  table.insert(lines, "")
  table.insert(lines, M.actions(card))

  return lines
end

function M.add(lines, text)
  text = tostring(text or "")

  for line in (text .. "\n"):gmatch("([^\n]*)\n") do
    table.insert(lines, line)
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
  local parts = {}

  for _, action in ipairs(actions) do
    local name = type(action) == "table" and "apply_patch" or action
    local label = labels[name]

    if label then
      table.insert(parts, "[" .. label[1] .. "] " .. label[2])
    end
  end

  return table.concat(parts, "  ")
end

function M.bind(buf, card)
  local actions = card.actions or card.next_actions or {}

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

return M
