local config = require("loopbiotic.config")
local state = require("loopbiotic.state")
local ui = require("loopbiotic.ui")

local M = {}

-- Which window kind ("Prompt"/"Reply") is currently open, so the async
-- warmup response can re-render the matching frame title.
local open_kind = "Prompt"

function M.open(mode)
  local source = require("loopbiotic.context").capture()

  -- Let the backend pay its startup cost (CLI boot, process spawn) while the
  -- user is still typing the prompt. The response also carries the backend
  -- identity (concrete model, known models) used for the title and picker.
  require("loopbiotic.rpc").request("backend/warmup", {}, M.on_warmup)

  open_kind = "Prompt"
  M.open_for({
    title = M.title("Prompt"),
    footer = " Ctrl-l model  /kind forces card type  Ctrl-s submit  Esc normal  q close ",
    submit = function(text)
      require("loopbiotic").start(text, mode, source)
    end,
  })
end

function M.reply()
  open_kind = "Reply"
  M.open_for({
    title = M.title("Reply"),
    footer = " Ctrl-l model  Ctrl-s send  Esc normal  q close ",
    submit = function(text)
      require("loopbiotic").reply(text)
    end,
  })
end

-- Store the identity reported by backend/warmup and refresh the open prompt
-- title with it. Old daemons answer {ok = true} without an identity field;
-- tolerate that (and error responses) by keeping the previous state.
---@param message table RPC response ({ result = ... } or { error = ... })
function M.on_warmup(message)
  if message.error or type(message.result) ~= "table" then
    return
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
    local frame_win = state.prompt_frame_win
    if not (frame_win and vim.api.nvim_win_is_valid(frame_win)) then
      return
    end

    pcall(vim.api.nvim_win_set_config, frame_win, { title = M.title(open_kind), title_pos = "left" })
  end)
end

-- Pick the concrete model out of the fixed resolution order: configured
-- model, then the model the warmup identity announced for the next turn,
-- then the model the backend reported after a turn. Returns nil when none
-- is known. vim.NIL (JSON null) and empty strings count as unknown.
---@param configured string|nil
---@param identity_model string|nil
---@param backend_model string|nil
---@return string|nil
function M.resolved_model(configured, identity_model, backend_model)
  local candidates = { configured, identity_model, backend_model }

  for index = 1, 3 do
    local value = candidates[index]
    if type(value) == "string" and value ~= "" then
      return value
    end
  end

  return nil
end

-- Title-ready model name; "model?" until any concrete model is known. The
-- word "default" is never rendered. When the backend runs a different
-- discovery model (identity.phases), it is shown alongside the patch model
-- instead of being presented as "the" model.
---@param configured string|nil
---@param identity table|nil backend/warmup identity ({ model, models, phases })
---@param backend_model string|nil
---@return string
function M.model_label(configured, identity, backend_model)
  local identity_model = type(identity) == "table" and identity.model or nil
  local label = M.resolved_model(configured, identity_model, backend_model) or "model?"
  local phases = type(identity) == "table" and type(identity.phases) == "table" and identity.phases or nil
  local discovery = phases and phases.discovery

  if type(discovery) == "string" and discovery ~= "" and discovery ~= label then
    return label .. " · discovery " .. discovery
  end

  return label
end

-- Deduped model-picker candidates, in resolution-priority order: configured
-- model, identity model, backend-enumerated models, the agent's `models`
-- config list, the model reported after the last turn.
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

function M.title(kind)
  local agent = config.agent()
  local model = M.model_label(config.model(), state.agent_identity, state.backend_model)

  return string.format(" Loopbiotic %s · %s / %s ", kind, agent, model)
end

-- Open a picker over every model known for the active agent. The choice
-- goes through the regular model-switch entry point (persisting the
-- per-agent preference); only the frame title changes, the typed prompt
-- text and window stay as they are.
function M.pick_model()
  local agent = config.agent()
  local identity = state.agent_identity
  local candidates = M.model_candidates(config.model(), identity, config.model_names(), state.backend_model)

  if #candidates == 0 then
    ui.notify("No known models for " .. agent .. " — use :LoopbioticModel <name>", vim.log.levels.WARN)
    return
  end

  vim.ui.select(candidates, { prompt = "Loopbiotic model (" .. agent .. ")" }, function(choice)
    if not choice or choice == "" then
      return
    end

    require("loopbiotic").model(choice)
    M.refresh_title()
  end)
end

function M.open_for(opts)
  M.close()

  local size = M.size()
  local position = M.position(size)
  local row = position.row
  local col = position.col
  local frame_buf = vim.api.nvim_create_buf(false, true)
  local zindex = config.values.prompt.zindex or 200
  local frame_win = vim.api.nvim_open_win(frame_buf, false, {
    relative = "editor",
    row = row,
    col = col,
    width = size.outer_width,
    height = size.outer_height,
    style = "minimal",
    border = config.values.prompt.border,
    title = opts.title,
    title_pos = "left",
    footer = opts.footer,
    footer_pos = "right",
    zindex = zindex,
  })

  state.prompt_frame_buf = frame_buf
  state.prompt_frame_win = frame_win

  vim.bo[frame_buf].bufhidden = "wipe"
  vim.bo[frame_buf].modifiable = false

  local buf = vim.api.nvim_create_buf(false, true)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    row = row + size.padding_y,
    col = col + size.padding_x,
    width = size.inner_width,
    height = size.inner_height,
    style = "minimal",
    border = "none",
    zindex = zindex + 1,
  })

  state.prompt_buf = buf
  state.prompt_win = win

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

  vim.keymap.set("n", "<CR>", function()
    M.submit(buf, submit)
  end, { buffer = buf, nowait = true, silent = true })

  vim.keymap.set("n", "q", function()
    M.close()
  end, { buffer = buf, nowait = true, silent = true })
end

function M.submit(buf, submit)
  local text = M.text(buf)

  if text == "" then
    return
  end

  if vim.fn.mode():match("^[iR]") then
    vim.cmd("stopinsert")
  end

  M.close()
  submit(text)
end

function M.close()
  ui.close(state.prompt_win)
  ui.close(state.prompt_frame_win)

  state.prompt_win = nil
  state.prompt_buf = nil
  state.prompt_frame_win = nil
  state.prompt_frame_buf = nil
end

function M.text(buf)
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)

  return vim.trim(table.concat(lines, "\n"))
end

function M.size()
  local outer_width = M.width()
  local viewport = ui.viewport()
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

return M
