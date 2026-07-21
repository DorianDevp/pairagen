local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

-- Review and transactional application of agent-proposed file operations
-- (moves/renames). A patch card may carry `file_ops` instead of diff hunks;
-- the operations stay inert behind the same Accept / Reject gate as a patch,
-- and Accept reports through the ordinary patch/apply_result round trip so
-- an authorized Goal continues (e.g. import fix-ups in the next step).

local M = {}

local MAX_OPS = 16

local function absolute(path)
  return (vim.fs.normalize(vim.fn.fnamemodify(path, ":p")):gsub("(.)/+$", "%1"))
end

local function relative(path)
  return vim.fn.fnamemodify(path, ":.")
end

function M.present(card)
  return type(card) == "table" and type(card.file_ops) == "table" and #card.file_ops > 0
end

function M.pending()
  return state.file_ops ~= nil
end

-- Validate a proposed operation set against the live filesystem and editor.
-- Returns an ordered plan with normalized absolute paths, the target parent
-- directories that do not exist yet, and the existing directory that best
-- grounds the review in Netrw. Nothing is created or moved here.
function M.inspect(ops)
  if type(ops) ~= "table" or #ops == 0 then
    return nil, "File operations are missing"
  end
  if #ops > MAX_OPS then
    return nil, "Too many file operations in one proposal"
  end
  local workspace = vim.uv.fs_realpath(vim.fn.getcwd())
  if not workspace then
    return nil, "Workspace root is unavailable"
  end

  local seen = {}
  local missing_seen = {}
  local plan = { ops = {}, missing_directories = {}, context_dir = nil }
  for index, op in ipairs(ops) do
    local kind = op.kind or "move"
    if kind ~= "move" then
      return nil, "Unsupported file operation: " .. tostring(kind)
    end
    if type(op.from) ~= "string" or op.from == "" or type(op.to) ~= "string" or op.to == "" then
      return nil, "File operation is missing a path"
    end
    local from = absolute(op.from)
    local to = absolute(op.to)
    if from == workspace or to == workspace then
      return nil, "File operation targets the workspace root"
    end
    if not util.in_workspace(from, workspace) or not util.in_workspace(to, workspace) then
      return nil, "File operation escapes the workspace"
    end
    if from == to then
      return nil, "Move source and target are the same: " .. relative(from)
    end
    if vim.startswith(to, from .. "/") then
      return nil, "Cannot move " .. relative(from) .. " into itself"
    end
    if not vim.uv.fs_stat(from) then
      return nil, "Move source does not exist: " .. relative(from)
    end
    if vim.uv.fs_stat(to) then
      return nil, "Move target already exists: " .. relative(to)
    end
    if seen[from] or seen[to] then
      return nil, "Duplicate paths across file operations"
    end
    seen[from] = true
    seen[to] = true

    for _, owner in ipairs(M.owning_buffers(from)) do
      if vim.bo[owner].modified then
        return nil, "An unsaved buffer owns " .. relative(vim.api.nvim_buf_get_name(owner))
      end
    end

    local parent = vim.fs.dirname(to)
    local missing = {}
    while parent and not vim.uv.fs_stat(parent) do
      table.insert(missing, 1, parent)
      local next_parent = vim.fs.dirname(parent)
      if next_parent == parent then
        return nil, "Move target has no existing parent"
      end
      parent = next_parent
    end
    local real_parent = parent and vim.uv.fs_realpath(parent)
    if not real_parent or not util.in_workspace(real_parent, workspace) then
      return nil, "Move target parent resolves outside the workspace"
    end
    for _, directory in ipairs(missing) do
      if not missing_seen[directory] then
        missing_seen[directory] = true
        table.insert(plan.missing_directories, directory)
      end
    end

    plan.context_dir = plan.context_dir or real_parent
    table.insert(plan.ops, {
      id = op.id or ("fileop-" .. index),
      kind = kind,
      from = from,
      to = to,
      relative_from = relative(from),
      relative_to = relative(to),
    })
  end

  -- Operations must be independent: a target inside another operation's
  -- source or target cannot be validated as one transaction.
  for i, a in ipairs(plan.ops) do
    for j, b in ipairs(plan.ops) do
      if
        i ~= j
        and (
          vim.startswith(a.to, b.to .. "/")
          or vim.startswith(a.to, b.from .. "/")
          or vim.startswith(a.from, b.from .. "/")
        )
      then
        return nil, "File operations overlap; propose them as separate steps"
      end
    end
  end

  return plan
end

-- Loaded buffers whose file lives at `path` or inside it.
function M.owning_buffers(path)
  local prefix = path .. "/"
  local buffers = {}
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) then
      local name = vim.fs.normalize(vim.api.nvim_buf_get_name(buf))
      if name == path or vim.startswith(name, prefix) then
        table.insert(buffers, buf)
      end
    end
  end
  return buffers
end

-- Apply a reviewed plan. Revalidates freshness first (paths may have changed
-- during review), creates missing target directories, then renames in order.
-- Any failure rolls the moves and created directories back and reports the
-- precise operation that failed; the filesystem is never left half-moved
-- silently.
function M.commit(plan)
  local fresh, reason = M.inspect(plan.ops)
  if not fresh then
    return nil, reason
  end
  for index, op in ipairs(fresh.ops) do
    local reviewed = plan.ops[index]
    if not reviewed or reviewed.from ~= op.from or reviewed.to ~= op.to then
      return nil, "File operations changed during review"
    end
  end

  local created = {}
  local function rollback_directories()
    for index = #created, 1, -1 do
      pcall(vim.fn.delete, created[index], "d")
    end
  end
  for _, directory in ipairs(fresh.missing_directories) do
    if vim.fn.mkdir(directory) == 0 and vim.fn.isdirectory(directory) ~= 1 then
      rollback_directories()
      return nil, "Could not create directory " .. relative(directory)
    end
    table.insert(created, directory)
  end

  local moved = {}
  for _, op in ipairs(fresh.ops) do
    if not vim.uv.fs_rename(op.from, op.to) then
      for index = #moved, 1, -1 do
        pcall(vim.uv.fs_rename, moved[index].to, moved[index].from)
      end
      rollback_directories()
      return nil, "Could not move " .. op.relative_from
    end
    table.insert(moved, op)
  end

  for _, op in ipairs(fresh.ops) do
    M.retarget_buffers(op.from, op.to)
  end

  return fresh
end

-- Point loaded buffers at the moved paths: windows showing an affected buffer
-- swap to a buffer of the new path, then the stale buffer is removed. The
-- buffers were validated unmodified before Accept, so no edits can be lost.
function M.retarget_buffers(from, to)
  local prefix = from .. "/"
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) then
      local name = vim.fs.normalize(vim.api.nvim_buf_get_name(buf))
      local target
      if name == from then
        target = to
      elseif vim.startswith(name, prefix) then
        target = to .. name:sub(#from + 1)
      end
      if target then
        local replacement = vim.fn.bufadd(target)
        for _, win in ipairs(vim.api.nvim_list_wins()) do
          if vim.api.nvim_win_get_buf(win) == buf then
            vim.fn.bufload(replacement)
            vim.api.nvim_win_set_buf(win, replacement)
          end
        end
        if state.source_buf == buf then
          state.source_buf = replacement
        end
        pcall(vim.api.nvim_buf_delete, buf, { force = true })
      end
    end
  end
end

-- Ground the review in the real file tree: reuse a window that already shows
-- a directory listing (the prompt usually came from Netrw), otherwise open
-- the target's existing parent in a split, tracked for cleanup.
function M.open_context(plan)
  if not plan.context_dir then
    return false
  end
  local win
  for _, candidate in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_get_config(candidate).relative == "" then
      local name = vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(candidate))
      if name ~= "" and vim.fn.isdirectory(name) == 1 then
        win = candidate
        break
      end
    end
  end
  if not win then
    local source = require("loopbiotic.navigation").normal_window()
    if not source then
      return false
    end
    vim.api.nvim_set_current_win(source)
    if not pcall(vim.cmd, "split") then
      return false
    end
    win = vim.api.nvim_get_current_win()
    state.creation_context_win = win
  end
  vim.api.nvim_set_current_win(win)
  pcall(vim.cmd, "edit " .. vim.fn.fnameescape(plan.context_dir))
  return true
end

function M.show(card, opts)
  opts = opts or {}
  if (card.patches or {})[1] then
    ui.notify("A proposal cannot mix patches and file operations", vim.log.levels.ERROR)
    return false
  end
  local plan, reason = M.inspect(card.file_ops)
  if not plan then
    require("loopbiotic.log").write("file operations rejected", { error = reason })
    ui.notify(reason, vim.log.levels.ERROR)
    return false
  end
  state.file_ops = plan
  M.open_context(plan)
  M.controls(card, plan, opts)
  return true
end

function M.controls(card, plan, opts)
  opts = opts or {}
  local diff = require("loopbiotic.diff")
  local keys = require("loopbiotic.config").values.keymaps
  local lines = {}
  if state.goal and state.goal.statement then
    table.insert(lines, "Goal  " .. diff.truncate(state.goal.statement, 52))
  end
  for _, op in ipairs(plan.ops) do
    table.insert(lines, "Move  " .. op.relative_from .. " -> " .. op.relative_to)
  end
  for _, directory in ipairs(plan.missing_directories) do
    table.insert(lines, "New   " .. relative(directory) .. "/")
  end
  local explanation = card.explanation or card.title or "File operations"
  table.insert(lines, "")
  table.insert(lines, diff.truncate(explanation, 58))
  table.insert(lines, "")
  table.insert(lines, "No path changes until Accept")
  table.insert(lines, string.format("[%s] Accept   [%s] Reject", keys.draft_accept, keys.draft_reject))

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
      anchor = require("loopbiotic.card").anchor(card),
      anchor_gap = 1,
      avoid_anchor_row = true,
      title = " Loopbiotic: Review ",
    },
    bind = function(buf)
      for index, line in ipairs(lines) do
        local group = line:match("^Goal") and "LoopbioticGoal"
          or line:match("^%[") and "LoopbioticAction"
          or (line:match("^Move") or line:match("^New")) and "LoopbioticTitle"
        if group then
          vim.api.nvim_buf_add_highlight(buf, -1, group, index - 1, 0, -1)
        end
      end
      M.bind_review(buf)
    end,
  })
  -- Accept/Reject also work from the focused directory listing, mirroring
  -- how a patch draft binds them on the draft buffer.
  local context_buf = vim.api.nvim_get_current_buf()
  if vim.fn.isdirectory(vim.api.nvim_buf_get_name(context_buf)) == 1 then
    M.bind_review(context_buf)
  end
end

function M.bind_review(buf)
  local keys = require("loopbiotic.config").values.keymaps
  for _, entry in ipairs({ { keys.draft_accept, M.accept }, { keys.draft_reject, M.reject } }) do
    if entry[1] and entry[1] ~= "" then
      vim.keymap.set("n", entry[1], entry[2], { buffer = buf, nowait = true, silent = true })
    end
  end
end

function M.accept()
  if not require("loopbiotic.scope").allows("accept") then
    return
  end
  local card = state.card
  local plan = state.file_ops
  if not plan or not M.present(card) then
    ui.notify("Reviewed file operations are unavailable", vim.log.levels.ERROR)
    return
  end

  local fresh, reason = M.commit(plan)
  if not fresh then
    ui.notify(reason, vim.log.levels.ERROR)
    return
  end
  M.clear()

  local ids = {}
  local changed = {}
  for _, op in ipairs(fresh.ops) do
    table.insert(ids, op.id)
    table.insert(changed, op.relative_from)
    table.insert(changed, op.relative_to)
  end
  require("loopbiotic.diff").send_accept(ids, changed)
end

function M.reject()
  if not require("loopbiotic.scope").allows("reject") then
    return
  end
  local card = state.card
  if not M.present(card) then
    return
  end
  M.clear()
  if state.goal then
    state.goal.status = "paused"
    state.goal.next_step = nil
  end

  local ids = {}
  for index, op in ipairs(card.file_ops) do
    table.insert(ids, op.id or ("fileop-" .. index))
  end
  local diff = require("loopbiotic.diff")
  diff.show_paused("No files were moved. The Goal is paused.")
  diff.acknowledge_rejection(ids)
  require("loopbiotic.prompt").reply()
end

-- Drop the pending plan and close the tracked context split. Safe to call
-- when nothing is pending (Stop, Reset, a superseding card).
function M.clear()
  state.file_ops = nil
  local win = state.creation_context_win
  if win and vim.api.nvim_win_is_valid(win) and #vim.api.nvim_tabpage_list_wins(0) > 1 then
    pcall(vim.api.nvim_win_close, win, true)
  end
  state.creation_context_win = nil
end

-- Error boundary: review rendering is reached from RPC callbacks; a bug must
-- log and notify, not kill the session (mirrors diff.show).
M.show = util.guard("fileops.show", M.show)

return M
