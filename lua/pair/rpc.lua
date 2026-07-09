local config = require("pair.config")
local ui = require("pair.ui")

local M = {
  job = nil,
  next_id = 1,
  pending = {},
  buffer = "",
}

function M.ensure()
  if M.job and vim.fn.jobwait({ M.job }, 0)[1] == -1 then
    return
  end

  local command = { config.values.backend.command }

  for _, arg in ipairs(config.values.backend.args or {}) do
    table.insert(command, arg)
  end

  M.job = vim.fn.jobstart(command, {
    cwd = vim.fn.getcwd(),
    env = config.backend_env(),
    stdout_buffered = false,
    stderr_buffered = false,
    on_stdout = function(_, data)
      M.on_data(data)
    end,
    on_stderr = function(_, data)
      M.on_stderr(data)
    end,
    on_exit = function(_, code)
      if code ~= 0 and code ~= 143 then
        ui.notify("Backend exited with code " .. code, vim.log.levels.ERROR)
      end
      M.job = nil
    end,
  })

  if M.job <= 0 then
    error("Could not start pair backend")
  end
end

function M.stop()
  if M.job and vim.fn.jobwait({ M.job }, 0)[1] == -1 then
    vim.fn.jobstop(M.job)
  end

  M.job = nil
  M.pending = {}
  M.buffer = ""
end

function M.request(method, params, callback)
  M.ensure()

  local id = M.next_id
  M.next_id = M.next_id + 1
  M.pending[id] = callback

  local payload = vim.json.encode({
    jsonrpc = "2.0",
    id = id,
    method = method,
    params = params or {},
  })

  vim.fn.chansend(M.job, payload .. "\n")
end

function M.on_data(data)
  for _, chunk in ipairs(data or {}) do
    if chunk ~= "" then
      M.read_chunk(chunk)
    end
  end
end

function M.read_chunk(chunk)
  local line = M.buffer .. chunk
  local ok = pcall(vim.json.decode, line)

  if ok then
    M.buffer = ""
    M.handle(line)

    return
  end

  M.buffer = line
end

function M.flush()
  while true do
    local index = M.buffer:find("\n", 1, true)

    if not index then
      return
    end

    local line = M.buffer:sub(1, index - 1)
    M.buffer = M.buffer:sub(index + 1)

    if line ~= "" then
      M.handle(line)
    end
  end
end

function M.handle(line)
  local ok, message = pcall(vim.json.decode, line)

  if not ok then
    ui.notify("Invalid backend JSON", vim.log.levels.ERROR)

    return
  end

  if message.id then
    local callback = M.pending[message.id]
    M.pending[message.id] = nil

    if callback then
      callback(message)
    end
  end
end

function M.on_stderr(data)
  local lines = vim.tbl_filter(function(line)
    return line ~= ""
  end, data or {})

  if #lines > 0 then
    ui.notify(table.concat(lines, "\n"), vim.log.levels.WARN)
  end
end

return M
