local apply = require("loopbiotic.apply")
local context = require("loopbiotic.context")
local log = require("loopbiotic.log")
local navigation = require("loopbiotic.navigation")
local rpc = require("loopbiotic.rpc")
local session = require("loopbiotic.session")
local state = require("loopbiotic.state")
local thinking = require("loopbiotic.thinking")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

local M = {}
local namespace = vim.api.nvim_create_namespace("loopbiotic-patch")

local function bind(buf, keys, callback)
  local seen = {}
  for _, key in ipairs(keys) do
    if key and key ~= "" and not seen[key] then
      seen[key] = true
      vim.keymap.set("n", key, callback, { buffer = buf, nowait = true, silent = true })
    end
  end
end

function M.show(card, opts)
  opts = opts or {}
  local patch = (card.patches or {})[1]

  if not patch then
    ui.notify("Patch card has no local change", vim.log.levels.ERROR)
    return false
  end

  local source_buf = apply.buffer(patch.file)
  if not source_buf then
    navigation.open_location({ file = patch.file, line = 1, column = 1 })
    source_buf = apply.buffer(patch.file)
  end
  if not source_buf or not vim.api.nvim_buf_is_valid(source_buf) then
    ui.notify("Open the proposed location before editing the patch", vim.log.levels.WARN)
    return false
  end

  local source_name = vim.fn.fnamemodify(vim.api.nvim_buf_get_name(source_buf), ":p")
  local patch_name = vim.fn.fnamemodify(patch.file, ":p")
  if source_name ~= patch_name then
    ui.notify("Patch target is not the currently accepted source location", vim.log.levels.WARN)
    return false
  end

  -- Queued patches were validated daemon-side, so a failure here means the
  -- draft went stale in review: parse failures are a malformed diff, while
  -- resolve/apply failures mean the buffer drifted after the draft was made
  -- (cards carry no queue-time changedtick, so resolution failure itself is
  -- the drift signal). Offer recovery instead of dead-ending the goal.
  local source_lines = vim.api.nvim_buf_get_lines(source_buf, 0, -1, false)
  local hunk_ok, hunk = pcall(apply.parse_hunk, patch.diff)
  if not hunk_ok then
    return M.recover(card, "malformed", hunk)
  end
  local start_ok, source_start = pcall(apply.resolve_start, source_lines, hunk)
  if not start_ok then
    return M.recover(card, "drift", source_start)
  end
  local draft_ok, draft_lines = pcall(apply.apply_diff, source_lines, patch.diff)
  if not draft_ok then
    return M.recover(card, "drift", draft_lines)
  end

  local annotations = M.annotations(hunk, source_start)
  local change_cursor = M.change_cursor(draft_lines, annotations)
  if
    not navigation.open_location({
      file = patch.file,
      line = change_cursor[1],
      column = change_cursor[2] + 1,
    })
  then
    ui.notify("Source location is not visible", vim.log.levels.WARN)
    return false
  end

  local source_win = vim.api.nvim_get_current_win()
  if vim.api.nvim_get_current_buf() ~= source_buf then
    ui.notify("Patch target did not open in the active editor window", vim.log.levels.WARN)
    return false
  end

  local draft_buf = vim.api.nvim_create_buf(false, true)
  vim.bo[draft_buf].buftype = "nofile"
  vim.bo[draft_buf].bufhidden = "wipe"
  vim.bo[draft_buf].swapfile = false
  vim.bo[draft_buf].modifiable = true
  vim.bo[draft_buf].filetype = vim.bo[source_buf].filetype
  vim.api.nvim_buf_set_name(draft_buf, "Loopbiotic draft: " .. patch.file)
  vim.api.nvim_buf_set_lines(draft_buf, 0, -1, false, draft_lines)

  vim.api.nvim_win_set_buf(source_win, draft_buf)

  state.diff_buf = draft_buf
  state.diff_win = source_win
  state.diff_source_buf = source_buf
  state.diff_source_tick = vim.api.nvim_buf_get_changedtick(source_buf)

  state.diff_first_row = annotations.first_row
  state.diff_cursor = change_cursor
  M.decorate(draft_buf, draft_lines, annotations, card.warnings or {})
  M.controls(card, opts)

  local keymaps = require("loopbiotic.config").values.keymaps
  vim.keymap.set("n", keymaps.draft_accept, M.accept, { buffer = draft_buf, nowait = true, silent = true })
  vim.keymap.set("n", keymaps.draft_reject, M.reject, { buffer = draft_buf, nowait = true, silent = true })
  vim.keymap.set("n", keymaps.draft_retry, M.retry, { buffer = draft_buf, nowait = true, silent = true })

  M.focus_change()

  return true
end

-- Decide what recovery to offer for a queued patch that failed to preview.
-- Pure (kinds and actions in, choices out) so headless tests can cover the
-- decision table directly.
--
-- Retrying redrafts the current goal slice against the live buffer, which is
-- cheap, so it comes first: recovery is one keypress. There is no "skip this
-- hunk" choice because no skip path exists — patch cards carry a single hunk
-- and rejecting a draft stops at an explicit retry/edit/stop decision.
---@param kind "malformed"|"drift" parse failure vs. stale buffer context
---@param actions (string|table)[]|nil the card's available actions
---@return { reason: string, choices: { label: string, action: "retry"|"cancel" }[] }|nil plan nil when the card cannot retry
function M.recovery_plan(kind, actions)
  local can_retry = false
  for _, action in ipairs(actions or {}) do
    if action == "retry" then
      can_retry = true
    end
  end
  if not can_retry then
    return nil
  end

  return {
    reason = kind == "malformed" and "the drafted patch is malformed"
      or "draft no longer matches the buffer (edited since it was drafted)",
    choices = {
      { label = "Retry slice with current buffer", action = "retry" },
      { label = "Cancel", action = "cancel" },
    },
  }
end

-- A queued patch failed to preview: prompt for recovery instead of leaving
-- the goal at a dead end. The retry turn costs tokens, so it only fires on
-- the user's explicit pick — never automatically. Always returns false so
-- callers fall back to the plain card while the choice is pending.
---@param card LoopbioticCard
---@param kind "malformed"|"drift"
---@param err string the underlying parse/resolve/apply error
---@return boolean shown always false
function M.recover(card, kind, err)
  log.write("patch preview failed", { kind = kind, error = err })

  local plan = M.recovery_plan(kind, card.actions or card.next_actions)
  if not plan then
    ui.notify(err, vim.log.levels.ERROR)
    return false
  end

  vim.ui.select(plan.choices, {
    prompt = "Loopbiotic: " .. plan.reason,
    format_item = function(choice)
      return choice.label
    end,
  }, function(choice)
    if choice and choice.action == "retry" then
      require("loopbiotic").action("retry", { allow_hidden = true })
    end
  end)

  return false
end

function M.annotations(hunk, source_start)
  local row = source_start
  local annotations = {
    first_row = source_start,
    added = {},
    removed = {},
  }

  for _, line in ipairs(hunk.lines) do
    if line.kind == "context" then
      row = row + 1
    elseif line.kind == "remove" then
      annotations.removed[row] = annotations.removed[row] or {}
      table.insert(annotations.removed[row], line.text)
    elseif line.kind == "add" then
      table.insert(annotations.added, row)
      row = row + 1
    end
  end

  return annotations
end

function M.change_cursor(draft_lines, annotations)
  local row = annotations.added[1] or annotations.first_row
  row = math.max(0, math.min(row, math.max(#draft_lines - 1, 0)))
  local line = draft_lines[row + 1] or ""
  local indentation = line:match("^%s*") or ""

  return { row + 1, #indentation }
end

function M.decorate(buf, draft_lines, annotations, warnings)
  for _, row in ipairs(annotations.added) do
    if row < #draft_lines then
      vim.api.nvim_buf_set_extmark(buf, namespace, row, 0, {
        end_row = row + 1,
        line_hl_group = "DiffAdd",
        virt_text = { { " +", "DiffAdd" } },
        virt_text_pos = "eol",
      })
    end
  end

  local virtual = {}
  for row, removed in pairs(annotations.removed) do
    virtual[row] = virtual[row] or {}
    for _, text in ipairs(removed) do
      table.insert(virtual[row], { { "- " .. text, "DiffDelete" } })
    end
  end

  if warnings[1] then
    local row = annotations.first_row
    virtual[row] = virtual[row] or {}
    table.insert(virtual[row], 1, { { "Warning: " .. warnings[1], "DiagnosticWarn" } })
  end

  for row, lines in pairs(virtual) do
    local anchor = math.min(row, math.max(#draft_lines - 1, 0))
    vim.api.nvim_buf_set_extmark(buf, namespace, anchor, 0, {
      virt_lines = lines,
      virt_lines_above = true,
    })
  end
end

function M.controls(card, opts)
  opts = opts or {}
  local keys = require("loopbiotic.config").values.keymaps
  local lines = M.control_lines(card, keys)

  local width = math.min(58, require("loopbiotic.config").values.card.max_width)
  local height = 0
  for _, line in ipairs(lines) do
    height = height + math.max(math.ceil(vim.fn.strdisplaywidth(line) / width), 1)
  end
  local buf, win = ui.render(state.card_buf, state.card_win, lines, {
    width = width,
    height = height,
    anchor = ui.buffer_anchor(
      state.diff_buf,
      (state.diff_cursor or { (state.diff_first_row or 0) + 1, 0 })[1],
      (state.diff_cursor or { 1, 0 })[2]
    ),
    anchor_gap = 1,
    avoid_anchor_row = true,
    enter = opts.enter == true,
    title = " Loopbiotic: Draft ",
  })
  state.card_buf = buf
  state.card_win = win
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true

  for index, line in ipairs(lines) do
    local group = line:match("^Goal") and "LoopbioticGoal" or line:match("^%[") and "LoopbioticAction"
    if group then
      vim.api.nvim_buf_add_highlight(buf, -1, group, index - 1, 0, -1)
    end
  end

  bind(buf, { "a", keys.draft_accept }, M.accept)
  bind(buf, { "q", keys.draft_reject }, M.reject)
  bind(buf, { "r", keys.draft_retry }, M.retry)
  bind(buf, { "w", keys.why }, function()
    require("loopbiotic").action("why")
  end)
  bind(buf, { "e", "g", keys.go_to }, function()
    M.focus_change()
  end)
  local details_key = keys.details or "z"
  pcall(vim.keymap.del, "n", details_key, { buffer = buf })
  if M.details_available(card) then
    vim.keymap.set("n", details_key, function()
      M.toggle_details(card)
    end, { buffer = buf, nowait = true, silent = true })
  end
end

function M.control_lines(card, keys)
  keys = keys or require("loopbiotic.config").values.keymaps
  local lines = {}
  if state.goal and state.goal.statement then
    local goal = state.details_expanded and M.one_line(state.goal.statement) or M.truncate(state.goal.statement, 52)
    table.insert(lines, "Goal  " .. goal)
    local completed = #(state.goal.completed_steps or {})
    if completed > 0 then
      table.insert(lines, "Done  " .. completed .. " accepted")
    end
    if state.goal.next_step and state.goal.next_step ~= "" then
      table.insert(lines, "Now   " .. M.truncate(state.goal.next_step, 52))
    end
    local network = M.observation_network(state.goal.known_observations or {})
    if network ~= "" then
      table.insert(lines, "Map   " .. network)
    end
  end

  local explanation = card.explanation or card.title or "Local change"
  table.insert(lines, state.details_expanded and M.one_line(explanation) or M.truncate(explanation, 58))
  table.insert(lines, "")
  if M.details_available(card) then
    local label = state.details_expanded and "Collapse details" or "Expand details"
    table.insert(lines, string.format("[%s] %s", keys.details or "z", label))
  end
  table.insert(lines, string.format("[%s] Back to proposal", keys.go_to))
  table.insert(lines, string.format("[%s] Accept   [%s] Reject", keys.draft_accept, keys.draft_reject))
  table.insert(lines, string.format("[%s] Why this hunk", keys.why or "w"))
  table.insert(lines, string.format("[%s] Retry    edit the draft directly", keys.draft_retry))
  if card.warnings and card.warnings[1] then
    table.insert(lines, "Warning shown at hunk")
  end

  return lines
end

function M.details_available(card)
  local goal = state.goal and state.goal.statement
  local explanation = card.explanation or card.title or "Local change"
  return goal and vim.fn.strdisplaywidth(M.one_line(goal)) > 52 or vim.fn.strdisplaywidth(M.one_line(explanation)) > 58
end

function M.toggle_details(card)
  state.details_expanded = not state.details_expanded
  require("loopbiotic.card").show(card, { enter = true })
end

function M.focus_change()
  if not M.valid_preview() then
    return false
  end

  vim.api.nvim_set_current_win(state.diff_win)
  local line_count = math.max(vim.api.nvim_buf_line_count(state.diff_buf), 1)
  local cursor = state.diff_cursor or { (state.diff_first_row or 0) + 1, 0 }
  vim.api.nvim_win_set_cursor(state.diff_win, {
    math.min(cursor[1], line_count),
    cursor[2],
  })
  vim.cmd("normal! zz")

  return true
end

function M.observation_network(observations)
  local nodes = {}
  for index, observation in ipairs(observations) do
    table.insert(nodes, util.observation_node(observation, index))
  end

  return table.concat(nodes, "--")
end

function M.truncate(text, limit)
  return require("loopbiotic.card").short(text, limit)
end

function M.one_line(text)
  return require("loopbiotic.card").one_line(text)
end

function M.accept()
  if not require("loopbiotic").require_actions_visible() then
    return
  end

  local card = state.card
  local patch = card and (card.patches or {})[1]
  local draft_buf = state.diff_buf
  local source_buf = state.diff_source_buf

  if not patch or not M.valid_preview() then
    ui.notify("Editable patch draft is unavailable", vim.log.levels.ERROR)
    return
  end
  if vim.api.nvim_buf_get_changedtick(source_buf) ~= state.diff_source_tick then
    ui.notify("Source changed while the draft was open", vim.log.levels.ERROR)
    return
  end

  local lines = vim.api.nvim_buf_get_lines(draft_buf, 0, -1, false)
  local cursor = vim.api.nvim_win_get_cursor(state.diff_win)
  vim.api.nvim_buf_set_lines(source_buf, 0, -1, false, lines)
  state.source_buf = source_buf
  state.source_cursor = cursor
  M.restore_source(cursor)
  M.send(true, { patch.id }, { patch.file }, nil)
end

function M.reject()
  if not require("loopbiotic").require_actions_visible() then
    return
  end

  local card = state.card
  local patch = card and (card.patches or {})[1]

  if not patch then
    return
  end

  M.restore_source()
  M.send(false, { patch.id }, {}, nil)
end

function M.retry()
  if not require("loopbiotic").require_actions_visible() then
    return
  end

  M.restore_source()
  require("loopbiotic").action("retry", { allow_hidden = true })
end

function M.valid_preview()
  return state.diff_buf
    and vim.api.nvim_buf_is_valid(state.diff_buf)
    and state.diff_source_buf
    and vim.api.nvim_buf_is_valid(state.diff_source_buf)
    and state.diff_win
    and vim.api.nvim_win_is_valid(state.diff_win)
end

function M.restore_source(cursor)
  local draft_buf = state.diff_buf
  local source_buf = state.diff_source_buf
  local win = state.diff_win

  if win and vim.api.nvim_win_is_valid(win) and source_buf and vim.api.nvim_buf_is_valid(source_buf) then
    vim.api.nvim_win_set_buf(win, source_buf)
    vim.api.nvim_set_current_win(win)

    if cursor then
      local line = math.min(cursor[1], vim.api.nvim_buf_line_count(source_buf))
      vim.api.nvim_win_set_cursor(win, { math.max(line, 1), cursor[2] })
    end
  end

  if draft_buf and vim.api.nvim_buf_is_valid(draft_buf) then
    pcall(vim.api.nvim_buf_delete, draft_buf, { force = true })
  end
  ui.close(state.card_win)
  state.card_win = nil

  state.diff_buf = nil
  state.diff_win = nil
  state.diff_source_buf = nil
  state.diff_source_tick = nil
  state.diff_first_row = nil
  state.diff_cursor = nil
end

function M.send(accepted, patch_ids, changed_files, error)
  local session_id = state.session_id
  state.accept_continuation = accepted == true
  local request_id = thinking.start(accepted and "Continuing" or "Rejecting", session_id)

  rpc.request("patch/apply_result", {
    session_id = session_id,
    card_id = state.card and state.card.id or "",
    accepted = accepted,
    patch_ids = patch_ids,
    changed_files = changed_files,
    error = error,
    context = context.session(),
  }, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      state.accept_continuation = nil
      log.write("patch apply error", message.error)
      ui.notify(message.error.message, vim.log.levels.ERROR)
      return
    end
    if message.result.session_id ~= state.session_id then
      state.accept_continuation = nil
      log.write("stale patch result", message.result)
      return
    end

    -- Patch results historically never updated state.backend_model.
    session.apply_turn_result(message.result, {
      update_model = false,
      track_backend_error = accepted,
    })
  end)
end

-- Error boundary: the draft preview is reached from RPC callbacks and
-- keymaps; a preview bug must log and notify, not kill the session. The
-- guarded wrapper returns nil on error, which callers treat as "not shown".
M.show = util.guard("diff.show", M.show)

return M
