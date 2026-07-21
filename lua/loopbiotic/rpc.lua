local config = require("loopbiotic.config")
local installer = require("loopbiotic.installer")
local log = require("loopbiotic.log")
local state = require("loopbiotic.state")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

local M = {
  job = nil,
  ready = false,
  incompatible = false,
  queue = {},
  next_id = 1,
  pending = {},
  notifications = {},
  buffer = "",
  generation = 0,
}
local protocol_version = require("loopbiotic.version").protocol

-- Send one JSON-RPC line to the backend. Callers can run from scheduled
-- callbacks after on_exit already cleared the job, so a missing or closed
-- channel is logged instead of thrown.
---@param payload string encoded JSON-RPC message, without trailing newline
---@return boolean sent
local function try_send(payload)
  if not M.job then
    log.event("rpc_send_dropped", { reason = "backend is not running" })
    return false
  end

  local ok, sent = pcall(vim.fn.chansend, M.job, payload .. "\n")
  if not ok or sent == 0 then
    log.event("rpc_send_dropped", { reason = ok and "channel closed" or tostring(sent) })
    return false
  end

  return true
end

-- Start the backend if it is not already running or starting.
function M.ensure()
  if M.job and vim.fn.jobwait({ M.job }, 0)[1] == -1 then
    return
  end
  if M.starting then
    return
  end

  M.starting = true
  M.generation = M.generation + 1
  local generation = M.generation
  installer.resolve(function(command, error_message)
    if generation ~= M.generation then
      return
    end
    M.starting = false
    if not command then
      local message = "Could not prepare loopbioticd: " .. tostring(error_message)
      log.event("backend_install_error", { error = error_message })
      ui.notify(message, vim.log.levels.ERROR)
      M.fail_all(message)
      return
    end
    M.start(command)
  end)
end

-- Spawn the backend process and perform the protocol handshake.
---@param backend_command string
function M.start(backend_command)
  if M.job and vim.fn.jobwait({ M.job }, 0)[1] == -1 then
    return
  end

  local command = { backend_command }

  for _, arg in ipairs(config.values.backend.args or {}) do
    table.insert(command, arg)
  end

  M.job = vim.fn.jobstart(command, {
    cwd = vim.fn.getcwd(),
    env = config.backend_env(),
    stdout_buffered = false,
    stderr_buffered = false,
    on_stdout = function(job, data)
      if M.job ~= job then
        return
      end
      M.on_data(data)
    end,
    on_stderr = function(job, data)
      if M.job ~= job then
        return
      end
      M.on_stderr(data)
    end,
    on_exit = function(job, code)
      if M.job ~= job then
        -- A previous backend process exiting after a restart (stop mid-turn,
        -- agent/model switch) must not clobber the fresh job's state or fail
        -- its queued requests.
        log.event("backend_exit", { code = code, stale = true })
        return
      end
      log.event("backend_exit", { code = code })
      if code ~= 0 and code ~= 143 then
        log.write("backend exited", { code = code })
        ui.notify("Backend exited with code " .. code, vim.log.levels.ERROR)
      end
      M.job = nil
      M.ready = false
      -- The warmup identity described this process; the next one may report
      -- a different backend or model.
      state.agent_identity = nil
      M.fail_all("Loopbiotic backend exited with code " .. code)
    end,
  })

  if M.job <= 0 then
    error("Could not start Loopbiotic backend")
  end

  log.event("backend_start", {
    command = command,
    protocol_version = protocol_version,
  })

  M.ready = false
  M.incompatible = false
  M.send("initialize", {
    client = {
      name = "loopbiotic.nvim",
      protocol_version = protocol_version,
    },
  }, function(message)
    local actual = message.result and message.result.protocol_version
    if message.error or actual ~= protocol_version then
      local label = actual == nil and "legacy" or tostring(actual)
      local error_message = string.format(
        "Loopbiotic backend protocol mismatch: client requires %d, backend reports %s. Rebuild loopbioticd and run :LoopbioticReset.",
        protocol_version,
        label
      )
      M.incompatible = true
      log.event("protocol_mismatch", {
        expected = protocol_version,
        actual = actual,
      })
      if M.fail_all(error_message) == 0 then
        ui.notify(error_message, vim.log.levels.ERROR)
      end
      return
    end

    M.ready = true
    log.event("protocol_ready", message.result)
    local queued = M.queue
    M.queue = {}
    for _, request in ipairs(queued) do
      M.send(request.method, request.params, request.callback)
    end
  end)
end

-- Stop the backend. In-flight and queued requests are failed with an aborted
-- error (like on_exit) so their callers can settle instead of waiting forever.
function M.stop()
  M.generation = M.generation + 1
  if M.job and vim.fn.jobwait({ M.job }, 0)[1] == -1 then
    vim.fn.jobstop(M.job)
  end

  M.job = nil
  M.ready = false
  M.incompatible = false
  M.starting = false
  M.buffer = ""
  -- Agent and model switches restart the backend through this path, so the
  -- previously reported identity no longer applies.
  state.agent_identity = nil
  M.fail_all("Loopbiotic backend was stopped")
end

-- Send a request, starting and queueing on the backend when it is not ready
-- yet. The callback always receives a response table ({ result = ... } or
-- { error = ... }).
---@param method string
---@param params table|nil
---@param callback fun(message: table)
function M.request(method, params, callback)
  if M.incompatible then
    callback({
      error = {
        code = -32099,
        message = "Loopbiotic backend protocol is incompatible. Rebuild loopbioticd and run :LoopbioticReset.",
      },
    })
    return
  end

  if not M.ready then
    table.insert(M.queue, {
      method = method,
      params = params,
      callback = callback,
    })
    M.ensure()
    return
  end

  M.send(method, params, callback)
end

---@param method string
---@param params table|nil
---@param callback fun(message: table)
function M.send(method, params, callback)
  local id = M.next_id
  M.next_id = M.next_id + 1
  M.pending[id] = callback

  local payload = vim.json.encode({
    jsonrpc = "2.0",
    id = id,
    method = method,
    params = params or {},
  })

  log.event("rpc_request", {
    id = id,
    method = method,
    params = params or {},
  })
  if not try_send(payload) then
    M.pending[id] = nil
    callback({
      error = {
        code = -32098,
        message = "Loopbiotic backend is not running",
      },
    })
  end
end

-- Fail every queued and pending request with one aborted-error response.
---@param message string
---@return integer failed how many callbacks were invoked
function M.fail_all(message)
  local response = {
    error = {
      code = -32098,
      message = message,
    },
  }

  local queued = M.queue
  M.queue = {}
  local failed = #queued
  for _, request in ipairs(queued) do
    request.callback(response)
  end

  local pending = M.pending
  M.pending = {}
  for _, callback in pairs(pending) do
    failed = failed + 1
    callback(response)
  end
  return failed
end

-- Register a handler for a backend notification (method without id).
---@param method string
---@param callback fun(params: table)
function M.on(method, callback)
  M.notifications[method] = callback
end

-- Handlers for requests initiated by loopbioticd (method + id). The handler
-- receives (params, respond); it must call respond(result) exactly once.
---@param method string
---@param callback fun(params: table, respond: fun(result: table))
function M.on_request(method, callback)
  M.requests = M.requests or {}
  M.requests[method] = callback
end

-- Answer a backend-initiated request.
---@param id integer|string
---@param result table
function M.respond(id, result)
  local payload = vim.json.encode({
    jsonrpc = "2.0",
    id = id,
    result = result,
  })

  log.event("rpc_server_response", { id = id })
  try_send(payload)
end

function M.on_data(data)
  if not data or #data == 0 then
    return
  end

  local lines = vim.deepcopy(data)
  lines[1] = M.buffer .. (lines[1] or "")
  M.buffer = table.remove(lines) or ""

  for _, line in ipairs(lines) do
    if line ~= "" then
      M.handle(line)
    end
  end
end

function M.handle(line)
  local ok, message = pcall(vim.json.decode, line)

  if not ok or type(message) ~= "table" then
    log.write("invalid backend JSON", line)
    ui.notify("Invalid backend JSON", vim.log.levels.ERROR)

    return
  end

  message = util.normalize_json_nulls(message)
  log.event(message.method and not message.id and "rpc_notification" or "rpc_response", message)

  if message.method and not message.id then
    local callback = M.notifications[message.method]

    if callback then
      callback(message.params or {})
    end

    return
  end

  if message.method and message.id ~= nil then
    local handler = M.requests and M.requests[message.method]
    local id = message.id

    if not handler then
      try_send(vim.json.encode({
        jsonrpc = "2.0",
        id = id,
        error = { code = -32601, message = "unknown editor request " .. message.method },
      }))
      return
    end

    -- Error boundary: a bug in an editor-request handler must not unwind
    -- into the event loop and take the whole session down with it.
    vim.schedule(util.guard("editor request " .. message.method, function()
      handler(message.params or {}, function(result)
        M.respond(id, result)
      end)
    end))

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
    local message = table.concat(lines, "\n")

    log.write("backend stderr", message)
    log.event("backend_stderr", { message = message })
    ui.notify(message, vim.log.levels.WARN)
  end
end

return M
