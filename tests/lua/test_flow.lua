local flow = require("loopbiotic.flow")

local function lsp_range(line, column)
  return {
    start = { line = line, character = column or 0 },
    ["end"] = { line = line, character = (column or 0) + 4 },
  }
end

local function item(uri, name, line)
  return {
    name = name,
    kind = 12,
    uri = uri,
    range = lsp_range(line, 0),
    selectionRange = lsp_range(line, 0),
  }
end

local function client(id, scenario)
  local value = { id = id, name = "flow-" .. id, server_capabilities = { callHierarchyProvider = true } }
  function value:supports_method(method)
    return method ~= "textDocument/references" or scenario.references ~= false
  end
  function value:request(method, params, callback)
    local key = method
    if params.item then
      key = key .. ":" .. params.item.name
    end
    local result = scenario[key]
    if type(result) == "function" then
      result = result(params, self)
    end
    if result ~= "timeout" then
      vim.schedule(function()
        callback(nil, vim.deepcopy(result))
      end)
    end
    scenario.request_id = (scenario.request_id or 0) + 1
    return true, scenario.request_id
  end
  function value:cancel_request() end
  return value
end

local function with_lsp(scenarios, callback)
  local old_get_clients = vim.lsp.get_clients
  local old_options = vim.deepcopy(require("loopbiotic.config").values.flow)
  local path = string.format("%s/.loopbiotic-flow-test-%s.lua", vim.fn.getcwd(), tostring(vim.uv.hrtime()))
  local lines = {}
  for index = 1, 40 do
    table.insert(lines, string.format("local line_%d = %d", index, index))
  end
  vim.fn.writefile(lines, path)
  local buf = vim.fn.bufadd(path)
  vim.fn.bufload(buf)
  local uri = vim.uri_from_bufnr(buf)
  local clients = {}
  for index, scenario in ipairs(scenarios) do
    scenario.uri = uri
    table.insert(clients, client(index, scenario))
  end
  vim.lsp.get_clients = function()
    return clients
  end

  local ok, error_message = pcall(callback, buf, uri, scenarios, function(next_clients)
    clients = next_clients
  end)
  vim.lsp.get_clients = old_get_clients
  require("loopbiotic.config").values.flow = old_options
  if vim.api.nvim_buf_is_valid(buf) then
    vim.api.nvim_buf_delete(buf, { force = true })
  end
  vim.fn.delete(path)
  if not ok then
    error(error_message, 0)
  end
end

local function normal_scenario(uri)
  local root = item(uri, "root", 1)
  local caller_a = item(uri, "caller_a", 10)
  local caller_b = item(uri, "caller_b", 14)
  local callee = item(uri, "callee", 20)
  return {
    ["textDocument/prepareCallHierarchy"] = { root },
    ["callHierarchy/incomingCalls:root"] = {
      { from = caller_a, fromRanges = { lsp_range(11, 2), lsp_range(12, 2) } },
      { from = caller_b, fromRanges = { lsp_range(15, 2) } },
    },
    ["callHierarchy/outgoingCalls:root"] = {
      { to = callee, fromRanges = { lsp_range(2, 2), lsp_range(3, 2) } },
    },
    ["callHierarchy/incomingCalls:caller_a"] = {},
    ["callHierarchy/outgoingCalls:caller_a"] = {},
    ["callHierarchy/incomingCalls:caller_b"] = {},
    ["callHierarchy/outgoingCalls:caller_b"] = {},
    ["callHierarchy/incomingCalls:callee"] = {},
    ["callHierarchy/outgoingCalls:callee"] = {},
    ["textDocument/references"] = {
      { uri = uri, range = lsp_range(11, 2) },
      { uri = uri, range = lsp_range(30, 1) },
    },
  }
end

local function wait_graph(graph, timeout)
  return vim.wait(timeout or 800, function()
    return graph.root ~= nil and graph.pending == 0
  end, 5)
end

local function public_graph(path)
  local id = "file://" .. path .. "|0|0|root"
  local node = {
    id = id,
    name = "root",
    kind = "Function",
    file = path,
    line = 1,
    column = 1,
    end_line = 1,
    end_column = 5,
    depth = 0,
    call_site_count = 0,
    reference_count = 0,
    state = "ready",
    references = {},
    _loaded = true,
    _references = {},
  }
  return {
    generation = -1,
    status = "ready",
    cwd = vim.fn.getcwd(),
    root = id,
    nodes = { node },
    edges = {},
    node_by_id = { [id] = node },
    edge_by_key = {},
    partial = false,
    truncated = false,
    unavailable = false,
    pending = 0,
    collapsed = {},
    view = "tree",
    view_cursor = 1,
  }
end

return function(t)
  t.test("Flow resolves callers, callees, exact call-sites and non-call references", function()
    with_lsp({ {} }, function(buf, uri, scenarios)
      scenarios[1] = normal_scenario(uri)
      vim.lsp.get_clients = function()
        return { client(1, scenarios[1]) }
      end
      local graph = flow.start(buf, { 2, 0 })
      t.eq(wait_graph(graph), true, "graph settled")
      t.eq(#graph.nodes, 4, "deduped symbols")
      t.eq(#graph.edges, 3, "two callers and one callee")
      local root = graph.node_by_id[graph.root]
      t.eq(root.call_site_count, 3)
      t.eq(root.reference_count, 1)
      local callee
      for _, node in ipairs(graph.nodes) do
        if node.name == "callee" then
          callee = node
        end
      end
      t.eq(callee.call_site_count, 2, "several ranges stay on one edge")
      local bundle = flow.bundle(graph)
      t.eq(bundle.root, graph.root)
      t.eq(#bundle.nodes, 4)
      t.eq(bundle.partial, false)
      t.eq(bundle.nodes[1].id, graph.root, "root wins deterministic snippet priority")
      t.eq(type(bundle.nodes[1].snippet), "string")
    end)
  end)

  t.test("Flow dedupes identical responses from multiple LSP clients", function()
    with_lsp({ {}, {} }, function(buf, uri, scenarios)
      scenarios[1] = normal_scenario(uri)
      scenarios[2] = normal_scenario(uri)
      vim.lsp.get_clients = function()
        return { client(1, scenarios[1]), client(2, scenarios[2]) }
      end
      local graph = flow.start(buf, { 2, 0 })
      t.eq(wait_graph(graph), true)
      t.eq(#graph.nodes, 4)
      t.eq(#graph.edges, 3)
      t.eq(graph.node_by_id[graph.root].call_site_count, 3)
    end)
  end)

  t.test("Flow marks cycles without recursively rebuilding them", function()
    with_lsp({ {} }, function(buf, uri, scenarios)
      local root = item(uri, "root", 1)
      local child = item(uri, "child", 5)
      scenarios[1] = {
        ["textDocument/prepareCallHierarchy"] = { root },
        ["callHierarchy/incomingCalls:root"] = {},
        ["callHierarchy/outgoingCalls:root"] = { { to = child, fromRanges = { lsp_range(2) } } },
        ["callHierarchy/incomingCalls:child"] = {},
        ["callHierarchy/outgoingCalls:child"] = { { to = root, fromRanges = { lsp_range(6) } } },
        ["textDocument/references"] = {},
      }
      vim.lsp.get_clients = function()
        return { client(1, scenarios[1]) }
      end
      local graph = flow.start(buf, { 2, 0 })
      t.eq(wait_graph(graph), true)
      t.eq(#graph.nodes, 2)
      local cycles = 0
      for _, edge in ipairs(graph.edges) do
        cycles = cycles + (edge.cycle and 1 or 0)
      end
      t.eq(cycles, 1)
    end)
  end)

  t.test("Flow enforces max_nodes and exposes a deterministic truncation", function()
    with_lsp({ {} }, function(buf, uri, scenarios)
      local root = item(uri, "root", 1)
      local calls = {}
      for index = 1, 8 do
        table.insert(calls, { to = item(uri, "child_" .. index, index + 2), fromRanges = { lsp_range(index + 1) } })
      end
      scenarios[1] = {
        ["textDocument/prepareCallHierarchy"] = { root },
        ["callHierarchy/incomingCalls:root"] = {},
        ["callHierarchy/outgoingCalls:root"] = calls,
        ["textDocument/references"] = {},
      }
      require("loopbiotic.config").values.flow.max_nodes = 3
      vim.lsp.get_clients = function()
        return { client(1, scenarios[1]) }
      end
      local graph = flow.start(buf, { 2, 0 })
      t.eq(wait_graph(graph), true)
      t.eq(#graph.nodes, 3)
      t.eq(graph.truncated, true)
      t.eq(flow.bundle(graph).partial, true)
    end)
  end)

  t.test("Flow reports timeout/partial and neutral provider absence", function()
    with_lsp({ {} }, function(buf, uri, scenarios)
      local root = item(uri, "root", 1)
      scenarios[1] = {
        ["textDocument/prepareCallHierarchy"] = { root },
        ["callHierarchy/incomingCalls:root"] = "timeout",
        ["callHierarchy/outgoingCalls:root"] = {},
        ["textDocument/references"] = {},
      }
      require("loopbiotic.config").values.flow.request_timeout_ms = 20
      vim.lsp.get_clients = function()
        return { client(1, scenarios[1]) }
      end
      local graph = flow.start(buf, { 2, 0 })
      t.eq(wait_graph(graph, 300), true)
      t.eq(graph.partial, true)
      t.eq(graph.node_by_id[graph.root].state, "partial")

      vim.lsp.get_clients = function()
        return {}
      end
      local unavailable = flow.start(buf, { 2, 0 })
      t.eq(unavailable.status, "unavailable")
      t.eq(flow.bundle(unavailable).unavailable, true)
      t.eq(flow.lines(unavailable, 50)[3], "Call hierarchy unavailable")
    end)
  end)

  t.test("Flow discards a late prepare response after Root here generation changes", function()
    with_lsp({ {} }, function(buf, uri)
      local late_callback
      local stale = client(1, {})
      function stale:request(_, _, callback)
        late_callback = callback
        return true, 1
      end
      vim.lsp.get_clients = function()
        return { stale }
      end
      local first = flow.start(buf, { 2, 0 })

      local scenario = normal_scenario(uri)
      scenario["textDocument/prepareCallHierarchy"] = { item(uri, "new_root", 4) }
      scenario["callHierarchy/incomingCalls:new_root"] = {}
      scenario["callHierarchy/outgoingCalls:new_root"] = {}
      vim.lsp.get_clients = function()
        return { client(2, scenario) }
      end
      local second = flow.start(buf, { 5, 0 })
      t.eq(wait_graph(second), true)
      late_callback(nil, { item(uri, "stale_root", 1) })
      vim.wait(20)
      t.eq(#first.nodes, 0)
      t.eq(second.node_by_id[second.root].name, "new_root")
    end)
  end)

  t.test("Flow snippet packing obeys its independent token budget", function()
    with_lsp({ {} }, function(buf, uri, scenarios)
      scenarios[1] = normal_scenario(uri)
      vim.lsp.get_clients = function()
        return { client(1, scenarios[1]) }
      end
      local graph = flow.start(buf, { 2, 0 })
      t.eq(wait_graph(graph), true)
      require("loopbiotic.config").values.flow.snippet_token_budget = 1
      local bundle = flow.bundle(graph)
      for _, node in ipairs(bundle.nodes) do
        t.eq(node.snippet, nil)
      end
    end)
  end)

  t.test("Flow loads branches beyond initial depth only after explicit expansion", function()
    with_lsp({ {} }, function(buf, uri, scenarios)
      local root = item(uri, "root", 1)
      local child = item(uri, "child", 5)
      local grandchild = item(uri, "grandchild", 9)
      local leaf = item(uri, "leaf", 13)
      scenarios[1] = {
        ["textDocument/prepareCallHierarchy"] = { root },
        ["callHierarchy/incomingCalls:root"] = {},
        ["callHierarchy/outgoingCalls:root"] = { { to = child, fromRanges = { lsp_range(2) } } },
        ["callHierarchy/incomingCalls:child"] = {},
        ["callHierarchy/outgoingCalls:child"] = { { to = grandchild, fromRanges = { lsp_range(6) } } },
        ["callHierarchy/incomingCalls:grandchild"] = {},
        ["callHierarchy/outgoingCalls:grandchild"] = { { to = leaf, fromRanges = { lsp_range(10) } } },
        ["textDocument/references"] = {},
      }
      vim.lsp.get_clients = function()
        return { client(1, scenarios[1]) }
      end
      local graph = flow.start(buf, { 2, 0 })
      t.eq(wait_graph(graph), true)
      t.eq(#graph.nodes, 3)
      local boundary
      for _, node in ipairs(graph.nodes) do
        if node.name == "grandchild" then
          boundary = node
        end
      end
      t.eq(boundary.state, "unloaded")
      flow.expand(graph, boundary.id)
      t.eq(
        vim.wait(500, function()
          return graph.pending == 0 and #graph.nodes == 4
        end, 5),
        true
      )
      t.eq(boundary.state, "ready")
    end)
  end)

  t.test("Flow await sends a partial snapshot after the short submit deadline", function()
    local graph = public_graph("tests/lua/test_flow.lua")
    graph.pending = 1
    local bundle
    flow.await(graph, 20, function(value)
      bundle = value
    end)
    t.eq(
      vim.wait(200, function()
        return bundle ~= nil
      end, 5),
      true
    )
    t.eq(bundle.partial, true)
  end)

  t.test("prompt capture leaves ordinary LSP hints on the asynchronous path", function()
    with_lsp({ {} }, function(buf, uri, scenarios)
      local context = require("loopbiotic.context")
      local sync_requests = 0
      local lsp_client = client(1, scenarios[1])
      function lsp_client:request_sync()
        sync_requests = sync_requests + 1
        return { result = {} }
      end
      vim.lsp.get_clients = function()
        return { lsp_client }
      end
      local captured = context.capture(buf, { skip_lsp = true })
      t.eq(sync_requests, 0)
      t.eq(captured.value.hints, {})

      local old_request_all = vim.lsp.buf_request_all
      vim.lsp.buf_request_all = function(_, method, _, callback)
        vim.schedule(function()
          callback({
            [1] = {
              result = method == "textDocument/definition" and { { uri = uri, range = lsp_range(3, 1) } } or {},
            },
          })
        end)
      end
      local hints
      context.lsp_hints_async(buf, { 2, 0 }, vim.fn.getcwd(), function(value)
        hints = value
      end)
      t.eq(hints, nil, "lookup did not block prompt creation")
      t.eq(
        vim.wait(300, function()
          return hints ~= nil
        end, 5),
        true
      )
      vim.lsp.buf_request_all = old_request_all
      t.eq(#hints, 1)
      t.eq(hints[1].kind, "definition")
    end)
  end)

  t.test("Flow navigation opens exact workspace uses and rejects stale files", function()
    local path = vim.fn.getcwd() .. "/.loopbiotic-test-" .. tostring((vim.uv or vim.loop).hrtime()) .. ".lua"
    vim.fn.writefile({ "first", "second", "third" }, path)
    local graph = public_graph(path)
    local node = graph.node_by_id[graph.root]
    node.references = {
      { file = path, start_line = 2, start_column = 2, end_line = 2, end_column = 4 },
    }
    graph.view = "uses"
    graph.view_node = graph.root
    t.eq(flow.open_current(graph), true)
    t.eq(vim.api.nvim_win_get_cursor(0), { 2, 1 })

    vim.cmd("enew")
    local loaded = vim.fn.bufnr(path)
    if loaded >= 0 and vim.api.nvim_buf_is_valid(loaded) then
      vim.api.nvim_buf_delete(loaded, { force = true })
    end
    vim.fn.delete(path)
    graph.view = "tree"
    local old_notify = vim.notify
    vim.notify = function() end
    t.eq(flow.open_current(graph), false)
    vim.notify = old_notify
  end)

  t.test("card Flow stays closed unless toggled or selected by the agent", function()
    local card = require("loopbiotic.card")
    local state = require("loopbiotic.state")
    local options = require("loopbiotic.config").values.flow
    local old_columns = vim.o.columns
    local old_threshold = options.responsive_split
    state.call_hierarchy = public_graph("tests/lua/test_flow.lua")
    options.responsive_split = 100
    vim.o.columns = 160
    state.card_flow_active = false
    local plain, _, visible = card.workspace({ "Context" }, 32)
    t.eq(visible, false)
    t.eq(plain, { "Context" })

    state.card_flow_active = true
    local wide, _, toggled = card.workspace({ "Context" }, 32)
    t.eq(toggled, true)
    t.eq(wide[1]:find("│", 1, true) ~= nil, true)

    state.card_flow_active = false
    local root = state.call_hierarchy.root
    local selected, _, selected_visible = card.workspace({ "Answer" }, 32, { flow_path = { root } })
    t.eq(selected_visible, true)
    t.eq(selected[1]:find("Call path", 1, true) ~= nil, true)
    local rejected, _, rejected_visible = card.workspace({ "Answer" }, 32, { flow_path = { "invented" } })
    t.eq(rejected_visible, false)
    t.eq(rejected, { "Answer" })

    vim.o.columns = 80
    local context_lines, _, context_flow = card.workspace({ "Context" }, 32)
    t.eq(context_flow, false)
    t.eq(context_lines, { "Context" })
    state.card_flow_active = true
    local flow_lines, _, narrow_flow = card.workspace({ "Context" }, 32)
    t.eq(narrow_flow, true)
    t.eq(flow_lines[1], "Flow")

    state.card_flow_active = false
    local stacked, _, path_visible = card.workspace({ "Answer" }, 32, { flow_path = { root } })
    t.eq(path_visible, true)
    t.eq(stacked[1], "Answer")
    t.eq(table.concat(stacked, "\n"):find("Call path", 1, true) ~= nil, true)

    state.card_flow_active = false
    state.call_hierarchy = nil
    options.responsive_split = old_threshold
    vim.o.columns = old_columns
  end)

  t.test("PromptWindow never renders Flow as a pane", function()
    local prompt = require("loopbiotic.prompt")
    local surfaces = require("loopbiotic.surfaces")
    prompt.open_for({ title = " Prompt test ", footer = " Test ", submit = function() end })
    local snapshot = surfaces.snapshot()
    t.eq(snapshot.prompt.mode, "open")
    t.eq(snapshot.agent.mode, "closed", "Flow did not create an AgentWindow before a response")
    t.eq(vim.api.nvim_list_wins() and #vim.api.nvim_list_wins() >= 3, true, "PromptWindow uses only its technical frames")
    prompt.close()
  end)

  t.test("agent-selected Flow path renders LSP nodes and exact call-sites", function()
    local graph = public_graph("lua/loopbiotic/init.lua")
    local root = graph.node_by_id[graph.root]
    root.name = "command"
    local child_id = "file://lua/loopbiotic/prompt.lua|0|0|open"
    local child = {
      id = child_id,
      name = "loopbiotic.prompt",
      kind = "Function",
      file = "lua/loopbiotic/prompt.lua",
      line = 42,
      column = 1,
      end_line = 42,
      end_column = 5,
      depth = 1,
      call_site_count = 1,
      reference_count = 0,
      state = "ready",
      references = {},
    }
    table.insert(graph.nodes, child)
    graph.node_by_id[child_id] = child
    local edge = {
      from = graph.root,
      to = child_id,
      call_sites = {
        { file = "lua/loopbiotic/init.lua", start_line = 17, start_column = 3, end_line = 17, end_column = 9 },
      },
    }
    table.insert(graph.edges, edge)
    graph.edge_by_key[graph.root .. "\0" .. child_id] = edge

    local lines, ids = flow.path_lines(graph, { graph.root, "invented", child_id }, 80)
    local rendered = table.concat(lines, "\n")
    t.eq(ids, { graph.root, child_id }, "invented agent ids are not rendered")
    t.eq(rendered:find("command", 1, true) ~= nil, true)
    t.eq(rendered:find("loopbiotic.prompt", 1, true) ~= nil, true)
    t.eq(rendered:find("1 call-site · lua/loopbiotic/init.lua:17", 1, true) ~= nil, true)
  end)
end
