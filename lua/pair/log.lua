local M = {}
local session_path = nil

function M.path()
  if not session_path then
    local directory = vim.fn.stdpath("state") .. "/pairagen/sessions"
    local ok = pcall(vim.fn.mkdir, directory, "p")
    if ok and vim.fn.isdirectory(directory) == 1 then
      session_path = string.format(
        "%s/%s-%s.jsonl",
        directory,
        os.date("%Y%m%dT%H%M%S"),
        vim.fn.getpid()
      )
    else
      session_path = vim.fn.tempname() .. "-pairagen.jsonl"
    end
  end

  return session_path
end

function M.write(message, data)
  M.event("client_log", {
    message = message,
    data = data,
  })
end

function M.event(kind, data)
  local record = {
    timestamp = os.date("!%Y-%m-%dT%H:%M:%SZ"),
    monotonic_ns = vim.uv.hrtime(),
    pid = vim.fn.getpid(),
    event = kind,
    data = data or {},
  }
  local ok, line = pcall(vim.json.encode, record)
  if not ok then
    line = vim.json.encode({
      timestamp = record.timestamp,
      pid = record.pid,
      event = "log_encode_error",
      data = {
        original_event = kind,
        error = tostring(line),
        fallback = vim.inspect(data),
      },
    })
  end

  pcall(vim.fn.writefile, { line }, M.path(), "a")
end

return M
