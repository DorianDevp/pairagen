local config = require("loopbiotic.config")
local selection = require("loopbiotic.selection")

local M = {}

function M.current(prompt, mode)
  local source = M.capture()
  local value = vim.deepcopy(source.value)

  value.prompt = prompt
  value.mode = mode or "auto"
  value.context_policy = vim.deepcopy(config.values.context.optimization)

  return value, source
end

function M.session()
  local value = M.capture(require("loopbiotic.state").source_buf).value
  value.hints = M.merge_hints(value.hints, require("loopbiotic.state").workspace_hints or {})

  return value
end

function M.new_file(file)
  return {
    cwd = vim.fn.getcwd(),
    file = vim.fn.fnamemodify(file, ":."),
    cursor = { line = 1, column = 1 },
    selection = nil,
    buffer_text = "",
    buffer_start_line = 1,
    diagnostics = {},
    hints = {},
  }
end

function M.file(file)
  local target = vim.fn.fnamemodify(file, ":p")
  local buf = vim.fn.bufnr(target)
  local lines
  local diagnostics = {}

  if buf >= 0 and vim.api.nvim_buf_is_loaded(buf) then
    lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
    diagnostics = M.diagnostics(target, buf)
  elseif vim.uv.fs_stat(target) then
    local ok, result = pcall(vim.fn.readfile, target, "b")
    if not ok then
      return nil
    end
    lines = result
  else
    return M.new_file(file)
  end

  return {
    cwd = vim.fn.getcwd(),
    file = vim.fn.fnamemodify(target, ":."),
    cursor = { line = 1, column = 1 },
    selection = nil,
    buffer_text = table.concat(lines, "\n"),
    buffer_start_line = 1,
    diagnostics = diagnostics,
    hints = {},
  }
end

function M.capture(preferred_buf)
  local buf = M.source_buffer(preferred_buf)
  local win = M.buffer_window(buf)
  local cursor = win and vim.api.nvim_win_get_cursor(win) or require("loopbiotic.state").source_cursor or { 1, 0 }
  local file = vim.api.nvim_buf_get_name(buf)
  local buffer_text, buffer_start_line = M.buffer_text(cursor[1], buf)
  local selected = nil

  if buf == vim.api.nvim_get_current_buf() then
    selected = selection.get()
  end

  return {
    buf = buf,
    value = {
      cwd = vim.fn.getcwd(),
      file = vim.fn.fnamemodify(file, ":."),
      cursor = {
        line = cursor[1],
        column = cursor[2] + 1,
      },
      selection = selected,
      buffer_text = buffer_text,
      buffer_start_line = buffer_start_line,
      diagnostics = M.diagnostics(file, buf),
      hints = M.lsp_hints(buf, cursor, vim.fn.getcwd()),
    },
  }
end

function M.lsp_hints(buf, cursor, cwd)
  local options = config.values.context.lsp or {}
  if options.enabled == false or not vim.lsp then
    return {}
  end

  local clients = vim.lsp.get_clients and vim.lsp.get_clients({ bufnr = buf }) or vim.lsp.get_active_clients({ bufnr = buf })
  local methods = {
    { enabled = options.definition, method = "textDocument/definition", kind = "definition" },
    { enabled = options.declaration, method = "textDocument/declaration", kind = "declaration" },
    { enabled = options.type_definition, method = "textDocument/typeDefinition", kind = "type_definition" },
    { enabled = options.implementation, method = "textDocument/implementation", kind = "implementation" },
    { enabled = options.references, method = "textDocument/references", kind = "reference" },
  }
  local params = {
    textDocument = { uri = vim.uri_from_bufnr(buf) },
    position = { line = cursor[1] - 1, character = cursor[2] },
  }
  local hints = {}
  local seen = {}
  local limit = options.max_locations or 16
  local timeout = options.timeout_ms or 120
  local started = vim.uv.hrtime()

  for _, item in ipairs(methods) do
    if item.enabled ~= false then
      for _, client in ipairs(clients or {}) do
        if #hints >= limit then
          return hints
        end
        local supported = not client.supports_method or client:supports_method(item.method, { bufnr = buf })
        if supported then
          local elapsed_ms = (vim.uv.hrtime() - started) / 1000000
          local remaining = math.floor(timeout - elapsed_ms)
          if remaining <= 0 then
            return hints
          end
          local request_params = vim.deepcopy(params)
          if item.kind == "reference" then
            request_params.context = { includeDeclaration = false }
          end
          local ok, response = pcall(client.request_sync, client, item.method, request_params, remaining, buf)
          if ok and response and not response.err and response.result then
            M.add_lsp_locations(hints, seen, response.result, item.kind, client.name or "lsp", limit, cwd)
          end
        end
      end
    end
  end

  return hints
end

function M.workspace_hints(prompt, cwd, buf)
  local options = config.values.context.lsp or {}
  if options.enabled == false or options.workspace_symbols == false or not vim.lsp then
    return {}
  end

  local clients = vim.lsp.get_clients and vim.lsp.get_clients({ bufnr = buf }) or vim.lsp.get_active_clients({ bufnr = buf })
  local queries = M.workspace_queries(prompt, options.max_workspace_queries or 3)
  local hints = {}
  local seen = {}
  local limit = options.max_locations or 16
  local timeout = options.workspace_timeout_ms or options.timeout_ms or 120
  local started = vim.uv.hrtime()

  for _, query in ipairs(queries) do
    for _, client in ipairs(clients or {}) do
      if #hints >= limit then
        return hints
      end
      local supported = not client.supports_method or client:supports_method("workspace/symbol")
      if supported then
        local elapsed_ms = (vim.uv.hrtime() - started) / 1000000
        local remaining = math.floor(timeout - elapsed_ms)
        if remaining <= 0 then
          return hints
        end
        local ok, response = pcall(client.request_sync, client, "workspace/symbol", { query = query }, remaining)
        if ok and response and not response.err and response.result then
          M.add_lsp_locations(
            hints,
            seen,
            response.result,
            "definition",
            (client.name or "lsp") .. ":workspace_symbol",
            limit,
            cwd
          )
        end
      end
    end
  end

  return hints
end

function M.workspace_queries(prompt, limit)
  local ignored = {
    concrete = true,
    potem = true,
    rzecz = true,
    struct = true,
    structów = true,
    template = true,
  }
  local weighted = {}
  local seen = {}
  for term in tostring(prompt or ""):lower():gmatch("[%w_%-]+") do
    if #term >= 5 and not ignored[term] and not seen[term] then
      seen[term] = true
      table.insert(weighted, {
        term = term,
        weight = (term:find("_", 1, true) and 1000 or 0) + #term,
      })
    end
  end
  table.sort(weighted, function(left, right)
    if left.weight == right.weight then
      return left.term < right.term
    end
    return left.weight > right.weight
  end)

  local queries = {}
  for index = 1, math.min(limit, #weighted) do
    table.insert(queries, weighted[index].term)
  end
  return queries
end

function M.merge_hints(left, right)
  local merged = {}
  local seen = {}
  for _, hints in ipairs({ left or {}, right or {} }) do
    for _, hint in ipairs(hints) do
      local key = table.concat({ hint.file or "", hint.line or 0, hint.column or 0 }, ":")
      if not seen[key] then
        seen[key] = true
        table.insert(merged, hint)
      end
    end
  end
  return merged
end

function M.add_lsp_locations(hints, seen, result, kind, source, limit, cwd)
  local locations = result
  if result.uri or result.targetUri then
    locations = { result }
  end
  if type(locations) ~= "table" then
    return
  end

  for _, location in ipairs(locations) do
    if #hints >= limit then
      return
    end
    local target = location.location or location
    local uri = target.targetUri or target.uri
    local range = target.targetSelectionRange or target.targetRange or target.range
    if uri and range and range.start then
      local ok, filename = pcall(vim.uri_to_fname, uri)
      local root = vim.fn.fnamemodify(cwd or vim.fn.getcwd(), ":p"):gsub("/$", "")
      local absolute = ok and vim.fn.fnamemodify(filename, ":p") or ""
      if ok and (absolute == root or absolute:sub(1, #root + 1) == root .. "/") then
        local file = vim.fn.fnamemodify(filename, ":.")
        local line = range.start.line + 1
        local column = range.start.character + 1
        local key = table.concat({ file, line, column }, ":")
        if not seen[key] then
          seen[key] = true
          table.insert(hints, {
            file = file,
            line = line,
            column = column,
            kind = kind,
            source = source,
          })
        end
      end
    end
  end
end

function M.source_buffer(preferred_buf)
  if preferred_buf and vim.api.nvim_buf_is_valid(preferred_buf) and vim.bo[preferred_buf].buftype == "" then
    return preferred_buf
  end

  local current = vim.api.nvim_get_current_buf()
  if vim.bo[current].buftype == "" and vim.api.nvim_buf_get_name(current) ~= "" then
    return current
  end

  local remembered = require("loopbiotic.state").source_buf
  if remembered and vim.api.nvim_buf_is_valid(remembered) and vim.bo[remembered].buftype == "" then
    return remembered
  end

  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    local win_config = vim.api.nvim_win_get_config(win)
    local buf = vim.api.nvim_win_get_buf(win)

    if win_config.relative == "" and vim.bo[buf].buftype == "" and vim.api.nvim_buf_get_name(buf) ~= "" then
      return buf
    end
  end

  return current
end

function M.buffer_window(buf)
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    if vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == buf then
      return win
    end
  end

  return nil
end

function M.buffer_text(line, buf)
  buf = buf or vim.api.nvim_get_current_buf()
  local before = config.values.context.before
  local after = config.values.context.after
  local start = math.max(line - before - 1, 0)
  local finish = math.min(line + after, vim.api.nvim_buf_line_count(buf))
  local lines = vim.api.nvim_buf_get_lines(buf, start, finish, false)

  return table.concat(lines, "\n"), start + 1
end

function M.diagnostics(file, buf)
  local items = {}
  local limit = config.values.context.max_diagnostics
  local buffers = { buf or 0 }
  local root = vim.fn.fnamemodify(vim.fn.getcwd(), ":p"):gsub("/$", "")
  for _, candidate in ipairs(vim.api.nvim_list_bufs()) do
    local candidate_file = vim.api.nvim_buf_get_name(candidate)
    local absolute = candidate_file ~= "" and vim.fn.fnamemodify(candidate_file, ":p") or ""
    local in_project = absolute == root or absolute:sub(1, #root + 1) == root .. "/"
    if candidate ~= buf and vim.api.nvim_buf_is_loaded(candidate) and in_project then
      table.insert(buffers, candidate)
    end
  end

  for _, diagnostic_buf in ipairs(buffers) do
    local diagnostic_file = diagnostic_buf == buf and file or vim.api.nvim_buf_get_name(diagnostic_buf)
    for _, diagnostic in ipairs(vim.diagnostic.get(diagnostic_buf)) do
      if #items >= limit then
        return items
      end
      table.insert(items, {
        file = vim.fn.fnamemodify(diagnostic_file, ":."),
        line = diagnostic.lnum + 1,
        column = diagnostic.col + 1,
        severity = tostring(diagnostic.severity),
        message = M.truncate(diagnostic.message, config.values.context.max_diagnostic_length),
      })
    end
  end

  return items
end

function M.truncate(text, limit)
  text = tostring(text or "")

  if #text <= limit then
    return text
  end

  return text:sub(1, limit) .. "..."
end

return M
