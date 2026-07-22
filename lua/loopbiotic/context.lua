local config = require("loopbiotic.config")
local selection = require("loopbiotic.selection")
local util = require("loopbiotic.util")

local M = {}

function M.current(prompt, mode)
  local source = M.capture()
  local value = vim.deepcopy(source.value)

  value.prompt = prompt
  value.mode = mode or "investigate"
  value.context_policy = vim.deepcopy(config.values.context.optimization)

  return value, source
end

function M.session(prompt)
  local state = require("loopbiotic.state")
  local captured = M.capture(state.source_buf)
  local value = captured.value
  if type(prompt) == "string" and prompt ~= "" then
    state.workspace_hints = M.workspace_hints(prompt, value.cwd, captured.buf)
  end
  value.hints = M.merge_hints(value.hints, state.workspace_hints or {})
  value.call_hierarchy = require("loopbiotic.flow").bundle(require("loopbiotic.state").call_hierarchy)

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
    call_hierarchy = require("loopbiotic.flow").bundle(require("loopbiotic.state").call_hierarchy),
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
    call_hierarchy = require("loopbiotic.flow").bundle(require("loopbiotic.state").call_hierarchy),
  }
end

function M.capture(preferred_buf, opts)
  opts = opts or {}
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
      hints = opts.skip_lsp and {} or M.lsp_hints(buf, cursor, vim.fn.getcwd()),
      call_hierarchy = require("loopbiotic.flow").bundle(require("loopbiotic.state").call_hierarchy),
    },
  }
end

function M.project_signals(buf, cwd)
  if not vim.lsp then
    return { lsp_clients = {} }
  end
  local clients = vim.lsp.get_clients and vim.lsp.get_clients({ bufnr = buf })
    or vim.lsp.get_active_clients({ bufnr = buf })
  local capability_fields = {
    call_hierarchy = "callHierarchyProvider",
    declaration = "declarationProvider",
    definition = "definitionProvider",
    diagnostics = "diagnosticProvider",
    implementation = "implementationProvider",
    references = "referencesProvider",
    type_definition = "typeDefinitionProvider",
    workspace_symbols = "workspaceSymbolProvider",
  }
  local signals = {}
  for _, client in ipairs(clients or {}) do
    if #signals >= 16 then
      break
    end
    local capabilities = {}
    for label, field in pairs(capability_fields) do
      if (client.server_capabilities or {})[field] then
        table.insert(capabilities, label)
      end
    end
    table.sort(capabilities)
    local root = client.config and client.config.root_dir or nil
    root = type(root) == "string" and util.relative_path(cwd, root) or nil
    table.insert(signals, {
      name = tostring(client.name or client.id or "lsp"),
      version = client.server_info and client.server_info.version or nil,
      root = root,
      capabilities = capabilities,
    })
  end
  table.sort(signals, function(left, right)
    return left.name < right.name
  end)
  return { lsp_clients = signals }
end

function M.lsp_hints_async(buf, cursor, cwd, callback)
  local lsp_options = config.values.context.lsp or {}
  if lsp_options.enabled == false or not vim.lsp then
    callback({})
    return
  end
  if type(vim.lsp.buf_request_all) ~= "function" then
    vim.schedule(function()
      callback(M.lsp_hints(buf, cursor, cwd))
    end)
    return
  end

  local active_clients = vim.lsp.get_clients and vim.lsp.get_clients({ bufnr = buf })
    or vim.lsp.get_active_clients({ bufnr = buf })
  local methods = {
    { enabled = lsp_options.definition, method = "textDocument/definition", kind = "definition" },
    { enabled = lsp_options.declaration, method = "textDocument/declaration", kind = "declaration" },
    {
      enabled = lsp_options.type_definition,
      method = "textDocument/typeDefinition",
      kind = "type_definition",
    },
    { enabled = lsp_options.implementation, method = "textDocument/implementation", kind = "implementation" },
    { enabled = lsp_options.references, method = "textDocument/references", kind = "reference" },
  }
  local hints = {}
  local seen = {}
  local pending = 0
  local finished = false
  local cancel = {}
  local limit = lsp_options.max_locations or 16

  local function supports(client, method)
    if type(client.supports_method) ~= "function" then
      return true
    end
    local ok, value = pcall(client.supports_method, client, method, { bufnr = buf })
    return not ok or value == true
  end

  local function complete()
    if finished or pending > 0 then
      return
    end
    finished = true
    table.sort(hints, function(left, right)
      local left_key = table.concat({ left.file, left.line, left.column, left.kind }, ":")
      local right_key = table.concat({ right.file, right.line, right.column, right.kind }, ":")
      return left_key < right_key
    end)
    callback(hints)
  end

  for _, item in ipairs(methods) do
    local has_client = false
    if item.enabled ~= false then
      for _, client in ipairs(active_clients or {}) do
        has_client = has_client or supports(client, item.method)
      end
    end
    if has_client then
      local request_item = item
      pending = pending + 1
      local params = {
        textDocument = { uri = vim.uri_from_bufnr(buf) },
        position = { line = cursor[1] - 1, character = cursor[2] },
      }
      if request_item.kind == "reference" then
        params.context = { includeDeclaration = false }
      end
      local ok, cancel_request = pcall(vim.lsp.buf_request_all, buf, request_item.method, params, function(responses)
        if finished then
          return
        end
        for client_id, response in pairs(responses or {}) do
          if not response.error and response.result then
            local client = vim.lsp.get_client_by_id and vim.lsp.get_client_by_id(client_id) or nil
            M.add_lsp_locations(
              hints,
              seen,
              response.result,
              request_item.kind,
              (client and client.name) or tostring(client_id),
              limit,
              cwd
            )
          end
        end
        pending = pending - 1
        complete()
      end)
      if ok and type(cancel_request) == "function" then
        table.insert(cancel, cancel_request)
      elseif not ok then
        pending = pending - 1
      end
    end
  end

  if pending == 0 then
    complete()
    return
  end
  vim.defer_fn(function()
    if finished then
      return
    end
    pending = 0
    for _, cancel_request in ipairs(cancel) do
      pcall(cancel_request)
    end
    complete()
  end, lsp_options.timeout_ms or 120)
end

function M.lsp_hints(buf, cursor, cwd)
  local options = config.values.context.lsp or {}
  if options.enabled == false or not vim.lsp then
    return {}
  end

  local clients = vim.lsp.get_clients and vim.lsp.get_clients({ bufnr = buf })
    or vim.lsp.get_active_clients({ bufnr = buf })
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

  local clients = vim.lsp.get_clients and vim.lsp.get_clients({ bufnr = buf })
    or vim.lsp.get_active_clients({ bufnr = buf })
  local queries = M.workspace_queries(prompt, options.max_workspace_queries or 3)
  local hints = {}
  local seen = {}
  local limit = options.max_locations or 16
  local timeout = options.workspace_timeout_ms or options.timeout_ms or 120
  local started = vim.uv.hrtime()

  local query_quota = math.max(1, math.floor(limit / math.max(#queries, 1)))
  for query_index, query in ipairs(queries) do
    local query_limit = query_index == #queries and limit or math.min(limit, #hints + query_quota)
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
            query_limit,
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
    about = true,
    better = true,
    could = true,
    concrete = true,
    dlaczego = true,
    explain = true,
    improve = true,
    please = true,
    potem = true,
    proszę = true,
    review = true,
    rzecz = true,
    should = true,
    struct = true,
    ["structów"] = true,
    template = true,
    tego = true,
    this = true,
    what = true,
    would = true,
  }
  local weighted = {}
  local seen = {}
  for term in tostring(prompt or ""):lower():gmatch("[%w_%-]+") do
    if #term >= 4 and not ignored[term] and not seen[term] then
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
      if ok and util.in_workspace(filename, cwd) then
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
  local current_name = vim.api.nvim_buf_get_name(current)
  if vim.bo[current].buftype == "" and current_name ~= "" then
    return current
  end
  -- A directory listing (Netrw) is a legitimate prompt source: the user is
  -- steering file operations from the file tree, and its rendered listing is
  -- the visible context of that intent.
  if current_name ~= "" and vim.fn.isdirectory(current_name) == 1 then
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

-- True when the buffer is a directory listing (e.g. Netrw). Such a source is
-- a valid place to author intent, but has no LSP or call-hierarchy context.
function M.directory_source(buf)
  local name = buf and vim.api.nvim_buf_is_valid(buf) and vim.api.nvim_buf_get_name(buf) or ""
  return name ~= "" and vim.fn.isdirectory(name) == 1
end

function M.buffer_window(buf)
  -- A buffer can be visible in several splits (and tabs); prefer the split
  -- the user is working in, then the current tab, so cursor capture and
  -- navigation do not land in whichever window happens to be listed first.
  local current = vim.api.nvim_get_current_win()
  if vim.api.nvim_win_get_buf(current) == buf then
    return current
  end
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_is_valid(win) and vim.api.nvim_win_get_buf(win) == buf then
      return win
    end
  end
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
  for _, candidate in ipairs(vim.api.nvim_list_bufs()) do
    local candidate_file = vim.api.nvim_buf_get_name(candidate)
    local in_project = util.in_workspace(candidate_file)
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
