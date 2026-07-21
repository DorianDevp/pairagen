return function(t)
  local config = require("loopbiotic.config")
  local prompt = require("loopbiotic.prompt")
  local state = require("loopbiotic.state")

  t.test("model_label prefers the actually-used backend model", function()
    -- The honest headline: what the turn actually ran wins over the pick.
    t.eq(prompt.model_label("configured", { model = "identity" }, "backend"), "backend")
  end)

  t.test("model_label falls back to configured, then identity, then model?", function()
    t.eq(prompt.model_label("configured", { model = "identity" }, nil), "configured")
    t.eq(prompt.model_label(nil, { model = "identity" }, nil), "identity")
    t.eq(prompt.model_label(nil, nil, "backend"), "backend")
    t.eq(prompt.model_label(nil, nil, nil), "model?")
  end)

  t.test("model_label treats empty strings and vim.NIL as unknown", function()
    t.eq(prompt.model_label("", { model = "" }, ""), "model?")
    t.eq(prompt.model_label(vim.NIL, { model = vim.NIL }, vim.NIL), "model?")
    t.eq(prompt.model_label("", { model = vim.NIL }, "backend"), "backend")
  end)

  t.test("model_label reflects the actual per-turn model, with no discovery suffix", function()
    local identity = { model = "claude-fable-5", phases = { discovery = "haiku", patch = "claude-fable-5" } }
    -- A discovery turn actually ran haiku; the headline says so plainly.
    t.eq(prompt.model_label(nil, identity, "haiku"), "haiku")
    -- A patch turn actually ran the patch model.
    t.eq(prompt.model_label(nil, identity, "claude-fable-5"), "claude-fable-5")
    -- Before any turn, the advertised identity model stands in, no suffix.
    t.eq(prompt.model_label(nil, identity, nil), "claude-fable-5")
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

    t.eq(prompt.title("Prompt", "investigate"), " Loopbiotic Prompt · investigate · mock / model? ")

    state.agent_identity = { backend = "mock", model = "claude-fable-5", models = {} }
    t.eq(prompt.title("Reply", "fix"), " Loopbiotic Reply · fix · mock / claude-fable-5 ")
    t.eq(prompt.title("Reply", "fix"):find("default", 1, true), nil, "no default in title")

    state.agent_identity = nil
    config.values.backend.agent = previous_agent
  end)

  t.test("title names the model of the phase the next turn will run", function()
    local previous_agent = config.values.backend.agent
    config.values.backend.agent = "mock"
    state.backend_model = nil
    state.backend_models = nil
    state.agent_identity = {
      backend = "mock",
      model = "gpt-5.4",
      phases = { patch = "gpt-5.4", discovery = "gpt-5.4-mini" },
    }

    t.eq(prompt.title("Prompt", "investigate"), " Loopbiotic Prompt · investigate · mock / gpt-5.4-mini ")
    t.eq(prompt.title("Prompt", "fix"), " Loopbiotic Prompt · fix · mock / gpt-5.4 ")

    -- A reported actual only ever feeds the phase that ran it.
    state.backend_models = { discovery = "haiku-actual" }
    t.eq(prompt.title("Prompt", "review"), " Loopbiotic Prompt · review · mock / haiku-actual ")
    t.eq(prompt.title("Reply", "fix"), " Loopbiotic Reply · fix · mock / gpt-5.4 ")

    state.agent_identity = nil
    state.backend_models = nil
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

  t.test("model_candidates includes the discovery model", function()
    local candidates = prompt.model_candidates(
      nil,
      { model = vim.NIL, phases = { discovery = "haiku", patch = vim.NIL }, models = { "sonnet" } },
      nil,
      nil
    )

    -- Discovery is selectable so the user can steer investigate/explain/review
    -- turns, not only patch turns.
    t.eq(candidates, { "haiku", "sonnet" })
  end)

  t.test("model_candidates filters nil, vim.NIL, and empty values", function()
    t.eq(prompt.model_candidates(nil, nil, nil, nil), {})
    t.eq(prompt.model_candidates("", { model = vim.NIL, models = vim.NIL }, {}, ""), {})
    t.eq(prompt.model_candidates(nil, { models = { "", "only" } }, nil, nil), { "only" })
  end)

  t.test("model picker targets the current mode's phase", function()
    t.eq(prompt.model_phase("fix"), "patch")
    t.eq(prompt.model_phase("propose"), "patch")
    t.eq(prompt.model_phase("investigate"), "discovery")
    t.eq(prompt.model_phase("explain"), "discovery")
    t.eq(prompt.model_phase("review"), "discovery")
  end)

  t.test("discovery_model is settable per agent and independent of the patch model", function()
    local previous_agent = config.values.backend.agent
    config.values.backend.agent = "mock"
    -- Treat the model as explicitly configured so the setter does not touch the
    -- real preferences.json on disk during the test.
    config.explicit_models.mock = true

    config.model("opus")
    config.discovery_model("haiku")
    t.eq(config.discovery_model(), "haiku")
    t.eq(config.model(), "opus", "patch model is independent of discovery")

    config.discovery_model("")
    t.eq(config.discovery_model(), nil, "cleared discovery model")
    t.eq(config.model(), "opus", "clearing discovery leaves the patch model")

    config.model("")
    config.explicit_models.mock = nil
    config.values.backend.agent = previous_agent
  end)

  t.test("on_warmup stores the identity and tolerates old daemons", function()
    state.agent_identity = nil

    -- The preflight path now warns on error responses; keep this test about
    -- identity handling (test_safety.lua covers the preflight behaviour).
    local original_notify = vim.notify
    vim.notify = function() end
    prompt.on_warmup({ error = { code = -32098, message = "stopped" } })
    vim.notify = original_notify
    t.eq(state.agent_identity, nil, "error response")
    state.backend_preflight_error = nil

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

  t.test("every PromptWindow exposes the complete mode picker", function()
    t.eq(prompt.mode_candidates(), { "fix", "explain", "investigate", "review", "propose" })
    t.eq(config.values.keymaps.modes, "<C-k>")

    local surfaces = require("loopbiotic.surfaces")
    state.reset()
    surfaces.open_prompt({
      row = 14,
      col = 4,
      outer_width = 60,
      outer_height = 10,
      inner_width = 52,
      inner_height = 6,
      padding_x = 4,
      padding_y = 2,
      title = " Prompt ",
      footer = " footer ",
    })

    prompt.pick_mode()
    local buf = state.surfaces.prompt.picker_buf
    local win = state.surfaces.prompt.picker_win
    t.eq(type(buf), "number", "mode picker is the subordinate prompt Frame")

    local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
    t.eq(#lines, #prompt.mode_candidates(), "every mode is listed")
    t.eq(lines[1]:find("Fix", 1, true) ~= nil, true, "modes render their labels")
    local current_index
    for index, mode in ipairs(prompt.mode_candidates()) do
      if mode == prompt.current_mode() then
        current_index = index
      end
    end
    t.eq(lines[current_index]:sub(1, 3), "[x]", "the active mode is marked")

    local function bound(lhs)
      local wanted = vim.api.nvim_replace_termcodes(lhs, true, true, true)
      for _, map in ipairs(vim.api.nvim_buf_get_keymap(buf, "n")) do
        if vim.api.nvim_replace_termcodes(map.lhs, true, true, true) == wanted then
          return map.callback
        end
      end
    end

    vim.api.nvim_win_set_cursor(win, { 1, 0 })
    bound("<CR>")()
    t.eq(prompt.current_mode(), "fix", "Enter picks the cursor line")
    t.eq(state.surfaces.prompt.picker_win, nil, "picking closes the picker")

    prompt.pick_mode()
    buf = state.surfaces.prompt.picker_buf
    win = state.surfaces.prompt.picker_win
    vim.api.nvim_win_set_cursor(win, { 3, 0 })
    bound("<Esc>")()
    t.eq(prompt.current_mode(), "fix", "Escape keeps the current mode")
    t.eq(state.surfaces.prompt.picker_win, nil, "cancel closes the picker")

    surfaces.close_prompt({ focus_agent = false })
  end)

  t.test("unsupported modes are rejected instead of silently falling back", function()
    local previous = config.values.backend.mode
    local ok, err = pcall(config.setup, { backend = { mode = "unsupported" } })

    t.eq(ok, false)
    t.eq(err:find("Configure one of: fix, explain, investigate, review, propose", 1, true) ~= nil, true)
    t.eq(err:find("PromptWindow with <C-k>", 1, true) ~= nil, true)
    t.eq(config.values.backend.mode, previous)
  end)

  t.test("keymaps.flow defaults to an explicit normal-mode toggle", function()
    t.eq(config.values.keymaps.flow, "F")
  end)
end
