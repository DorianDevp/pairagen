local config = require("loopbiotic.config")
local navigation = require("loopbiotic.navigation")
local util = require("loopbiotic.util")

local M = {}
local generation = 0

local symbol_kinds = {
  "File",
  "Module",
  "Namespace",
  "Package",
  "Class",
  "Method",
  "Property",
  "Field",
  "Constructor",
  "Enum",
  "Interface",
  "Function",
  "Variable",
  "Constant",
  "String",
  "Number",
  "Boolean",
  "Array",
  "Object",
  "Key",
  "Null",
  "EnumMember",
  "Struct",
  "Event",
  "Operator",
  "TypeParameter",
}

local function options()
  return config.values.flow or {}
end

local function client_key(client)
  return tostring(client.id or client.name or client)
end

local function supported(client, method, buf)
  if type(client.supports_method) == "function" then
    local ok, value = pcall(client.supports_method, client, method, { bufnr = buf })
    if ok then
      return value == true
    end
  end

  local capabilities = client.server_capabilities or {}
  if method == "textDocument/prepareCallHierarchy" then
    return capabilities.callHierarchyProvider ~= nil and capabilities.callHierarchyProvider ~= false
  end
  if method == "textDocument/references" then
    return capabilities.referencesProvider ~= nil and capabilities.referencesProvider ~= false
  end
  return capabilities.callHierarchyProvider ~= nil and capabilities.callHierarchyProvider ~= false
end

local function clients(buf, method)
  if not vim.lsp then
    return {}
  end
  local all = vim.lsp.get_clients and vim.lsp.get_clients({ bufnr = buf })
    or vim.lsp.get_active_clients({ bufnr = buf })
  local out = {}
  for _, client in ipairs(all or {}) do
    if supported(client, method, buf) then
      table.insert(out, client)
    end
  end
  table.sort(out, function(left, right)
    return client_key(left) < client_key(right)
  end)
  return out
end

local function relative_file(uri, cwd)
  local ok, filename = pcall(vim.uri_to_fname, uri)
  if not ok then
    return nil
  end
  filename = vim.fn.fnamemodify(filename, ":p")
  if util.in_workspace(filename, cwd) then
    return vim.fn.fnamemodify(filename, ":.")
  end
  return filename
end

local function range_location(uri, range, cwd)
  if type(range) ~= "table" or type(range.start) ~= "table" then
    return nil
  end
  local file = relative_file(uri, cwd)
  if not file then
    return nil
  end
  local finish = type(range["end"]) == "table" and range["end"] or range.start
  return {
    file = file,
    start_line = (range.start.line or 0) + 1,
    start_column = (range.start.character or 0) + 1,
    end_line = (finish.line or range.start.line or 0) + 1,
    end_column = (finish.character or range.start.character or 0) + 1,
  }
end

local function location_key(location)
  return table.concat({
    location.file or "",
    location.start_line or 0,
    location.start_column or 0,
    location.end_line or 0,
    location.end_column or 0,
  }, ":")
end

local function notify(graph)
  if graph.generation ~= generation or graph._notify_pending then
    return
  end
  graph._notify_pending = true
  vim.defer_fn(function()
    -- Always release the latch; leaving it set on a superseded graph would
    -- permanently suppress notifications on it.
    graph._notify_pending = false
    if graph.generation ~= generation then
      return
    end
    if type(graph._listener) == "function" then
      graph._listener(graph)
    end
  end, options().render_batch_ms or 24)
end

local function request_group(graph, requests, callback)
  if #requests == 0 then
    callback({}, false, false)
    return
  end

  local remaining = #requests
  local results = {}
  local timed_out = false
  local had_error = false
  local finished = false
  graph.pending = graph.pending + #requests

  local function complete()
    if finished or remaining > 0 then
      return
    end
    finished = true
    callback(results, timed_out, had_error)
    notify(graph)
  end

  local function finish(entry, err, result)
    if entry.done then
      return
    end
    entry.done = true
    remaining = remaining - 1
    graph.pending = math.max(graph.pending - 1, 0)
    -- A superseded graph still settles its pending count above (so M.await /
    -- bundle.partial never hang on it) but must not accumulate results or fire
    -- the callback.
    if graph.generation ~= generation then
      return
    end
    if err then
      had_error = true
    elseif result ~= nil then
      table.insert(results, {
        client = entry.client,
        method = entry.method,
        result = result,
      })
    end
    complete()
  end

  for _, entry in ipairs(requests) do
    local ok, accepted, request_id = pcall(
      entry.client.request,
      entry.client,
      entry.method,
      entry.params,
      function(err, result)
        finish(entry, err, result)
      end,
      entry.buf
    )
    entry.request_id = request_id
    if not ok or accepted == false then
      finish(entry, ok and "request rejected" or accepted, nil)
    end
  end

  vim.defer_fn(function()
    -- Even when the graph is superseded, cancel outstanding requests and let
    -- finish() settle their pending count; only skip once already complete.
    if finished then
      return
    end
    timed_out = true
    for _, entry in ipairs(requests) do
      if not entry.done then
        if entry.request_id and type(entry.client.cancel_request) == "function" then
          pcall(entry.client.cancel_request, entry.client, entry.request_id)
        end
        finish(entry, "timeout", nil)
      end
    end
  end, options().request_timeout_ms or 1200)
end

local function item_range(item)
  return item.selectionRange or item.range
end

local function item_id(item)
  local range = item_range(item) or {}
  local start = range.start or {}
  return table.concat({ item.uri or "", start.line or 0, start.character or 0, item.name or "" }, "|")
end

local function add_item(graph, item, client, depth)
  if type(item) ~= "table" or not item.uri or not item_range(item) then
    return nil
  end
  local id = item_id(item)
  local existing = graph.node_by_id[id]
  if existing then
    existing.depth = math.min(existing.depth, depth)
    existing._items[client_key(client)] = item
    existing._clients[client_key(client)] = client
    return existing
  end

  if #graph.nodes >= (options().max_nodes or 40) then
    graph.truncated = true
    graph.partial = true
    return nil
  end

  local location = range_location(item.uri, item_range(item), graph.cwd)
  if not location then
    return nil
  end
  local node = {
    id = id,
    name = item.name or "<anonymous>",
    detail = item.detail,
    kind = symbol_kinds[item.kind] or tostring(item.kind or "Symbol"),
    file = location.file,
    line = location.start_line,
    column = location.start_column,
    end_line = location.end_line,
    end_column = location.end_column,
    depth = depth,
    call_site_count = 0,
    reference_count = 0,
    state = "ready",
    references = {},
    _uri = item.uri,
    _items = { [client_key(client)] = item },
    _clients = { [client_key(client)] = client },
    _references = {},
    _reference_keys = {},
  }
  graph.node_by_id[id] = node
  table.insert(graph.nodes, node)
  return node
end

local function add_edge(graph, from, to, uri, ranges, cycle)
  if not from or not to then
    return
  end
  local key = from.id .. "\0" .. to.id
  local edge = graph.edge_by_key[key]
  if not edge then
    edge = { from = from.id, to = to.id, call_sites = {}, cycle = cycle == true, _site_keys = {} }
    graph.edge_by_key[key] = edge
    table.insert(graph.edges, edge)
  else
    edge.cycle = edge.cycle or cycle == true
  end
  for _, range in ipairs(ranges or {}) do
    local location = range_location(uri, range, graph.cwd)
    if location then
      local site_key = location_key(location)
      if not edge._site_keys[site_key] then
        edge._site_keys[site_key] = true
        table.insert(edge.call_sites, location)
      end
    end
  end
end

local function call_site_keys(graph, target_id)
  local keys = {}
  for _, edge in ipairs(graph.edges) do
    if edge.to == target_id then
      for _, location in ipairs(edge.call_sites) do
        keys[location_key(location)] = true
      end
    end
  end
  return keys
end

local function recompute_counts(graph)
  for _, node in ipairs(graph.nodes) do
    local sites = call_site_keys(graph, node.id)
    local calls = 0
    for _ in pairs(sites) do
      calls = calls + 1
    end
    local references = {}
    for _, location in ipairs(node._references) do
      if not sites[location_key(location)] then
        table.insert(references, location)
      end
    end
    table.sort(references, function(left, right)
      return location_key(left) < location_key(right)
    end)
    node.call_site_count = calls
    node.reference_count = #references
    node.references = references
  end
end

local function node_buf(node, fallback)
  local ok, target = pcall(vim.uri_to_bufnr, node._uri)
  if ok and target and target >= 0 then
    return target
  end
  return fallback
end

local function update_node_state(node)
  if node._loading then
    node.state = "loading"
  elseif node._truncated then
    node.state = "truncated"
  elseif node._timed_out and not node._had_result then
    node.state = "timeout"
  elseif node._timed_out or node._had_error then
    node.state = "partial"
  elseif node._loaded then
    node.state = "ready"
  end
end

local function copy_set(value)
  local copy = {}
  for key, present in pairs(value or {}) do
    copy[key] = present
  end
  return copy
end

local load_node

load_node = function(graph, node, depth, ancestry, recurse)
  if graph.generation ~= generation or node._loading or node._loaded then
    return
  end
  node._loading = true
  node._calls_done = false
  node._refs_done = false
  update_node_state(node)
  notify(graph)

  local call_requests = {}
  local reference_requests = {}
  for key, item in pairs(node._items) do
    local client = node._clients[key]
    local buf = node_buf(node, graph.buf)
    for _, method in ipairs({ "callHierarchy/incomingCalls", "callHierarchy/outgoingCalls" }) do
      if supported(client, method, buf) then
        table.insert(call_requests, {
          client = client,
          method = method,
          params = { item = item },
          buf = buf,
        })
      end
    end
    if supported(client, "textDocument/references", buf) then
      local range = item_range(item)
      table.insert(reference_requests, {
        client = client,
        method = "textDocument/references",
        params = {
          textDocument = { uri = item.uri },
          position = vim.deepcopy(range.start),
          context = { includeDeclaration = false },
        },
        buf = buf,
      })
    end
  end

  local function done_part(kind, timed_out, had_error, had_result)
    node._timed_out = node._timed_out or timed_out
    node._had_error = node._had_error or had_error
    node._had_result = node._had_result or had_result
    if timed_out or had_error then
      graph.partial = true
    end
    if kind == "calls" then
      node._calls_done = true
      -- Only a clean calls result pins the node as loaded. A timeout or error
      -- must stay reloadable so pressing expand again retries (load_node gates
      -- on _loaded); otherwise a single slow callHierarchy request wedges the
      -- node's children forever.
      if not timed_out and not had_error then
        node._loaded = true
      end
    else
      node._refs_done = true
    end
    node._loading = not (node._calls_done and node._refs_done)
    update_node_state(node)
  end

  request_group(graph, call_requests, function(results, timed_out, had_error)
    local children = {}
    for _, response in ipairs(results) do
      local values = response.result
      if type(values) == "table" and (values.from or values.to) then
        values = { values }
      end
      for _, call in ipairs(type(values) == "table" and values or {}) do
        if response.method == "callHierarchy/incomingCalls" and type(call.from) == "table" then
          local caller = add_item(graph, call.from, response.client, depth + 1)
          if caller then
            local cycle = ancestry[caller.id] == true
            add_edge(graph, caller, node, call.from.uri, call.fromRanges, cycle)
            if not cycle then
              children[caller.id] = caller
            end
          elseif graph.truncated then
            node._truncated = true
          end
        elseif response.method == "callHierarchy/outgoingCalls" and type(call.to) == "table" then
          local callee = add_item(graph, call.to, response.client, depth + 1)
          if callee then
            local cycle = ancestry[callee.id] == true
            add_edge(graph, node, callee, node._uri, call.fromRanges, cycle)
            if not cycle then
              children[callee.id] = callee
            end
          elseif graph.truncated then
            node._truncated = true
          end
        end
      end
    end
    recompute_counts(graph)
    done_part("calls", timed_out, had_error, #results > 0)

    if recurse and depth + 1 < (options().initial_depth or 2) then
      local ids = {}
      for id in pairs(children) do
        table.insert(ids, id)
      end
      table.sort(ids)
      for _, id in ipairs(ids) do
        local child = children[id]
        local next_ancestry = copy_set(ancestry)
        next_ancestry[child.id] = true
        load_node(graph, child, depth + 1, next_ancestry, true)
      end
    else
      for _, child in pairs(children) do
        if not child._loaded and not child._loading then
          child.state = "unloaded"
        end
      end
    end
    notify(graph)
  end)

  request_group(graph, reference_requests, function(results, timed_out, had_error)
    for _, response in ipairs(results) do
      local values = response.result
      if type(values) == "table" and values.uri then
        values = { values }
      end
      for _, location in ipairs(type(values) == "table" and values or {}) do
        local uri = location.uri or location.targetUri
        local range = location.range or location.targetSelectionRange or location.targetRange
        local normalized = uri and range_location(uri, range, graph.cwd) or nil
        if normalized then
          local key = location_key(normalized)
          if not node._reference_keys[key] then
            node._reference_keys[key] = true
            table.insert(node._references, normalized)
          end
        end
      end
    end
    recompute_counts(graph)
    done_part("references", timed_out, had_error, #results > 0)
    notify(graph)
  end)
end

local function new_graph(buf, cursor, listener)
  generation = generation + 1
  return {
    generation = generation,
    status = "loading",
    cwd = vim.fn.getcwd(),
    buf = buf,
    cursor = { cursor[1], cursor[2] },
    root = nil,
    nodes = {},
    edges = {},
    node_by_id = {},
    edge_by_key = {},
    partial = false,
    truncated = false,
    unavailable = false,
    pending = 0,
    collapsed = {},
    view = "tree",
    view_node = nil,
    view_cursor = 1,
    _listener = listener,
  }
end

function M.start(buf, cursor, listener)
  local graph = new_graph(buf, cursor, listener)
  if options().enabled == false then
    graph.status = "unavailable"
    graph.unavailable = true
    notify(graph)
    return graph
  end

  local providers = clients(buf, "textDocument/prepareCallHierarchy")
  if #providers == 0 then
    graph.status = "unavailable"
    graph.unavailable = true
    notify(graph)
    return graph
  end

  local requests = {}
  for _, client in ipairs(providers) do
    table.insert(requests, {
      client = client,
      method = "textDocument/prepareCallHierarchy",
      params = {
        textDocument = { uri = vim.uri_from_bufnr(buf) },
        position = { line = cursor[1] - 1, character = cursor[2] },
      },
      buf = buf,
    })
  end

  request_group(graph, requests, function(results, timed_out, had_error)
    local candidates = {}
    for _, response in ipairs(results) do
      local values = response.result
      if type(values) == "table" and values.uri then
        values = { values }
      end
      for _, item in ipairs(type(values) == "table" and values or {}) do
        local id = item_id(item)
        candidates[id] = candidates[id] or {}
        table.insert(candidates[id], { item = item, client = response.client })
      end
    end

    local ids = {}
    for id in pairs(candidates) do
      table.insert(ids, id)
    end
    table.sort(ids)
    local selected = ids[1]
    if not selected then
      graph.status = timed_out and "timeout" or "empty"
      graph.partial = timed_out or had_error
      notify(graph)
      return
    end

    local root
    for _, candidate in ipairs(candidates[selected]) do
      root = add_item(graph, candidate.item, candidate.client, 0) or root
    end
    if not root then
      graph.status = "empty"
      notify(graph)
      return
    end
    graph.root = root.id
    graph.status = "ready"
    graph.partial = graph.partial or timed_out or had_error
    root._timed_out = timed_out
    root._had_error = had_error
    root._had_result = #results > 0
    load_node(graph, root, 0, { [root.id] = true }, true)
    notify(graph)
  end)

  return graph
end

function M.set_listener(graph, listener)
  if graph then
    graph._listener = listener
  end
end

function M.expand(graph, node_id)
  local node = graph and graph.node_by_id[node_id]
  if not node then
    return
  end
  graph.collapsed[node_id] = false
  if not node._loaded then
    load_node(graph, node, node.depth, { [node.id] = true }, false)
  end
  notify(graph)
end

local function relations(graph, node_id, direction)
  local out = {}
  for _, edge in ipairs(graph.edges) do
    if direction == "incoming" and edge.to == node_id then
      table.insert(out, { node_id = edge.from, edge = edge })
    elseif direction == "outgoing" and edge.from == node_id then
      table.insert(out, { node_id = edge.to, edge = edge })
    end
  end
  table.sort(out, function(left, right)
    local left_node = graph.node_by_id[left.node_id]
    local right_node = graph.node_by_id[right.node_id]
    local left_key = (left_node and left_node.name or "") .. left.node_id
    local right_key = (right_node and right_node.name or "") .. right.node_id
    return left_key < right_key
  end)
  return out
end

function M.tree_entries(graph)
  if not graph or not graph.root or not graph.node_by_id[graph.root] then
    return {}
  end
  local entries = { { kind = "node", node_id = graph.root, depth = 0, direction = "root" } }
  local function descend(node_id, direction, depth, seen)
    if graph.collapsed[node_id] then
      return
    end
    for _, relation in ipairs(relations(graph, node_id, direction)) do
      table.insert(entries, {
        kind = "node",
        node_id = relation.node_id,
        depth = depth,
        direction = direction,
        cycle = relation.edge.cycle or seen[relation.node_id] == true,
      })
      if not relation.edge.cycle and not seen[relation.node_id] then
        local next_seen = copy_set(seen)
        next_seen[relation.node_id] = true
        descend(relation.node_id, direction, depth + 1, next_seen)
      end
    end
  end
  descend(graph.root, "incoming", 1, { [graph.root] = true })
  descend(graph.root, "outgoing", 1, { [graph.root] = true })
  return entries
end

local function incoming_sites(graph, node_id)
  local out = {}
  local seen = {}
  for _, edge in ipairs(graph.edges) do
    if edge.to == node_id then
      for _, location in ipairs(edge.call_sites) do
        local key = location_key(location)
        if not seen[key] then
          seen[key] = true
          table.insert(out, { kind = "call", location = location })
        end
      end
    end
  end
  local node = graph.node_by_id[node_id]
  for _, location in ipairs(node and node.references or {}) do
    local key = location_key(location)
    if not seen[key] then
      seen[key] = true
      table.insert(out, { kind = "reference", location = location })
    end
  end
  table.sort(out, function(left, right)
    if left.kind ~= right.kind then
      return left.kind < right.kind
    end
    return location_key(left.location) < location_key(right.location)
  end)
  return out
end

function M.entries(graph)
  if not graph then
    return {}
  end
  if graph.view == "uses" and graph.view_node then
    return incoming_sites(graph, graph.view_node)
  end
  return M.tree_entries(graph)
end

local function branch_marker(graph, node)
  if node._loading then
    return "◌"
  end
  local has_children = #relations(graph, node.id, "incoming") + #relations(graph, node.id, "outgoing") > 0
  if graph.collapsed[node.id] then
    return "▸"
  end
  if not node._loaded then
    return "+"
  end
  return has_children and "▾" or "·"
end

local function trim(text, width)
  if width <= 3 or vim.fn.strdisplaywidth(text) <= width then
    return text
  end
  local count = vim.fn.strchars(text)
  while count > 0 and vim.fn.strdisplaywidth(vim.fn.strcharpart(text, 0, count)) > width - 3 do
    count = count - 1
  end
  return vim.fn.strcharpart(text, 0, count) .. "..."
end

function M.lines(graph, width)
  width = math.max(width or 52, 12)
  if not graph or graph.status == "loading" then
    return { "Flow", "", "◌ Discovering symbol..." }, {}
  end
  if graph.status == "unavailable" then
    return { "Flow", "", "Call hierarchy unavailable", "References remain separate context." }, {}
  end
  if graph.status == "empty" then
    return { "Flow", "", "No call hierarchy at cursor" }, {}
  end
  if graph.status == "timeout" and not graph.root then
    return { "Flow", "", "Call hierarchy timed out (partial)" }, {}
  end

  local entries = M.entries(graph)
  graph.view_cursor = math.max(1, math.min(graph.view_cursor or 1, math.max(#entries, 1)))
  local lines = {}
  if graph.view == "uses" then
    local node = graph.node_by_id[graph.view_node]
    table.insert(lines, trim("Uses · " .. (node and node.name or "symbol"), width))
    table.insert(lines, trim("u back · Enter open · s context", width))
  else
    local suffix = graph.truncated and " · truncated" or graph.partial and " · partial" or ""
    table.insert(lines, trim("Flow" .. suffix, width))
    table.insert(lines, trim("j/k select · h/l fold · u uses · s context · R root", width))
  end
  table.insert(lines, "")

  for _, entry in ipairs(entries) do
    if entry.kind == "node" then
      local node = graph.node_by_id[entry.node_id]
      local selected = require("loopbiotic.state").pending_widget_context["flow:symbol:" .. node.id] and "[x]" or "[ ]"
      local arrow = entry.direction == "incoming" and "↑" or entry.direction == "outgoing" and "↓" or "◆"
      local indent = string.rep("  ", math.min(entry.depth or 0, 6))
      local state_suffix = node.state ~= "ready" and (" · " .. node.state) or ""
      local cycle_suffix = entry.cycle and " · cycle" or ""
      local text = string.format(
        "%s%s %s %s %s · %s %s:%d · calls %d · refs %d%s%s",
        indent,
        branch_marker(graph, node),
        selected,
        arrow,
        node.name,
        node.kind,
        node.file,
        node.line,
        node.call_site_count,
        node.reference_count,
        state_suffix,
        cycle_suffix
      )
      table.insert(lines, trim(text, width))
    else
      local location = entry.location
      local label = entry.kind == "call" and "call" or "ref "
      local ref_id =
        string.format("flow:%s:%s:%d:%d", entry.kind, location.file, location.start_line, location.start_column)
      local selected = require("loopbiotic.state").pending_widget_context[ref_id] and "[x]" or "[ ]"
      table.insert(
        lines,
        trim(
          string.format("%s %s  %s:%d:%d", selected, label, location.file, location.start_line, location.start_column),
          width
        )
      )
    end
  end
  if #entries == 0 then
    table.insert(lines, "No uses resolved")
  end
  return lines, entries
end

---Render an agent-selected path using only nodes and edges already resolved
---by the editor. Invalid ids are ignored rather than presented as LSP facts.
---@param graph table
---@param ids string[]
---@param width integer
---@return string[], string[]
function M.path_lines(graph, ids, width)
  width = math.max(width or 52, 18)
  local nodes = {}
  local resolved_ids = {}
  local seen = {}
  for _, id in ipairs(type(ids) == "table" and ids or {}) do
    local node = graph and graph.node_by_id and graph.node_by_id[id] or nil
    if node and not seen[id] then
      seen[id] = true
      table.insert(nodes, node)
      table.insert(resolved_ids, id)
    end
  end

  local suffix = graph and graph.truncated and " · truncated" or graph and graph.partial and " · partial" or ""
  local lines = { trim("Call path" .. suffix, width), "" }
  if #nodes == 0 then
    table.insert(lines, "No resolved path")
    return lines, resolved_ids
  end

  for index, node in ipairs(nodes) do
    table.insert(lines, trim(string.format("%d  %s", index, node.name), width))
    table.insert(lines, trim(string.format("   %s · %s:%d", node.kind, node.file, node.line), width))
    local next_node = nodes[index + 1]
    if next_node then
      local edge = graph.edge_by_key[node.id .. "\0" .. next_node.id]
      if edge then
        local site = edge.call_sites[1]
        local label = string.format("   │ %d call-site%s", #edge.call_sites, #edge.call_sites == 1 and "" or "s")
        if site then
          label = label .. string.format(" · %s:%d", site.file, site.start_line)
        end
        table.insert(lines, trim(label, width))
      else
        table.insert(lines, "   │ unresolved link")
      end
      table.insert(lines, "   ▼")
    end
  end
  return lines, resolved_ids
end

function M.move(graph, delta)
  local entries = M.entries(graph)
  if #entries == 0 then
    return
  end
  graph.view_cursor = math.max(1, math.min((graph.view_cursor or 1) + delta, #entries))
  notify(graph)
end

function M.current_entry(graph)
  local entries = M.entries(graph)
  return entries[math.max(1, math.min(graph.view_cursor or 1, #entries))]
end

function M.collapse(graph)
  local entry = M.current_entry(graph)
  if not entry or entry.kind ~= "node" then
    return
  end
  if not graph.collapsed[entry.node_id] then
    graph.collapsed[entry.node_id] = true
  elseif (entry.depth or 0) > 0 then
    local entries = M.tree_entries(graph)
    for index = math.min(graph.view_cursor - 1, #entries), 1, -1 do
      if (entries[index].depth or 0) < (entry.depth or 0) then
        graph.view_cursor = index
        break
      end
    end
  end
  notify(graph)
end

function M.expand_current(graph)
  local entry = M.current_entry(graph)
  if entry and entry.kind == "node" then
    M.expand(graph, entry.node_id)
  end
end

function M.toggle_uses(graph)
  if graph.view == "uses" then
    graph.view = "tree"
    graph.view_node = nil
  else
    local entry = M.current_entry(graph)
    if not entry or entry.kind ~= "node" then
      return
    end
    graph.view = "uses"
    graph.view_node = entry.node_id
  end
  graph.view_cursor = 1
  notify(graph)
end

local function open_location(location)
  if not location then
    return false
  end
  local target = vim.fn.fnamemodify(location.file, ":p")
  local loaded = vim.fn.bufnr(target)
  if not vim.uv.fs_stat(target) and not (loaded >= 0 and vim.api.nvim_buf_is_loaded(loaded)) then
    require("loopbiotic.ui").notify("Flow location is no longer available: " .. location.file, vim.log.levels.WARN)
    return false
  end
  return navigation.open_location({
    file = location.file,
    line = location.start_line or location.line,
    column = location.start_column or location.column,
  })
end

function M.open_current(graph)
  local entry = M.current_entry(graph)
  if not entry then
    return false
  end
  if entry.kind == "node" then
    local node = graph.node_by_id[entry.node_id]
    return open_location(node)
  end
  return open_location(entry.location)
end

function M.open_node(graph, node_id)
  local node = graph and graph.node_by_id and graph.node_by_id[node_id] or nil
  return node and open_location(node) or false
end

function M.root_here(graph)
  local source = require("loopbiotic.context").source_buffer()
  local win = require("loopbiotic.context").buffer_window(source)
  local cursor = win and vim.api.nvim_win_get_cursor(win) or { 1, 0 }
  return M.start(source, cursor, graph and graph._listener or nil)
end

local function snippet_lines(graph, node)
  local target = vim.fn.fnamemodify(node.file, ":p")
  if not util.in_workspace(target, graph.cwd) then
    return nil
  end
  local buf = vim.fn.bufnr(target)
  local lines
  if buf >= 0 and vim.api.nvim_buf_is_loaded(buf) then
    lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  elseif vim.uv.fs_stat(target) then
    local ok, value = pcall(vim.fn.readfile, target, "", 100000)
    if ok then
      lines = value
    end
  end
  if not lines or #lines == 0 then
    return nil
  end
  local first = math.max(node.line - 3, 1)
  local last = math.min(node.end_line + 3, #lines)
  local out = {}
  for line = first, last do
    table.insert(out, string.format("%d %s", line, lines[line]))
  end
  return table.concat(out, "\n")
end

function M.bundle(graph)
  if not graph or options().enabled == false then
    return nil
  end
  local value = {
    root = graph.root,
    nodes = {},
    edges = {},
    partial = graph.partial or graph.pending > 0 or graph.status == "timeout",
    truncated = graph.truncated,
    unavailable = graph.unavailable,
  }
  local ordered = {}
  for _, node in ipairs(graph.nodes) do
    table.insert(ordered, node)
  end
  table.sort(ordered, function(left, right)
    if left.id == graph.root then
      return true
    end
    if right.id == graph.root then
      return false
    end
    if left.depth ~= right.depth then
      return left.depth < right.depth
    end
    local left_use = left.call_site_count + left.reference_count
    local right_use = right.call_site_count + right.reference_count
    if left_use ~= right_use then
      return left_use > right_use
    end
    return left.id < right.id
  end)

  local remaining = options().snippet_token_budget or 800
  for _, node in ipairs(ordered) do
    local public = {
      id = node.id,
      name = node.name,
      detail = node.detail,
      kind = node.kind,
      file = node.file,
      line = node.line,
      column = node.column,
      end_line = node.end_line,
      end_column = node.end_column,
      depth = node.depth,
      call_site_count = node.call_site_count,
      reference_count = node.reference_count,
      state = node.state,
      references = vim.deepcopy(node.references),
    }
    local snippet = snippet_lines(graph, node)
    local tokens = snippet and math.ceil(#snippet / 4) or 0
    if snippet and tokens <= remaining then
      public.snippet = snippet
      remaining = remaining - tokens
    end
    table.insert(value.nodes, public)
  end

  for _, edge in ipairs(graph.edges) do
    table.insert(value.edges, {
      from = edge.from,
      to = edge.to,
      call_sites = vim.deepcopy(edge.call_sites),
      cycle = edge.cycle,
    })
  end
  table.sort(value.edges, function(left, right)
    return left.from .. "\0" .. left.to < right.from .. "\0" .. right.to
  end)
  return value
end

function M.await(graph, timeout_ms, callback, ready)
  local started = vim.uv.hrtime()
  local function poll()
    local elapsed = (vim.uv.hrtime() - started) / 1000000
    local graph_ready = not graph or graph.pending == 0
    local extra_ready = not ready or ready()
    if (graph_ready and extra_ready) or elapsed >= timeout_ms then
      callback(M.bundle(graph))
      return
    end
    vim.defer_fn(poll, 10)
  end
  poll()
end

return M
