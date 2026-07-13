local M = {}

M.values = {
  backend = {
    command = nil,
    args = {},
    mode = "auto",
    agent = "mock",
    -- Speculative prefetch spends a patch turn before the user asks for it.
    -- Keep it opt-in with "fix"; "off" never starts speculative model work.
    prefetch = "off",
  },
  distribution = {
    repository = "DorianDevp/pairagen",
    auto_install = true,
  },
  logging = {
    enabled = true,
    include_content = false,
    max_files = 20,
  },
  agents = {
    mock = {
      kind = "mock",
    },
    codex = {
      kind = "codex_app",
      command = "codex",
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
      kind = "claude_app",
      command = "claude",
      args = {},
      -- Discovery cards (hypothesis/finding/choice) run on a faster model
      -- with a capped thinking budget; patch drafting keeps the main model
      -- with adaptive thinking. Set either to nil to use the CLI default.
      discovery_model = "haiku",
      discovery_thinking = 1024,
    },
    aider = {
      kind = "generic",
      command = "aider",
      args = {},
    },
    ["local"] = {
      kind = "ollama",
      model = "qwen2.5-coder:7b",
      host = "http://127.0.0.1:11434",
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
    details = "z",
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
    zindex = 200,
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
    optimization = {
      enabled = true,
      total_token_budget = 2400,
      reserved_tokens = 700,
      primary_token_budget = 1000,
      max_artifacts = 4,
      snippet_lines = 10,
      max_scan_files = 2000,
      max_file_bytes = 524288,
      cache_ttl_ms = 1500,
      min_artifact_score = 40,
      exclude = {},
    },
    lsp = {
      enabled = true,
      timeout_ms = 120,
      max_locations = 16,
      workspace_timeout_ms = 120,
      max_workspace_queries = 3,
      definition = true,
      declaration = true,
      type_definition = true,
      implementation = true,
      references = false,
      workspace_symbols = true,
    },
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

M.explicit_models = {}

function M.setup(opts)
  M.explicit_models = {}
  for name, agent in pairs((opts and opts.agents) or {}) do
    if agent.model ~= nil then
      M.explicit_models[name] = true
    end
  end
  M.values = vim.tbl_deep_extend("force", M.values, opts or {})
  M.migrate_legacy_codex()
  M.load_models()

  return M.values
end

function M.preferences_path()
  return vim.fn.stdpath("state") .. "/pairagen/preferences.json"
end

function M.read_preferences()
  local path = M.preferences_path()
  if vim.fn.filereadable(path) ~= 1 then
    return { models = {} }
  end
  local ok, value = pcall(vim.json.decode, table.concat(vim.fn.readfile(path), "\n"))
  if not ok or type(value) ~= "table" then
    return { models = {} }
  end
  value.models = type(value.models) == "table" and value.models or {}
  return value
end

function M.load_models()
  local preferences = M.read_preferences()
  for name, model in pairs(preferences.models) do
    local agent = M.values.agents[name]
    if agent and not M.explicit_models[name] and type(model) == "string" and model ~= "" then
      agent.model = model
    end
  end
end

function M.persist_model(agent_name, model)
  local preferences = M.read_preferences()
  preferences.models[agent_name] = model and model ~= "" and model or nil
  local path = M.preferences_path()
  local directory = vim.fn.fnamemodify(path, ":h")
  local ok, error_message = pcall(function()
    if vim.fn.mkdir(directory, "p") == 0 and vim.fn.isdirectory(directory) ~= 1 then
      error("could not create " .. directory)
    end
    if vim.fn.writefile({ vim.json.encode(preferences) }, path) ~= 0 then
      error("could not write " .. path)
    end
  end)
  if ok then
    return true, nil
  end
  return false, tostring(error_message)
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
  local agent_name, agent = M.agent_config()

  if name then
    if name == "" then
      agent.model = nil
    else
      agent.model = name
    end
    if not M.explicit_models[agent_name] then
      local saved, error_message = M.persist_model(agent_name, agent.model)
      return agent.model, saved, error_message
    end
  end

  return agent.model, false
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
  local env = M.agent_env(agent)
  env.PAIR_PREFETCH = M.values.backend.prefetch or "off"

  return env
end

function M.agent_env(agent)
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

  if agent.kind == "claude_app" then
    local args = vim.deepcopy(agent.args or {})

    return {
      PAIR_BACKEND = "claude_app",
      PAIR_CLAUDE_COMMAND = agent.command,
      PAIR_CLAUDE_ARGS = table.concat(args, " "),
      PAIR_CLAUDE_ARGS_JSON = vim.json.encode(args),
      PAIR_CLAUDE_MODEL = agent.model or "",
      PAIR_CLAUDE_DISCOVERY_MODEL = agent.discovery_model or "",
      PAIR_CLAUDE_DISCOVERY_THINKING = agent.discovery_thinking
          and tostring(agent.discovery_thinking)
        or "",
    }
  end

  if agent.kind == "ollama" then
    return {
      PAIR_BACKEND = "ollama",
      PAIR_OLLAMA_MODEL = agent.model or "",
      PAIR_OLLAMA_HOST = agent.host or "",
      PAIR_OLLAMA_KEEP_ALIVE = agent.keep_alive or "",
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
