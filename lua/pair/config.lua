local M = {}

M.values = {
  backend = {
    command = "paird",
    args = {},
    mode = "auto",
    agent = "mock",
  },
  agents = {
    mock = {
      kind = "mock",
    },
    codex = {
      kind = "codex_app",
      command = "codex",
      model = "gpt-5.4-mini",
      effort = "low",
      models = {},
      args = {
        "app-server",
        "--stdio",
      },
    },
    agent = {
      kind = "agent",
      command = "paird",
      args = { "dev", "stdio-agent" },
    },
    claude = {
      kind = "generic",
      command = "claude",
      args = {},
    },
    aider = {
      kind = "generic",
      command = "aider",
      args = {},
    },
    ["local"] = {
      kind = "generic",
      command = "ollama",
      args = { "run", "qwen2.5-coder:7b" },
    },
  },
  keymaps = {
    prompt = "<leader>a",
    reply = "<leader>pm",
    follow = "<leader>pf",
    why = "<leader>pw",
    fix = "<leader>px",
    other_lead = "<leader>pn",
    stop = "<leader>pq",
    hide = "<leader>ph",
    resume = "<leader>pr",
    reset = "<leader>pR",
    go_to = "<leader>pg",
    draft_accept = "<leader>pa",
    draft_reject = "<leader>pd",
    draft_retry = "<leader>pr",
  },
  prompt = {
    border = "rounded",
    width = 96,
    height = 10,
    padding_x = 4,
    padding_y = 2,
  },
  card = {
    border = "rounded",
    max_width = 64,
    max_height = 14,
  },
  thinking = {
    enabled = true,
    interval = 800,
  },
  context = {
    before = 24,
    after = 24,
    max_diagnostics = 8,
    max_diagnostic_length = 160,
  },
  navigation = {
    open = "current",
    annotate = true,
  },
  diff = {
    layout = "inline",
    apply_to_buffer = true,
    max_changed_lines = 32,
  },
}

function M.setup(opts)
  M.values = vim.tbl_deep_extend("force", M.values, opts or {})
  M.migrate_legacy_codex()

  return M.values
end

function M.migrate_legacy_codex()
  local codex = M.values.agents.codex

  if not codex or codex.command ~= "codex" or codex.kind ~= "generic" then
    return
  end

  codex.kind = "codex_app"
  codex.args = { "app-server", "--stdio" }
  codex.effort = codex.effort or "low"
  codex.model = codex.model or "gpt-5.4-mini"
end

function M.agent(name)
  if name then
    if not M.values.agents[name] then
      error("Unknown Pair agent: " .. name)
    end

    M.values.backend.agent = name
  end

  return M.values.backend.agent
end

function M.agent_config()
  local name = M.agent()
  local agent = M.values.agents[name]

  if not agent then
    error("Unknown Pair agent: " .. name)
  end

  return name, agent
end

function M.model(name)
  local _, agent = M.agent_config()

  if name then
    if name == "" then
      agent.model = nil
    else
      agent.model = name
    end
  end

  return agent.model
end

function M.model_names()
  local _, agent = M.agent_config()
  local names = {}

  for _, name in ipairs(agent.models or {}) do
    table.insert(names, name)
  end

  return names
end

function M.agent_args(agent)
  local args = vim.deepcopy(agent.args or {})
  local model = agent.model

  if model and model ~= "" then
    local flag = agent.model_flag or "--model"

    if flag ~= "" then
      table.insert(args, flag)
    end

    table.insert(args, model)
  end

  return args
end

function M.agent_names()
  local names = {}

  for name, _ in pairs(M.values.agents) do
    table.insert(names, name)
  end

  table.sort(names)

  return names
end

function M.backend_env()
  local _, agent = M.agent_config()

  if agent.kind == "mock" then
    return {
      PAIR_BACKEND = "mock",
    }
  end

  if agent.kind == "agent" then
    local args = M.agent_args(agent)

    return {
      PAIR_BACKEND = "agent_stdio",
      PAIR_AGENT_COMMAND = agent.command,
      PAIR_AGENT_ARGS = table.concat(args, " "),
      PAIR_AGENT_ARGS_JSON = vim.json.encode(args),
    }
  end

  if agent.kind == "codex_app" then
    local args = vim.deepcopy(agent.args or {})

    return {
      PAIR_BACKEND = "codex_app",
      PAIR_CODEX_COMMAND = agent.command,
      PAIR_CODEX_ARGS = table.concat(args, " "),
      PAIR_CODEX_ARGS_JSON = vim.json.encode(args),
      PAIR_CODEX_MODEL = agent.model or "",
      PAIR_CODEX_EFFORT = agent.effort or "low",
    }
  end

  local args = M.agent_args(agent)

  return {
    PAIR_BACKEND = "generic",
    PAIR_GENERIC_COMMAND = agent.command,
    PAIR_GENERIC_ARGS = table.concat(args, " "),
    PAIR_GENERIC_ARGS_JSON = vim.json.encode(args),
  }
end

return M
