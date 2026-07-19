local state = require("loopbiotic.state")
local util = require("loopbiotic.util")

local M = {}
local registry = {}
local allowed_ref_kinds = {
  file = true,
  location = true,
  call_site = true,
  symbol = true,
  diagnostic = true,
}

function M.register(kind, version, spec)
  assert(type(kind) == "string" and kind ~= "", "widget kind is required")
  assert(type(version) == "number" and version >= 1, "widget version is required")
  registry[kind] = registry[kind] or {}
  registry[kind][version] = spec or {}
end

function M.validate(envelope)
  if type(envelope) ~= "table" or type(envelope.id) ~= "string" or envelope.id == "" then
    return nil, "invalid widget id"
  end
  local versions = registry[envelope.kind]
  local spec = versions and versions[envelope.version]
  if not spec then
    return nil, "unsupported widget kind or version"
  end
  if type(envelope.data) ~= "table" then
    return nil, "invalid widget data"
  end

  local intents = {}
  for _, intent in ipairs(envelope.intents or {}) do
    if spec.intents and spec.intents[intent] then
      table.insert(intents, intent)
    end
  end
  local valid, reason = spec.validate and spec.validate(envelope.data) or true
  if not valid then
    return nil, reason or "invalid widget payload"
  end
  local safe = vim.deepcopy(envelope)
  safe.intents = intents
  return safe
end

function M.validate_ref(ref)
  if type(ref) ~= "table" or type(ref.id) ~= "string" or ref.id == "" or not allowed_ref_kinds[ref.kind] then
    return nil, "invalid widget context reference"
  end
  if type(ref.file) ~= "string" or ref.file == "" or not util.in_workspace(ref.file) then
    return nil, "widget context is outside the workspace"
  end
  local safe = vim.deepcopy(ref)
  safe.file = vim.fn.fnamemodify(ref.file, ":p")
  safe.label = tostring(ref.label or vim.fn.fnamemodify(safe.file, ":."))
  safe.provenance = tostring(ref.provenance or "editor")
  if safe.range then
    local first = tonumber(safe.range.start_line)
    local last = tonumber(safe.range.end_line or first)
    if not first or first < 1 or not last or last < first then
      return nil, "invalid widget context range"
    end
    safe.range.start_line = first
    safe.range.end_line = last
    safe.range.start_column = math.max(tonumber(safe.range.start_column) or 1, 1)
    safe.range.end_column = math.max(tonumber(safe.range.end_column) or safe.range.start_column, 1)
  end
  return safe
end

function M.select(ref)
  local safe, reason = M.validate_ref(ref)
  if not safe then
    return false, reason
  end
  state.pending_widget_context[safe.id] = safe
  return true
end

function M.deselect(id)
  state.pending_widget_context[id] = nil
end

function M.toggle(ref)
  if state.pending_widget_context[ref.id] then
    M.deselect(ref.id)
    return false
  end
  return M.select(ref)
end

function M.list()
  local refs = {}
  for _, ref in pairs(state.pending_widget_context or {}) do
    local safe = M.validate_ref(ref)
    if safe then
      table.insert(refs, safe)
    else
      state.pending_widget_context[ref.id] = nil
    end
  end
  table.sort(refs, function(left, right)
    return left.id < right.id
  end)
  return refs
end

function M.summary()
  local refs = M.list()
  if #refs == 0 then
    return nil
  end
  local files = {}
  for _, ref in ipairs(refs) do
    files[ref.file] = true
  end
  return string.format("Context %d ref%s · %d file%s", #refs, #refs == 1 and "" or "s", vim.tbl_count(files), vim.tbl_count(files) == 1 and "" or "s")
end

function M.attach(context)
  context.hints = context.hints or {}
  for _, ref in ipairs(M.list()) do
    table.insert(context.hints, {
      file = vim.fn.fnamemodify(ref.file, ":."),
      line = ref.range and ref.range.start_line or 1,
      column = ref.range and ref.range.start_column or 1,
      kind = "reference",
      source = string.format("widget:%s:%s", ref.provenance, ref.id),
    })
  end
  return context
end

function M.clear()
  state.pending_widget_context = {}
end

function M.flow_ref(graph)
  local entry = require("loopbiotic.flow").current_entry(graph)
  if not entry then
    return nil
  end
  if entry.kind == "node" then
    local node = graph.node_by_id[entry.node_id]
    if not node then
      return nil
    end
    return M.validate_ref({
      id = "flow:symbol:" .. node.id,
      kind = "symbol",
      file = node.file,
      range = {
        start_line = node.line,
        start_column = node.column,
        end_line = node.end_line,
        end_column = node.end_column,
      },
      label = node.name,
      provenance = "lsp",
    })
  end
  local location = entry.location
  return M.validate_ref({
    id = string.format("flow:%s:%s:%d:%d", entry.kind, location.file, location.start_line, location.start_column),
    kind = entry.kind == "call" and "call_site" or "location",
    file = location.file,
    range = {
      start_line = location.start_line,
      start_column = location.start_column,
      end_line = location.end_line,
      end_column = location.end_column,
    },
    label = string.format("%s:%d", location.file, location.start_line),
    provenance = "lsp",
  })
end

M.register("flow", 1, {
  intents = { navigate = true, expand = true, select_context = true, inspect = true },
  validate = function(data)
    return type(data.graph) == "table", "Flow requires an editor-resolved graph"
  end,
})

return M
