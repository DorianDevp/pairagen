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
      kind = "generic",
      command = "codex",
      timeout = 180,
      args = {
        "exec",
        "--sandbox",
        "read-only",
        "--color",
        "never",
        "--skip-git-repo-check",
        "-",
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
      timeout = 180,
      args = {},
    },
    aider = {
      kind = "generic",
      command = "aider",
      timeout = 180,
      args = {},
    },
    ["local"] = {
      kind = "generic",
      command = "ollama",
      timeout = 180,
      args = { "run", "qwen2.5-coder:7b" },
    },
  },
  keymaps = {
    prompt = "<leader>a",
    follow = "<leader>pf",
    why = "<leader>pw",
    fix = "<leader>px",
    other_lead = "<leader>pn",
    stop = "<leader>pq",
    hide = "<leader>ph",
    resume = "<leader>pr",
    reset = "<leader>pR",
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
    max_width = 72,
    max_height = 12,
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
    open = "tab",
    annotate = true,
  },
  diff = {
    layout = "tab",
    apply_to_buffer = true,
  },
}

function M.setup(opts)
  M.values = vim.tbl_deep_extend("force", M.values, opts or {})

  return M.values
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
    return {
      PAIR_BACKEND = "agent_stdio",
      PAIR_AGENT_COMMAND = agent.command,
      PAIR_AGENT_ARGS = table.concat(agent.args or {}, " "),
    }
  end

  return {
    PAIR_BACKEND = "generic",
    PAIR_GENERIC_COMMAND = agent.command,
    PAIR_GENERIC_ARGS = table.concat(agent.args or {}, " "),
    PAIR_GENERIC_TIMEOUT_SECS = tostring(agent.timeout or 180),
  }
end

return M
