local apply = require("loopbiotic.apply")
local context = require("loopbiotic.context")
local log = require("loopbiotic.log")
local navigation = require("loopbiotic.navigation")
local rpc = require("loopbiotic.rpc")
local session = require("loopbiotic.session")
local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
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
  if not util.in_workspace(patch.file) then
    ui.notify("Patch target is outside the workspace", vim.log.levels.ERROR)
    return false
  end

  local source_buf = apply.buffer(patch.file)
  local target = vim.fn.fnamemodify(patch.file, ":p")
  if source_buf and vim.uv.fs_stat(target) == nil then
    ui.notify("A loaded unsaved buffer already owns the proposed path", vim.log.levels.ERROR)
    return false
  end
  local is_new = source_buf == nil and vim.uv.fs_stat(target) == nil
  if is_new then
    local plan, reason = require("loopbiotic.creation").inspect(patch.file)
    if not plan then
      ui.notify(reason, vim.log.levels.ERROR)
      return false
    end
    state.creation = plan
    source_buf = vim.fn.bufadd(target)
    vim.fn.bufload(source_buf)
    M.open_creation_context(plan, source_buf)
  else
    state.creation = nil
  end
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

  -- A malformed or stale proposal cannot cross the review boundary. It is
  -- reported as inert content; replacement work can only be requested through
  -- PromptWindow.
  local source_lines = vim.api.nvim_buf_get_lines(source_buf, 0, -1, false)
  local hunk_ok, hunk = pcall(apply.parse_hunk, patch.diff)
  if not hunk_ok then
    log.write("patch preview failed", { kind = "malformed", error = hunk })
    ui.notify(hunk, vim.log.levels.ERROR)
    return false
  end
  local start_ok, source_start = pcall(apply.resolve_start, source_lines, hunk)
  if not start_ok then
    log.write("patch preview failed", { kind = "drift", error = source_start })
    ui.notify(source_start, vim.log.levels.ERROR)
    return false
  end
  local draft_ok, draft_lines = pcall(apply.apply_diff, source_lines, patch.diff)
  if not draft_ok then
    log.write("patch preview failed", { kind = "drift", error = draft_lines })
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

  M.focus_change()

  return true
end

function M.open_creation_context(plan, source_buf)
  state.creation_context_win = nil
  local source_win = navigation.normal_window()
  vim.api.nvim_set_current_win(source_win)
  local split_ok = pcall(vim.cmd, "vsplit")
  if not split_ok then
    vim.api.nvim_win_set_buf(source_win, source_buf)
    return false
  end
  local draft_win = vim.api.nvim_get_current_win()
  local netrw_win
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if win ~= draft_win and vim.api.nvim_win_get_config(win).relative == "" then
      netrw_win = win
      break
    end
  end
  if netrw_win then
    vim.api.nvim_set_current_win(netrw_win)
    pcall(vim.cmd, "edit " .. vim.fn.fnameescape(plan.existing_parent))
    -- Tracked so restore_source can close it; otherwise every new-file
    -- proposal leaks another parent-directory split for the rest of the
    -- session.
    state.creation_context_win = netrw_win
  end
  vim.api.nvim_set_current_win(draft_win)
  vim.api.nvim_win_set_buf(draft_win, source_buf)
  return netrw_win ~= nil
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
  surfaces.render_agent(lines, {
    view = "review",
    working = false,
    wrap = true,
    enter = opts.enter == true,
    window = {
      width = width,
      height = height,
      anchor = ui.buffer_anchor(
        state.diff_buf,
        (state.diff_cursor or { (state.diff_first_row or 0) + 1, 0 })[1],
        (state.diff_cursor or { 1, 0 })[2]
      ),
      anchor_gap = 1,
      avoid_anchor_row = true,
      title = " Loopbiotic: Review ",
    },
    bind = function(active_buf, active_win)
      M.bind_controls(active_buf, active_win, card, lines)
    end,
  })
end

function M.bind_controls(buf, _win, card, lines)
  local keys = require("loopbiotic.config").values.keymaps
  for index, line in ipairs(lines) do
    local group = line:match("^Goal") and "LoopbioticGoal" or line:match("^%[") and "LoopbioticAction"
    if group then
      vim.api.nvim_buf_add_highlight(buf, -1, group, index - 1, 0, -1)
    end
  end
  bind(buf, { keys.draft_accept }, M.accept)
  bind(buf, { keys.draft_reject }, M.reject)
  bind(buf, { keys.go_to }, function()
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
  if state.creation then
    table.insert(lines, "Create " .. state.creation.relative)
    for _, directory in ipairs(state.creation.missing_directories or {}) do
      table.insert(lines, "Parent " .. vim.fn.fnamemodify(directory, ":."))
    end
  end
  table.insert(lines, state.details_expanded and M.one_line(explanation) or M.truncate(explanation, 58))
  table.insert(lines, "")
  if M.details_available(card) then
    local label = state.details_expanded and "Collapse details" or "Expand details"
    table.insert(lines, string.format("[%s] %s", keys.details or "z", label))
  end
  table.insert(lines, string.format("[%s] Back to proposal", keys.go_to))
  table.insert(lines, string.format("[%s] Accept   [%s] Reject", keys.draft_accept, keys.draft_reject))
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
  if not require("loopbiotic.scope").allows("accept") then
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
  if state.creation then
    local ok, reason = require("loopbiotic.creation").commit(state.creation, lines)
    if not ok then
      ui.notify(reason, vim.log.levels.ERROR)
      return
    end
  end
  vim.api.nvim_buf_set_lines(source_buf, 0, -1, false, lines)
  if state.creation then
    vim.bo[source_buf].modified = false
  end
  state.source_buf = source_buf
  state.source_cursor = cursor
  M.restore_source(cursor)
  state.creation = nil
  M.send_accept({ patch.id }, { patch.file })
end

function M.reject()
  if not require("loopbiotic.scope").allows("reject") then
    return
  end

  local card = state.card
  local patch = card and (card.patches or {})[1]

  if not patch then
    return
  end

  M.restore_source()
  if state.goal then
    state.goal.status = "paused"
    state.goal.next_step = nil
  end

  M.show_paused("Accepted source was restored. The Goal is paused.")
  M.acknowledge_rejection({ patch.id })
  require("loopbiotic.prompt").reply()
end

-- The paused/rejected View shared by patch and file-operation rejection: the
-- rejected proposal is no longer actionable, only Reply and Quit remain.
function M.show_paused(detail)
  local lines = {
    "Proposal rejected",
    detail or "The Goal is paused.",
    "",
    "[m] Reply   [q] Quit",
  }
  surfaces.render_agent(lines, {
    view = "paused",
    working = false,
    enter = false,
    window = {
      width = 58,
      height = #lines,
      border = require("loopbiotic.config").values.card.border,
      title = " Loopbiotic: Paused ",
    },
    bind = function(buf)
      bind(buf, { "m" }, function()
        require("loopbiotic.scope").run("reply", require("loopbiotic").reply_prompt)
      end)
      bind(buf, { "q" }, require("loopbiotic").stop)
    end,
  })
end

function M.acknowledge_rejection(patch_ids)
  local session_id = state.session_id
  state.turn_barrier = true
  rpc.request("patch/apply_result", {
    session_id = session_id,
    card_id = state.card and state.card.id or "",
    accepted = false,
    patch_ids = patch_ids,
    changed_files = {},
    error = nil,
    context = context.session(),
  }, function(message)
    if message.error then
      log.write("patch rejection acknowledgement error", message.error)
      surfaces.render_agent({
        "Rejection could not be recorded",
        tostring(message.error.message),
        "The source is restored, but this session cannot safely continue.",
        "",
        "[q] Quit",
      }, {
        view = "error",
        working = false,
        enter = false,
        window = {
          width = 62,
          height = 5,
          border = require("loopbiotic.config").values.card.border,
          title = " Loopbiotic: Error ",
        },
        bind = function(buf)
          bind(buf, { "q" }, require("loopbiotic").stop)
        end,
      })
      return
    end
    if state.session_id == session_id and message.result then
      state.goal = message.result.goal or state.goal
      state.token_usage = message.result.token_usage or state.token_usage
    end
    state.turn_barrier = false
  end)
end

function M.valid_preview()
  return state.diff_buf
    and vim.api.nvim_buf_is_valid(state.diff_buf)
    and state.diff_source_buf
    and vim.api.nvim_buf_is_valid(state.diff_source_buf)
    and state.diff_win
    and vim.api.nvim_win_is_valid(state.diff_win)
end

-- opts.focus (default true): user-driven accept/reject return the cursor to
-- the source window; background cleanup (a non-patch card superseding a stale
-- preview, e.g. a progress tick mid-turn) passes focus=false so it swaps the
-- buffer back without yanking the user into the diff window.
function M.restore_source(cursor, opts)
  opts = opts or {}
  local focus = opts.focus ~= false
  local draft_buf = state.diff_buf
  local source_buf = state.diff_source_buf
  local win = state.diff_win
  local discard_creation = cursor == nil and state.creation ~= nil

  if win and vim.api.nvim_win_is_valid(win) and source_buf and vim.api.nvim_buf_is_valid(source_buf) then
    vim.api.nvim_win_set_buf(win, source_buf)
    if focus then
      vim.api.nvim_set_current_win(win)

      if cursor then
        local line = math.min(cursor[1], vim.api.nvim_buf_line_count(source_buf))
        vim.api.nvim_win_set_cursor(win, { math.max(line, 1), cursor[2] })
      end
    end
  end

  -- Close the parent-directory split opened for new-file review, unless it is
  -- the very window we just restored the source into.
  local context_win = state.creation_context_win
  if
    context_win
    and context_win ~= win
    and type(context_win) == "number"
    and vim.api.nvim_win_is_valid(context_win)
    and #vim.api.nvim_tabpage_list_wins(0) > 1
  then
    pcall(vim.api.nvim_win_close, context_win, true)
  end
  state.creation_context_win = nil

  if draft_buf and vim.api.nvim_buf_is_valid(draft_buf) then
    pcall(vim.api.nvim_buf_delete, draft_buf, { force = true })
  end
  if discard_creation and source_buf and vim.api.nvim_buf_is_valid(source_buf) then
    -- The rejected creation buffer is about to be wiped; drop any remembered
    -- reference so context capture doesn't fall back to a deleted buffer.
    if state.source_buf == source_buf then
      state.source_buf = nil
      state.source_cursor = nil
    end
    pcall(vim.api.nvim_buf_delete, source_buf, { force = true })
  end
  if discard_creation then
    state.creation = nil
  end
  state.diff_buf = nil
  state.diff_win = nil
  state.diff_source_buf = nil
  state.diff_source_tick = nil
  state.diff_first_row = nil
  state.diff_cursor = nil
end

function M.send_accept(patch_ids, changed_files)
  local session_id = state.session_id
  local request_id = thinking.start("Continuing", session_id)

  rpc.request("patch/apply_result", {
    session_id = session_id,
    card_id = state.card and state.card.id or "",
    accepted = true,
    patch_ids = patch_ids,
    changed_files = changed_files,
    error = nil,
    context = context.session(),
  }, function(message)
    if not thinking.current(request_id) then
      return
    end

    thinking.stop()

    if message.error then
      log.write("patch apply error", message.error)
      local lines = {
        "Accepted change could not continue",
        tostring(message.error.message),
        "The local accepted source was kept.",
        "",
        "[m] Reply   [q] Quit",
      }
      surfaces.render_agent(lines, {
        view = "error",
        working = false,
        enter = false,
        window = {
          width = 58,
          height = #lines,
          border = require("loopbiotic.config").values.card.border,
          title = " Loopbiotic: Error ",
        },
        bind = function(buf)
          bind(buf, { "m" }, require("loopbiotic").reply_prompt)
          bind(buf, { "q" }, require("loopbiotic").stop)
        end,
      })
      return
    end
    if message.result.session_id ~= state.session_id then
      log.write("stale patch result", message.result)
      return
    end

    -- Accepting a patch now runs a real patch-phase turn on the daemon, which
    -- reports the model it actually ran; adopt it so the displayed model and
    -- cost attribution reflect the accepted turn rather than the previous
    -- (often discovery) turn's model.
    session.apply_turn_result(message.result, {
      update_model = true,
      track_backend_error = true,
    })
  end)
end

-- Error boundary: the draft preview is reached from RPC callbacks and
-- keymaps; a preview bug must log and notify, not kill the session. The
-- guarded wrapper returns nil on error, which callers treat as "not shown".
M.show = util.guard("diff.show", M.show)

return M
