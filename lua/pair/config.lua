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
      args = {
        "exec",
        "--sandbox",
        "read-only",
        "--ask-for-approval",
        "never",
        "--color",
        "never",
        "--skip-git-repo-check",
        "-",
      },
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
      kind = "api",
      base_url = "http://127.0.0.1:11434/v1",
      model = "qwen2.5-coder:7b",
    },
  },
  keymaps = {
    prompt = "<leader>a",
  },
  prompt = {
    border = "rounded",
  },
  card = {
    border = "rounded",
    max_width = 72,
    max_height = 12,
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

  if agent.kind == "api" then
    local env = {
      PAIR_BACKEND = "openai_compat",
      PAIR_API_BASE = agent.base_url,
      PAIR_API_MODEL = agent.model,
    }

    if agent.api_key_env and vim.env[agent.api_key_env] then
      env.PAIR_API_KEY = vim.env[agent.api_key_env]
    elseif agent.api_key then
      env.PAIR_API_KEY = agent.api_key
    end

    return env
  end

  return {
    PAIR_BACKEND = "generic",
    PAIR_GENERIC_COMMAND = agent.command,
    PAIR_GENERIC_ARGS = table.concat(agent.args or {}, " "),
  }
end

return M
