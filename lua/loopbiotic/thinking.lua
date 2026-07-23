local config = require("loopbiotic.config")
local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
local ui = require("loopbiotic.ui")

local M = {}
local uv = vim.uv or vim.loop

local width = 52
local max_steps = 3
local spinner = { "|", "/", "-", "\\" }

function M.start(label, session_id)
  if not config.values.thinking.enabled then
    return nil
  end

  M.stop(false)

  local request_id = tostring(uv.hrtime())

  state.thinking_frame = 0
  state.thinking_request_id = request_id
  state.thinking_session_id = session_id
  state.thinking_started_at = uv.hrtime()
  state.thinking_label = M.clean(label or "Preparing agent")
  state.thinking_preview = nil
  state.thinking_steps = {
    {
      phase = "starting",
      message = state.thinking_label,
      current = true,
    },
  }

  M.render()

  state.thinking_timer = uv.new_timer()
  state.thinking_timer:start(
    0,
    config.values.thinking.interval,
    vim.schedule_wrap(function()
      M.tick(request_id)
    end)
  )

  return request_id
end

function M.tick(request_id)
  if not M.current(request_id) then
    M.stop(false)

    return
  end

  state.thinking_frame = (state.thinking_frame or 0) + 1
  M.render()
end

function M.progress(progress)
  if not state.thinking_request_id then
    return
  end

  if state.thinking_session_id and progress.session_id ~= state.thinking_session_id then
    return
  end

  if progress.phase == "repairing" or progress.phase == "restarting" then
    state.thinking_preview = nil
  end

  local preview_updated = false
  if type(progress.preview) == "table" and type(progress.preview.title) == "string" then
    local preview = {
      title = progress.preview.title,
      body = type(progress.preview.body) == "string" and progress.preview.body or nil,
    }
    preview_updated = not vim.deep_equal(state.thinking_preview, preview)
    state.thinking_preview = preview
  end

  local message = M.clean(progress.message or "Agent is working")
  local phase = M.clean(progress.phase or "working")
  local steps = state.thinking_steps or {}
  local current = steps[#steps]

  if current and current.phase == phase and current.message == message then
    if preview_updated then
      M.render()
    end
    return
  end

  if current then
    current.current = false
  end

  table.insert(steps, {
    phase = phase,
    message = message,
    current = true,
  })

  while #steps > max_steps do
    table.remove(steps, 1)
  end

  state.thinking_steps = steps
  state.thinking_label = message
  M.render()
end

function M.render()
  if state.permission then
    -- A pending location-permission gate owns AgentWindow; the spinner keeps
    -- its request tracking and resumes rendering once the gate resolves.
    return
  end
  local lines = M.lines(state.thinking_frame or 0)
  local drafting = type(state.thinking_preview) == "table"
  surfaces.render_agent(lines, {
    view = "working",
    working = true,
    enter = false,
    window = {
      width = width,
      height = math.min(#lines, 8),
      border = config.values.card.border,
      anchor = M.anchor(),
      title = drafting and " Loopbiotic: Drafting " or " Loopbiotic: Working ",
    },
  })
end

function M.current(request_id)
  if request_id ~= state.thinking_request_id then
    return false
  end

  if state.thinking_session_id and state.session_id ~= state.thinking_session_id then
    return false
  end

  return true
end

function M.lines(frame)
  local steps = state.thinking_steps or {}
  local current = steps[#steps]
  local marker = spinner[(frame % #spinner) + 1]
  local preview = state.thinking_preview
  if type(preview) == "table" then
    local lines = {
      marker .. " Drafting response",
      "Elapsed  " .. M.elapsed() .. "s · validating before actions",
      "",
    }
    vim.list_extend(lines, M.wrap(preview.title or "Draft", width - 4, 2))
    if preview.body and preview.body ~= "" then
      table.insert(lines, "")
      vim.list_extend(lines, M.wrap(preview.body, width - 4, 3))
    end
    return lines
  end

  local lines = {
    marker .. " " .. (current and current.message or "Preparing agent"),
    "Elapsed  " .. M.elapsed() .. "s",
  }

  for _, step in ipairs(steps) do
    if not step.current then
      table.insert(lines, "Done     " .. step.message)
    end
  end

  return lines
end

function M.wrap(value, limit, max_lines)
  local lines = {}
  local current = ""
  local truncated = false

  for word in tostring(value):gmatch("%S+") do
    local candidate = current == "" and word or (current .. " " .. word)
    if current ~= "" and vim.fn.strdisplaywidth(candidate) > limit then
      table.insert(lines, current)
      current = word
      if #lines >= max_lines then
        truncated = true
        break
      end
    else
      current = candidate
    end
  end
  if current ~= "" and #lines < max_lines then
    table.insert(lines, current)
  elseif current ~= "" then
    truncated = true
  end
  if truncated and #lines > 0 and lines[#lines]:sub(-1) ~= "…" then
    lines[#lines] = lines[#lines] .. "…"
  end

  return lines
end

function M.anchor()
  local cursor = state.source_cursor or { 1, 0 }
  return ui.buffer_anchor(state.source_buf, cursor[1], cursor[2])
end

function M.elapsed()
  if not state.thinking_started_at then
    return 0
  end

  return math.max(0, math.floor((uv.hrtime() - state.thinking_started_at) / 1000000000))
end

function M.clean(value)
  local text = tostring(value):gsub("[\r\n]", " ")

  return text:sub(1, width - 8)
end

function M.stop(close)
  if state.thinking_timer and not state.thinking_timer:is_closing() then
    state.thinking_timer:stop()
    state.thinking_timer:close()
  end

  state.thinking_timer = nil
  state.thinking_request_id = nil
  state.thinking_session_id = nil
  state.thinking_started_at = nil
  state.thinking_label = nil
  state.thinking_steps = nil
  state.thinking_preview = nil
  surfaces.set_agent_working(false)

  if close then
    surfaces.close_agent()
  end
end

return M
