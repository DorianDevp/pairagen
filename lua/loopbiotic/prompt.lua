local config = require("loopbiotic.config")
local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
local ui = require("loopbiotic.ui")

local M = {}

-- Which window kind ("Prompt"/"Reply") is currently open, so the async
-- warmup response can re-render the matching frame title. open_footer keeps
-- the default footer so a cleared preflight error can restore it.
local open_kind = "Prompt"
local open_footer = nil
local open_source = nil
local open_graph = nil
local open_mode = "investigate"
local submit_token = 0

local mode_labels = {
  fix = "Fix — prepare a reviewed patch",
  explain = "Explain — explain without patching",
  investigate = "Investigate — form a grounded hypothesis",
  review = "Review — review the selected code",
  propose = "Propose — propose a reviewed patch",
}

function M.normalize_mode(mode)
  if config.valid_mode(mode) then
    return mode
  end
  error("Unknown Loopbiotic mode: " .. tostring(mode))
end

function M.mode_candidates()
  return config.mode_names()
end

function M.current_mode()
  return open_mode
end

local function flow_listener(graph)
  if surfaces.prompt_open() and open_source then
    open_source.value.call_hierarchy = require("loopbiotic.flow").bundle(graph)
  end
end

local function resolve_hints(source)
  source.lsp_pending = true
  require("loopbiotic.context").lsp_hints_async(
    source.buf,
    { source.value.cursor.line, math.max(source.value.cursor.column - 1, 0) },
    source.value.cwd,
    function(hints)
      source.value.hints = hints
      source.lsp_pending = false
    end
  )
end

function M.open(mode)
  open_mode = M.normalize_mode(mode or state.prompt_stash_mode or config.values.backend.mode)
  local source = require("loopbiotic.context").capture(nil, { skip_lsp = true })
  open_source = source

  open_kind = "Prompt"
  M.open_for({
    title = M.title("Prompt", open_mode),
    footer = " Ctrl-k mode  Ctrl-l model  Ctrl-s submit  Esc normal  q close ",
    return_to_agent = state.session_id ~= nil,
    submit = function(text, selected_mode)
      require("loopbiotic").submit_prompt(text, selected_mode, open_source)
    end,
  })

  -- Open the editor workspace before starting any process or LSP work. The
  -- backend can then pay its startup cost while the user is already typing;
  -- its response also supplies the concrete model used by the title/picker.
  require("loopbiotic.rpc").request("backend/warmup", {}, M.on_warmup)
  resolve_hints(source)

  if (config.values.flow or {}).enabled ~= false then
    open_graph = require("loopbiotic.flow").start(source.buf, {
      source.value.cursor.line,
      math.max(source.value.cursor.column - 1, 0),
    }, flow_listener)
    source.value.call_hierarchy = require("loopbiotic.flow").bundle(open_graph)
  else
    open_graph = nil
  end

  M.prefill()
  M.refresh_footer()
end

function M.reply(mode)
  open_mode = M.normalize_mode(mode or state.session_mode or config.values.backend.mode)
  open_source = nil
  open_graph = nil
  open_kind = "Reply"
  M.open_for({
    title = M.title("Reply", open_mode),
    footer = " Ctrl-k mode  Ctrl-l model  Ctrl-s send  Esc normal  q close ",
    return_to_agent = true,
    submit = function(text, selected_mode)
      require("loopbiotic").submit_reply(text, selected_mode)
    end,
  })
  M.prefill()
  M.refresh_footer()
end

-- Store the identity reported by backend/warmup and refresh the open prompt
-- title with it. Old daemons answer {ok = true} without an identity field;
-- tolerate that by keeping the previous state. A warmup error is surfaced
-- immediately in the open prompt window's footer (and one WARN notification)
-- so the user learns the backend is broken before composing a full prompt.
---@param message table RPC response ({ result = ... } or { error = ... })
function M.on_warmup(message)
  if message.error then
    local error_message = tostring(type(message.error) == "table" and message.error.message or message.error)

    if state.backend_preflight_error ~= error_message then
      state.backend_preflight_error = error_message
      ui.notify(
        "Loopbiotic backend not ready: " .. error_message .. " — see :checkhealth loopbiotic",
        vim.log.levels.WARN
      )
    end
    M.refresh_footer()

    return
  end

  if type(message.result) ~= "table" then
    return
  end

  if state.backend_preflight_error then
    state.backend_preflight_error = nil
    M.refresh_footer()
  end

  local identity = message.result.identity
  if type(identity) ~= "table" then
    return
  end

  state.agent_identity = identity
  M.refresh_title()
end

-- Re-render the frame title of the currently open prompt window, if any.
-- Callers may run outside the main loop (RPC callbacks), hence the schedule.
function M.refresh_title()
  vim.schedule(function()
    surfaces.update_prompt_frame({ title = M.title(open_kind), title_pos = "left" })
  end)
end

-- Re-render the frame footer of the currently open prompt window, if any:
-- the preflight-error footer while a warmup failure is stored, otherwise the
-- default keymap hints. Mirrors refresh_title (schedule + validity check).
function M.refresh_footer()
  vim.schedule(function()
    local footer = open_footer
    if type(state.backend_preflight_error) == "string" and state.backend_preflight_error ~= "" then
      footer = M.preflight_footer(state.backend_preflight_error)
    end

    local context_summary = require("loopbiotic.widgets").summary()
    if context_summary then
      footer = " " .. context_summary .. " · Ctrl-x remove   " .. (footer or "")
    end

    surfaces.update_prompt_frame({ footer = footer, footer_pos = "right" })
  end)
end

-- Footer line shown while the backend fails its warmup preflight.
---@param error_message string
---@return string
function M.preflight_footer(error_message)
  return " backend not ready: " .. M.short_error(error_message) .. " — :checkhealth loopbiotic "
end

-- One-line, footer-sized rendering of a backend error message.
---@param error_message string
---@return string
function M.short_error(error_message)
  local text = tostring(error_message or ""):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
  if #text > 60 then
    text = text:sub(1, 57) .. "..."
  end

  return text
end

-- Pre-fill the freshly opened prompt buffer with text stashed by a failed
-- session start, cursor at the end, so the composed prompt is not lost.
function M.prefill()
  local stash = state.prompt_stash
  local buf, win = surfaces.prompt_handles()
  if type(stash) ~= "string" or stash == "" or not (buf and vim.api.nvim_buf_is_valid(buf)) then
    return
  end

  local lines = vim.split(stash, "\n", { plain = true })
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)

  if win and vim.api.nvim_win_is_valid(win) then
    pcall(vim.api.nvim_win_set_cursor, win, { #lines, #lines[#lines] })
    if vim.api.nvim_get_current_win() == win then
      -- Append after the restored text instead of before its last character.
      vim.cmd("startinsert!")
    end
  end
end

-- Pure state transition for the prompt stash: submitting stashes the
-- composed text (the window is closed before the backend answers), a
-- successful start clears it, and a failed start keeps it for the next
-- prompt.open to pre-fill.
---@param stash string|nil current stash
---@param event "submit"|"start_ok"|"start_error"
---@param text string|nil submitted text (only used for "submit")
---@return string|nil next stash
function M.next_stash(stash, event, text)
  if event == "submit" then
    return text
  end
  if event == "start_ok" then
    return nil
  end

  return stash
end

-- Pick the concrete model out of the fixed resolution order: the model the
-- backend reported it actually ran this turn, then the user's configured
-- pick, then the model the warmup identity announced. Returns nil when none
-- is known. vim.NIL (JSON null) and empty strings count as unknown.
---@param configured string|nil
---@param identity_model string|nil
---@param backend_model string|nil
---@return string|nil
function M.resolved_model(configured, identity_model, backend_model)
  -- Actual-used first: once a turn has run, the backend-reported model is the
  -- honest answer for the headline (a discovery turn ran discovery_model, a
  -- patch turn ran the patch model). Before any turn, fall back to the user's
  -- configured pick, then the backend's advertised default.
  local candidates = { backend_model, configured, identity_model }

  for index = 1, 3 do
    local value = candidates[index]
    if type(value) == "string" and value ~= "" then
      return value
    end
  end

  return nil
end

-- Title-ready model name; "model?" until any concrete model is known. The
-- word "default" is never rendered. The label always reflects the actual
-- per-turn model (a discovery turn shows discovery_model, a patch turn shows
-- the patch model) via resolved_model, so no separate discovery suffix is
-- needed — the headline is never a model the turn did not run.
---@param configured string|nil
---@param identity table|nil backend/warmup identity ({ model, models, phases })
---@param backend_model string|nil
---@return string
function M.model_label(configured, identity, backend_model)
  local identity_model = type(identity) == "table" and identity.model or nil
  return M.resolved_model(configured, identity_model, backend_model) or "model?"
end

-- Deduped model-picker candidates, in resolution-priority order: configured
-- patch model, backend default/patch model, backend-enumerated selectable
-- models, the agent's `models` config list, and the last reported model.
---@param configured string|nil
---@param identity table|nil backend/warmup identity ({ model, models })
---@param agent_models string[]|nil the agent's `models` config list
---@param backend_model string|nil
---@return string[]
function M.model_candidates(configured, identity, agent_models, backend_model)
  local seen = {}
  local candidates = {}
  local function add(value)
    if type(value) == "string" and value ~= "" and not seen[value] then
      seen[value] = true
      table.insert(candidates, value)
    end
  end

  add(configured)
  if type(identity) == "table" then
    add(identity.model)
    if type(identity.phases) == "table" then
      add(identity.phases.patch)
      -- Discovery runs a distinct (often cheaper) model; keep it selectable so
      -- the user can control which model answers investigate/explain/review
      -- turns, not only patch turns.
      add(identity.phases.discovery)
    end
    if type(identity.models) == "table" then
      for _, name in ipairs(identity.models) do
        add(name)
      end
    end
  end
  for _, name in ipairs(agent_models or {}) do
    add(name)
  end
  add(backend_model)

  return candidates
end

function M.title(kind, mode)
  local agent = config.agent()
  local model = M.model_label(config.model(), state.agent_identity, state.backend_model)

  return string.format(" Loopbiotic %s · %s · %s / %s ", kind, M.normalize_mode(mode or open_mode), agent, model)
end

-- Choose the behavior contract for this PromptWindow. The picker is local UI:
-- it preserves typed text and does not contact the backend until submit.
function M.pick_mode()
  vim.ui.select(M.mode_candidates(), {
    prompt = "Loopbiotic mode",
    format_item = function(mode)
      return mode_labels[mode] or mode
    end,
  }, function(choice)
    if not choice then
      return
    end
    open_mode = M.normalize_mode(choice)
    M.refresh_title()
  end)
end

-- Open a picker over every model known for the active agent. The choice
-- goes through the regular model-switch entry point (persisting the
-- per-agent preference); only the frame title changes, the typed prompt
-- text and window stay as they are.
-- fix/propose draft a patch; explain/investigate/review run discovery. The
-- visible PromptWindow mode therefore names the phase the model picker targets.
function M.model_phase(mode)
  if mode == "fix" or mode == "propose" then
    return "patch"
  end
  return "discovery"
end

function M.pick_model()
  local agent = config.agent()
  local identity = state.agent_identity
  local phase = M.model_phase(M.current_mode())
  local candidates = M.model_candidates(config.model(), identity, config.model_names(), state.backend_model)

  if #candidates == 0 then
    ui.notify("No known models for " .. agent .. " — use :LoopbioticModel <name>", vim.log.levels.WARN)
    return
  end

  local label = phase == "patch" and "patch model" or "discovery model"
  vim.ui.select(candidates, { prompt = "Loopbiotic " .. label .. " (" .. agent .. ")" }, function(choice)
    if not choice or choice == "" then
      return
    end

    if phase == "patch" then
      require("loopbiotic").model(choice)
    else
      require("loopbiotic").discovery_model(choice)
    end
    M.refresh_title()
  end)
end

function M.open_for(opts)
  M.close()

  open_footer = opts.footer
  local size = M.size()
  local position = M.position(size)
  local row = position.row
  local col = position.col
  local buf, win = surfaces.open_prompt({
    row = row,
    col = col,
    outer_width = size.outer_width,
    outer_height = size.outer_height,
    inner_width = size.inner_width,
    inner_height = size.inner_height,
    padding_x = size.padding_x,
    padding_y = size.padding_y,
    border = config.values.prompt.border,
    title = opts.title,
    footer = opts.footer,
    return_to_agent = opts.return_to_agent == true,
  })
  M.prepare(buf, win)
  M.bind(buf, opts.submit)

  vim.cmd("startinsert")
end

function M.prepare(buf, win)
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "markdown"
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true
  vim.wo[win].cursorline = true
  vim.wo[win].number = false
  vim.wo[win].relativenumber = false
  vim.wo[win].signcolumn = "no"
end

function M.bind(buf, submit)
  vim.keymap.set({ "i", "n" }, "<C-s>", function()
    M.submit(buf, submit)
  end, { buffer = buf, nowait = true, silent = true })

  local models_key = config.values.keymaps.models
  if models_key and models_key ~= "" then
    vim.keymap.set({ "i", "n" }, models_key, function()
      M.pick_model()
    end, { buffer = buf, nowait = true, silent = true })
  end

  local modes_key = config.values.keymaps.modes
  if modes_key and modes_key ~= "" then
    vim.keymap.set({ "i", "n" }, modes_key, function()
      M.pick_mode()
    end, { buffer = buf, nowait = true, silent = true })
  end

  vim.keymap.set("n", "<CR>", function()
    M.submit(buf, submit)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "q", function()
    M.close()
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set({ "i", "n" }, "<C-x>", function()
    M.remove_context()
  end, { buffer = buf, nowait = true, silent = true })

end

function M.remove_context()
  local widgets = require("loopbiotic.widgets")
  local refs = widgets.list()
  if #refs == 0 then
    return
  end
  vim.ui.select(refs, {
    prompt = "Remove attached context",
    format_item = function(ref)
      return ref.label .. " · " .. vim.fn.fnamemodify(ref.file, ":.")
    end,
  }, function(ref)
    if ref then
      widgets.deselect(ref.id)
      M.refresh_footer()
    end
  end)
end

function M.submit(buf, submit)
  local text = M.text(buf)
  local selected_mode = open_mode

  if text == "" then
    return
  end

  -- PromptWindow may open as soon as a running turn is invalidated locally.
  -- Keep the composed request in place until the daemon confirms cancellation,
  -- so two turns can never overlap in one session.
  if state.turn_barrier then
    return
  end

  if vim.fn.mode():match("^[iR]") then
    vim.cmd("stopinsert")
  end

  -- The window closes before the backend answers, so stash the composed text
  -- now; a successful start clears it, a failed one leaves it for prefill.
  state.prompt_stash = M.next_stash(state.prompt_stash, "submit", text)
  state.prompt_stash_mode = selected_mode
  submit_token = submit_token + 1
  local token = submit_token
  vim.b[buf].loopbiotic_submitting = true
  local graph = open_graph
  local source = open_source
  require("loopbiotic.flow").await(graph, (config.values.flow or {}).submit_wait_ms or 160, function(bundle)
    if token ~= submit_token then
      return
    end
    if source then
      source.value.call_hierarchy = bundle
    end
    if graph then
      state.call_hierarchy = graph
    end
    M.close(true)
    submit(text, selected_mode)
  end, function()
    return not source or source.lsp_pending ~= true
  end)
end

function M.close(preserve_submit)
  if not preserve_submit then
    submit_token = submit_token + 1
  end
  local graph = open_graph
  if not preserve_submit and graph and graph ~= state.call_hierarchy then
    require("loopbiotic.flow").set_listener(graph, nil)
  end
  open_graph = nil
  surfaces.close_prompt({ focus_agent = preserve_submit ~= true })
end

function M.text(buf)
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)

  return vim.trim(table.concat(lines, "\n"))
end

function M.size()
  local viewport = ui.viewport()
  local outer_width = M.width()
  local outer_height = math.min(config.values.prompt.height, math.max(viewport.height - 2, 1))
  local padding_x = math.min(config.values.prompt.padding_x, math.floor((outer_width - 1) / 2))
  local padding_y = math.min(config.values.prompt.padding_y, math.floor((outer_height - 1) / 2))
  local inner_width = math.max(outer_width - padding_x * 2, 1)
  local inner_height = math.max(outer_height - padding_y * 2, 1)

  return {
    outer_width = outer_width,
    outer_height = outer_height,
    inner_width = inner_width,
    inner_height = inner_height,
    padding_x = padding_x,
    padding_y = padding_y,
  }
end

function M.position(size)
  local viewport = ui.viewport()
  local cursor = M.cursor_screen_position()
  local total_width = size.outer_width + 2
  local total_height = size.outer_height + 2
  local max_row = math.max(viewport.height - total_height, 0)
  local max_col = math.max(viewport.width - total_width, 0)
  local below = cursor.row + 1
  local above = cursor.row - total_height
  local row

  if below <= max_row then
    row = below
  elseif above >= 0 then
    row = above
  else
    row = ui.clamp(below, 0, max_row)
  end

  return {
    row = ui.clamp(row, 0, max_row),
    col = ui.clamp(cursor.col - math.floor(total_width / 2), 0, max_col),
  }
end

function M.cursor_screen_position()
  local win = vim.api.nvim_get_current_win()
  local cursor = vim.api.nvim_win_get_cursor(win)
  local position = vim.fn.screenpos(win, cursor[1], cursor[2] + 1)

  if position.row == 0 or position.col == 0 then
    local viewport = ui.viewport()
    return {
      row = math.floor(viewport.height / 2),
      col = math.floor(viewport.width / 2),
    }
  end

  return {
    row = position.row - 1,
    col = position.col - 1,
  }
end

function M.width()
  local configured = config.values.prompt.width or 96
  local limit = math.max(ui.viewport().width - 2, 1)

  return math.min(configured, limit)
end

function M.relayout()
  if not surfaces.prompt_open() then
    return
  end
  local size = M.size()
  local viewport = ui.viewport()
  local _, _, _, frame = surfaces.prompt_handles()
  local frame_config = vim.api.nvim_win_get_config(frame)
  local row = ui.clamp(ui.number(frame_config.row) or 0, 0, math.max(viewport.height - size.outer_height - 2, 0))
  local col = ui.clamp(ui.number(frame_config.col) or 0, 0, math.max(viewport.width - size.outer_width - 2, 0))
  surfaces.relayout_prompt({
    row = row,
    col = col,
    outer_width = size.outer_width,
    outer_height = size.outer_height,
    inner_width = size.inner_width,
    inner_height = size.inner_height,
    padding_x = size.padding_x,
    padding_y = size.padding_y,
  })
end

return M
