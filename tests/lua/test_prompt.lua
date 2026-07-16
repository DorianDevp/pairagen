return function(t)
  local config = require("loopbiotic.config")
  local prompt = require("loopbiotic.prompt")
  local state = require("loopbiotic.state")

  t.test("model_label prefers the configured model", function()
    t.eq(prompt.model_label("configured", "identity", "backend"), "configured")
  end)

  t.test("model_label falls back to identity, then backend, then model?", function()
    t.eq(prompt.model_label(nil, "identity", "backend"), "identity")
    t.eq(prompt.model_label(nil, nil, "backend"), "backend")
    t.eq(prompt.model_label(nil, nil, nil), "model?")
  end)

  t.test("model_label treats empty strings and vim.NIL as unknown", function()
    t.eq(prompt.model_label("", "", ""), "model?")
    t.eq(prompt.model_label(vim.NIL, vim.NIL, vim.NIL), "model?")
    t.eq(prompt.model_label("", vim.NIL, "backend"), "backend")
  end)

  t.test("model_label never yields the word default", function()
    t.eq(prompt.model_label(nil, nil, nil) ~= "default", true)
    t.eq(prompt.model_label("default", nil, nil), "default") -- explicit input passes through
  end)

  t.test("title renders agent and resolved model, never default", function()
    local previous_agent = config.values.backend.agent
    config.values.backend.agent = "mock"
    state.agent_identity = nil
    state.backend_model = nil

    t.eq(prompt.title("Prompt"), " Loopbiotic Prompt · mock / model? ")

    state.agent_identity = { backend = "mock", model = "claude-fable-5", models = {} }
    t.eq(prompt.title("Reply"), " Loopbiotic Reply · mock / claude-fable-5 ")
    t.eq(prompt.title("Reply"):find("default", 1, true), nil, "no default in title")

    state.agent_identity = nil
    config.values.backend.agent = previous_agent
  end)

  t.test("model_candidates unions and dedupes in priority order", function()
    local candidates = prompt.model_candidates(
      "configured",
      { model = "identity", models = { "alpha", "configured", "beta" } },
      { "beta", "gamma" },
      "backend"
    )

    t.eq(candidates, { "configured", "identity", "alpha", "beta", "gamma", "backend" })
  end)

  t.test("model_candidates filters nil, vim.NIL, and empty values", function()
    t.eq(prompt.model_candidates(nil, nil, nil, nil), {})
    t.eq(prompt.model_candidates("", { model = vim.NIL, models = vim.NIL }, {}, ""), {})
    t.eq(prompt.model_candidates(nil, { models = { "", "only" } }, nil, nil), { "only" })
  end)

  t.test("on_warmup stores the identity and tolerates old daemons", function()
    state.agent_identity = nil

    prompt.on_warmup({ error = { code = -32098, message = "stopped" } })
    t.eq(state.agent_identity, nil, "error response")

    prompt.on_warmup({ result = { ok = true } })
    t.eq(state.agent_identity, nil, "legacy daemon without identity")

    prompt.on_warmup({ result = { ok = true, identity = vim.NIL } })
    t.eq(state.agent_identity, nil, "null identity")

    local identity = { backend = "mock", model = "mock-model", models = { "mock-model", "mock-mini" } }
    prompt.on_warmup({ result = { ok = true, identity = identity } })
    t.eq(state.agent_identity, identity, "identity stored")

    state.agent_identity = nil
  end)

  t.test("rpc.stop clears the stored agent identity", function()
    state.agent_identity = { backend = "mock", model = "mock-model" }

    require("loopbiotic.rpc").stop()

    t.eq(state.agent_identity, nil)
  end)

  t.test("keymaps.models defaults to <C-l>", function()
    t.eq(config.values.keymaps.models, "<C-l>")
  end)
end
