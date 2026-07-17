local M = {}
local session_path = nil
local sensitive_keys = {
  annotation = true,
  artifacts = true,
  buffer_text = true,
  candidate_card = true,
  claim = true,
  detail = true,
  diff = true,
  explanation = true,
  finding = true,
  message = true,
  prompt = true,
  preview = true,
  raw_output = true,
  selection = true,
  summary = true,
  text = true,
}

function M.options()
  return require("loopbiotic.config").values.logging or {}
end

function M.path()
  if not session_path then
    local directory = vim.fn.stdpath("state") .. "/loopbiotic/sessions"
    local ok = pcall(vim.fn.mkdir, directory, "p")
    if ok and vim.fn.isdirectory(directory) == 1 then
      M.prune(directory)
      session_path = string.format("%s/%s-%s.jsonl", directory, os.date("%Y%m%dT%H%M%S"), vim.fn.getpid())
    else
      session_path = vim.fn.tempname() .. "-loopbiotic.jsonl"
    end
  end

  return session_path
end

function M.prune(directory)
  local limit = math.max(tonumber(M.options().max_files) or 20, 1)
  local files = vim.fn.globpath(directory, "*.jsonl", false, true)
  table.sort(files, function(left, right)
    return vim.fn.getftime(left) > vim.fn.getftime(right)
  end)
  for index = limit, #files do
    pcall(vim.fn.delete, files[index])
  end
end

function M.clear()
  local directory = vim.fn.stdpath("state") .. "/loopbiotic/sessions"
  for _, file in ipairs(vim.fn.globpath(directory, "*.jsonl", false, true)) do
    pcall(vim.fn.delete, file)
  end
  session_path = nil
end

function M.write(message, data)
  M.event("client_log", {
    message = message,
    data = data,
  })
end

function M.event(kind, data)
  if M.options().enabled == false then
    return
  end
  local record = {
    timestamp = os.date("!%Y-%m-%dT%H:%M:%SZ"),
    monotonic_ns = vim.uv.hrtime(),
    pid = vim.fn.getpid(),
    event = kind,
    data = M.sanitize(data or {}),
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

function M.sanitize(value, key, seen)
  if M.options().include_content == true then
    return value
  end
  if sensitive_keys[key] then
    local ok, encoded = pcall(vim.json.encode, value)
    encoded = ok and encoded or tostring(value)
    return {
      redacted = true,
      bytes = #encoded,
      sha256 = vim.fn.sha256(encoded),
    }
  end
  if type(value) ~= "table" then
    return value
  end
  seen = seen or {}
  if seen[value] then
    return "<cycle>"
  end
  seen[value] = true
  local output = {}
  for child_key, child in pairs(value) do
    output[child_key] = M.sanitize(child, child_key, seen)
  end
  seen[value] = nil
  return output
end

return M
