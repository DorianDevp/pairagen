local apply = require("pair.apply")
local context = require("pair.context")
local log = require("pair.log")
local rpc = require("pair.rpc")
local state = require("pair.state")
local thinking = require("pair.thinking")
local ui = require("pair.ui")

local M = {}
local namespace = vim.api.nvim_create_namespace("pair-patch")

function M.show(card)
  local patch = (card.patches or {})[1]

  if not patch then
    ui.notify("Patch card has no local change", vim.log.levels.ERROR)
    return
  end

  local source_buf = state.source_buf or apply.buffer(patch.file)
  if not source_buf or not vim.api.nvim_buf_is_valid(source_buf) then
    ui.notify("Open the proposed location before editing the patch", vim.log.levels.WARN)
    return
  end

  local source_name = vim.fn.fnamemodify(vim.api.nvim_buf_get_name(source_buf), ":p")
  local patch_name = vim.fn.fnamemodify(patch.file, ":p")
  if source_name ~= patch_name then
    ui.notify("Patch target is not the currently accepted source location", vim.log.levels.WARN)
    return
  end

  local source_win = context.buffer_window(source_buf)
  if not source_win then
    ui.notify("Source location is not visible", vim.log.levels.WARN)
    return
  end

  local source_lines = vim.api.nvim_buf_get_lines(source_buf, 0, -1, false)
  local ok, draft_lines = pcall(apply.apply_diff, source_lines, patch.diff)
  if not ok then
    ui.notify(draft_lines, vim.log.levels.ERROR)
    return
  end

  local draft_buf = vim.api.nvim_create_buf(false, true)
  vim.bo[draft_buf].buftype = "nofile"
  vim.bo[draft_buf].bufhidden = "wipe"
  vim.bo[draft_buf].swapfile = false
  vim.bo[draft_buf].modifiable = true
  vim.bo[draft_buf].filetype = vim.bo[source_buf].filetype
  vim.api.nvim_buf_set_name(draft_buf, "Pair draft: " .. patch.file)
  vim.api.nvim_buf_set_lines(draft_buf, 0, -1, false, draft_lines)

  ui.close(state.card_win)
  state.card_win = nil
  vim.api.nvim_win_set_buf(source_win, draft_buf)
  vim.api.nvim_set_current_win(source_win)

  local new_start, new_len = M.new_range(patch.diff)
  local row = math.max(new_start - 1, 0)
  local end_row = math.min(row + math.max(new_len, 1), #draft_lines)
  vim.api.nvim_win_set_cursor(source_win, { math.min(new_start, #draft_lines), 0 })
  vim.api.nvim_buf_set_extmark(draft_buf, namespace, row, 0, {
    end_row = end_row,
    line_hl_group = "DiffChange",
    virt_text = { { " Pair draft", "DiagnosticInfo" } },
    virt_text_pos = "eol",
  })

  state.diff_buf = draft_buf
  state.diff_win = source_win
  state.diff_source_buf = source_buf
  state.diff_source_tick = vim.api.nvim_buf_get_changedtick(source_buf)

  vim.keymap.set("n", "a", M.accept, { buffer = draft_buf, nowait = true, silent = true })
  vim.keymap.set("n", "q", M.reject, { buffer = draft_buf, nowait = true, silent = true })
  vim.keymap.set("n", "r", M.retry, { buffer = draft_buf, nowait = true, silent = true })
end

function M.new_range(diff)
  local new_start, new_len = diff:match("^@@ %-%d+,%d+ %+(%d+),(%d+) @@")

  return tonumber(new_start) or 1, tonumber(new_len) or 1
end

function M.accept()
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
  local card = state.card
  local patch = card and (card.patches or {})[1]

  if not patch then
    return
  end

  M.restore_source()
  M.send(false, { patch.id }, {}, nil)
end

function M.retry()
  M.restore_source()
  require("pair").action("retry")
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

  state.diff_buf = nil
  state.diff_win = nil
  state.diff_source_buf = nil
  state.diff_source_tick = nil
end

function M.send(accepted, patch_ids, changed_files, error)
  local session_id = state.session_id
  local request_id = thinking.start("Applying", session_id)

  rpc.request("patch/apply_result", {
    session_id = session_id,
    card_id = state.card and state.card.id or "",
    accepted = accepted,
    patch_ids = patch_ids,
    changed_files = changed_files,
    error = error,
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

    state.token_usage = message.result.token_usage
    state.turn_token_usage = message.result.turn_token_usage
    require("pair.card").show(message.result.card)
  end)
end

return M
