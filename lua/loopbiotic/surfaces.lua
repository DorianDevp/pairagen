local config = require("loopbiotic.config")
local state = require("loopbiotic.state")
local ui = require("loopbiotic.ui")

local M = {}

local function valid_win(win)
  return win ~= nil and vim.api.nvim_win_is_valid(win)
end

local function valid_buf(buf)
  return buf ~= nil and vim.api.nvim_buf_is_valid(buf)
end

local function current_tab()
  return vim.api.nvim_get_current_tabpage()
end

local function prompt_state()
  return state.surfaces.prompt
end

local function agent_state()
  return state.surfaces.agent
end

local function close_handle(win)
  if valid_win(win) then
    ui.close(win)
  end
end

local function configure_content_window(win)
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true
  vim.wo[win].number = false
  vim.wo[win].relativenumber = false
  vim.wo[win].signcolumn = "no"
end

function M.setup()
  local group = vim.api.nvim_create_augroup("LoopbioticSurfaces", { clear = true })
  vim.api.nvim_create_autocmd("TabEnter", {
    group = group,
    callback = function()
      vim.schedule(function()
        ui.cleanup_deferred()
        local agent = agent_state()
        if agent.owner_tab == current_tab() and agent.mode ~= "closed" then
          M.refresh_agent({ enter = false })
        end
      end)
    end,
  })
  vim.api.nvim_create_autocmd("VimResized", {
    group = group,
    callback = function()
      vim.schedule(function()
        if M.prompt_open() then
          require("loopbiotic.prompt").relayout()
        end
        local agent = agent_state()
        if agent.owner_tab == current_tab() and agent.mode ~= "closed" then
          M.refresh_agent({ enter = false })
        end
      end)
    end,
  })
end

function M.open_prompt(spec)
  M.close_prompt({ focus_agent = false })

  local prompt = prompt_state()
  local frame_buf = vim.api.nvim_create_buf(false, true)
  local zindex = config.values.prompt.zindex or 200
  local frame_win = vim.api.nvim_open_win(frame_buf, false, {
    relative = "editor",
    row = spec.row,
    col = spec.col,
    width = spec.outer_width,
    height = spec.outer_height,
    style = "minimal",
    border = spec.border or config.values.prompt.border,
    title = spec.title,
    title_pos = "left",
    footer = spec.footer,
    footer_pos = "right",
    zindex = zindex,
  })
  vim.bo[frame_buf].bufhidden = "wipe"
  vim.bo[frame_buf].modifiable = false

  local buf = vim.api.nvim_create_buf(false, true)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    row = spec.row + spec.padding_y,
    col = spec.col + spec.padding_x,
    width = spec.inner_width,
    height = spec.inner_height,
    style = "minimal",
    border = "none",
    zindex = zindex + 1,
  })

  prompt.frame_buf = frame_buf
  prompt.frame_win = frame_win
  prompt.buf = buf
  prompt.win = win
  prompt.mode = "open"
  prompt.spec = vim.deepcopy(spec)
  prompt.return_to_agent = spec.return_to_agent == true

  configure_content_window(win)
  return buf, win
end

function M.open_prompt_picker(lines, spec)
  spec = spec or {}
  M.close_prompt_picker({ focus_prompt = false })
  local prompt = prompt_state()
  if not M.prompt_open() or not prompt.spec then
    return nil, nil
  end

  local maximum = tonumber((config.values.skills or {}).picker_height) or 10
  local prompt_row = tonumber(prompt.spec.row) or 0
  local available_above = math.max(math.floor(prompt_row) - 2, 1)
  local height = math.max(math.min(#lines, maximum, available_above), 1)
  local width = math.max(1, math.min(prompt.spec.outer_width, ui.viewport().width - 2))
  local row = math.max(prompt_row - height - 2, 0)
  local buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    row = row,
    col = prompt.spec.col,
    width = width,
    height = height,
    style = "minimal",
    border = config.values.prompt.border,
    title = spec.title,
    title_pos = "left",
    footer = spec.footer,
    footer_pos = "right",
    zindex = (config.values.prompt.zindex or 200) + 2,
  })
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = "loopbiotic-skills"
  configure_content_window(win)
  prompt.picker_buf = buf
  prompt.picker_win = win
  return buf, win
end

function M.close_prompt_picker(opts)
  opts = opts or {}
  local prompt = prompt_state()
  close_handle(prompt.picker_win)
  prompt.picker_win = nil
  prompt.picker_buf = nil
  if opts.focus_prompt and valid_win(prompt.win) then
    ui.focus(prompt.win)
  end
end

function M.prompt_open()
  local prompt = prompt_state()
  return prompt.mode == "open" and valid_win(prompt.win) and valid_win(prompt.frame_win)
end

function M.prompt_handles()
  local prompt = prompt_state()
  return prompt.buf, prompt.win, prompt.frame_buf, prompt.frame_win
end

function M.update_prompt_frame(values)
  local prompt = prompt_state()
  if not valid_win(prompt.frame_win) then
    return false
  end
  return pcall(vim.api.nvim_win_set_config, prompt.frame_win, values)
end

function M.relayout_prompt(spec)
  local prompt = prompt_state()
  if not M.prompt_open() then
    return false
  end
  M.close_prompt_picker({ focus_prompt = false })
  spec = spec or prompt.spec
  if not spec then
    return false
  end
  prompt.spec = vim.deepcopy(spec)
  local frame_ok = pcall(vim.api.nvim_win_set_config, prompt.frame_win, {
    relative = "editor",
    row = spec.row,
    col = spec.col,
    width = spec.outer_width,
    height = spec.outer_height,
  })
  local content_ok = pcall(vim.api.nvim_win_set_config, prompt.win, {
    relative = "editor",
    row = spec.row + spec.padding_y,
    col = spec.col + spec.padding_x,
    width = spec.inner_width,
    height = spec.inner_height,
  })
  return frame_ok and content_ok
end

function M.close_prompt(opts)
  opts = opts or {}
  local prompt = prompt_state()
  local focus_agent = opts.focus_agent
  if focus_agent == nil then
    focus_agent = prompt.return_to_agent
  end

  M.close_prompt_picker({ focus_prompt = false })

  close_handle(prompt.win)
  close_handle(prompt.frame_win)
  prompt.win = nil
  prompt.buf = nil
  prompt.frame_win = nil
  prompt.frame_buf = nil
  prompt.mode = "closed"
  prompt.spec = nil
  prompt.return_to_agent = false

  if focus_agent then
    M.resume_agent()
  end
end

function M.claim_agent(tab)
  local agent = agent_state()
  -- Ownership is chosen by the first View in a session. Async updates arriving
  -- while the user is in another tab must update retained content, never move
  -- the singleton to that tab.
  tab = agent.owner_tab or tab or current_tab()
  agent.owner_tab = tab
  if agent.mode == "closed" then
    agent.mode = "visible"
  end
  return agent
end

local function wrapped_lines()
  local agent = agent_state()
  local label = agent.working and "working" or agent.view or "ready"
  return { string.format("Loopbiotic · %s  %s show", label, config.values.keymaps.resume) }
end

local function wrapped_opts()
  local viewport = ui.viewport()
  local lines = wrapped_lines()
  local width = math.min(math.max(vim.fn.strdisplaywidth(lines[1]) + 2, 24), math.max(viewport.width - 2, 1))
  return {
    width = width,
    height = 1,
    row = 1,
    col = math.max(viewport.width - width - 2, 0),
    border = config.values.card.border,
    title = " Loopbiotic ",
    enter = false,
  }
end

local function render_agent_now(opts)
  opts = opts or {}
  local agent = agent_state()
  if agent.owner_tab ~= current_tab() or agent.mode == "closed" then
    return agent.buf, agent.win
  end

  local lines
  local render_opts
  if agent.mode == "wrapped" then
    lines = wrapped_lines()
    render_opts = wrapped_opts()
  else
    lines = agent.lines or { "" }
    render_opts = vim.deepcopy(agent.opts or {})
    render_opts.enter = opts.enter == true
  end

  local buf, win = ui.render_frame(agent.buf, agent.win, lines, render_opts)
  agent.buf = buf
  agent.win = win
  configure_content_window(win)
  vim.wo[win].wrap = agent.mode == "wrapped" or agent.wrap ~= false
  vim.wo[win].cursorline = agent.mode == "visible" and agent.cursorline == true

  if agent.mode == "visible" and type(agent.bind) == "function" then
    agent.bind(buf, win)
  end
  return buf, win
end

function M.render_agent(lines, opts)
  opts = opts or {}
  local agent = M.claim_agent(opts.owner_tab)
  agent.lines = vim.deepcopy(lines or { "" })
  agent.opts = vim.deepcopy(opts.window or opts)
  agent.opts.owner_tab = nil
  agent.opts.window = nil
  agent.opts.view = nil
  agent.opts.bind = nil
  agent.opts.wrap = nil
  agent.opts.cursorline = nil
  agent.view = opts.view or agent.view or "response"
  agent.bind = opts.bind
  agent.wrap = opts.wrap
  agent.cursorline = opts.cursorline == true
  agent.working = opts.working == true
  return render_agent_now({ enter = opts.enter == true })
end

function M.refresh_agent(opts)
  return render_agent_now(opts)
end

function M.wrap_agent()
  local agent = agent_state()
  if agent.mode ~= "visible" or agent.owner_tab ~= current_tab() then
    return false
  end
  agent.mode = "wrapped"
  render_agent_now({ enter = false })
  return true
end

function M.resume_agent()
  local agent = agent_state()
  if agent.mode == "closed" or not agent.owner_tab or not vim.api.nvim_tabpage_is_valid(agent.owner_tab) then
    return false
  end
  if agent.owner_tab ~= current_tab() then
    vim.api.nvim_set_current_tabpage(agent.owner_tab)
  end
  agent.mode = "visible"
  render_agent_now({ enter = true })
  return true
end

function M.focus_agent()
  local agent = agent_state()
  if agent.owner_tab ~= current_tab() or agent.mode ~= "visible" then
    return false
  end
  if not valid_win(agent.win) then
    render_agent_now({ enter = true })
  elseif valid_win(agent.win) then
    ui.focus(agent.win)
  end
  return valid_win(agent.win)
end

function M.agent_actionable()
  local agent = agent_state()
  return agent.owner_tab == current_tab() and agent.mode == "visible" and valid_win(agent.win)
end

function M.agent_owner_tab()
  return agent_state().owner_tab
end

function M.agent_mode()
  return agent_state().mode
end

function M.agent_view()
  return agent_state().view
end

function M.set_agent_view(view)
  agent_state().view = view
end

function M.set_agent_working(value)
  agent_state().working = value == true
  if agent_state().mode == "wrapped" and agent_state().owner_tab == current_tab() then
    render_agent_now({ enter = false })
  end
end

function M.close_agent()
  local agent = agent_state()
  close_handle(agent.win)
  agent.win = nil
  agent.buf = nil
  agent.owner_tab = nil
  agent.mode = "closed"
  agent.view = nil
  agent.lines = nil
  agent.opts = nil
  agent.bind = nil
  agent.wrap = nil
  agent.cursorline = false
  agent.working = false
end

function M.close_all()
  M.close_prompt({ focus_agent = false })
  M.close_agent()
end

function M.snapshot()
  local prompt = prompt_state()
  local agent = agent_state()
  return {
    prompt = {
      mode = prompt.mode,
      win = prompt.win,
      buf = prompt.buf,
      frame_win = prompt.frame_win,
    },
    agent = {
      mode = agent.mode,
      view = agent.view,
      owner_tab = agent.owner_tab,
      win = agent.win,
      buf = agent.buf,
      working = agent.working,
    },
  }
end

return M
