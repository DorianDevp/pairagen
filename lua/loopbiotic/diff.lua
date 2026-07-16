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

  local source_lines = vim.api.nvim_buf_get_lines(source_buf, 0, -1, false)
  local hunk_ok, hunk = pcall(apply.parse_hunk, patch.diff)
  if not hunk_ok then
    ui.notify(hunk, vim.log.levels.ERROR)
    return false
  end
  local start_ok, source_start = pcall(apply.resolve_start, source_lines, hunk)
  if not start_ok then
    ui.notify(source_start, vim.log.levels.ERROR)
    return false
  end
  local draft_ok, draft_lines = pcall(apply.apply_diff, source_lines, patch.diff)
  if not draft_ok then
    ui.notify(draft_lines, vim.log.levels.ERROR)
    return false
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

  vim.keymap.set("n", "a", M.accept, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "q", M.reject, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "r", M.retry, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "w", function()
    require("loopbiotic").action("why")
  end, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "e", function()
    M.focus_change()
  end, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "g", M.focus_change, { buffer = buf, nowait = true, silent = true })
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
  local request_id = thinking.start(accepted and "Continuing" or "Reworking", session_id)

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
      log.write("patch apply error", message.error)
      ui.notify(message.error.message, vim.log.levels.ERROR)
      return
    end
    if message.result.session_id ~= state.session_id then
      log.write("stale patch result", message.result)
      return
    end

    -- Patch results historically never updated state.backend_model.
    session.apply_turn_result(message.result, { update_model = false })
  end)
end

-- Error boundary: the draft preview is reached from RPC callbacks and
-- keymaps; a preview bug must log and notify, not kill the session. The
-- guarded wrapper returns nil on error, which callers treat as "not shown".
M.show = util.guard("diff.show", M.show)

return M
