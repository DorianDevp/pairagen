return function(t)
  local card = require("loopbiotic.card")
  local session = require("loopbiotic.session")
  local state = require("loopbiotic.state")

  -- Stub card.show so the helper stays a pure state transition in tests.
  local function with_stubbed_show(fn)
    local original = card.show
    local shown = {}
    card.show = function(shown_card)
      table.insert(shown, shown_card)
    end
    local ok, err = pcall(fn, shown)
    card.show = original
    if not ok then
      error(err, 0)
    end
  end

  local function turn_result()
    return {
      session_id = "session-1",
      token_usage = { total_tokens = 20, input_tokens = 15, output_tokens = 5 },
      turn_token_usage = { total_tokens = 8 },
      model = "reported-model",
      context_report = { enabled = true, used_tokens = 100 },
      attempts = { { outcome = "accepted" } },
      goal = { statement = "updated goal" },
      card = { id = "c1", kind = "finding", title = "Found it" },
    }
  end

  t.test("apply_turn_result records usage, goal, model, and shows the card", function()
    state.reset()
    with_stubbed_show(function(shown)
      session.apply_turn_result(turn_result())
      t.eq(state.token_usage.total_tokens, 20, "token_usage")
      t.eq(state.turn_token_usage.total_tokens, 8, "turn_token_usage")
      t.eq(state.backend_model, "reported-model", "backend_model")
      t.eq(state.context_report.enabled, true, "context_report")
      t.eq(state.goal.statement, "updated goal", "goal")
      t.eq(#shown, 1, "card.show calls")
      t.eq(shown[1].id, "c1", "shown card")
    end)
    state.reset()
  end)

  t.test("apply_turn_result keeps the previous goal and model when absent", function()
    state.reset()
    state.goal = { statement = "existing goal" }
    state.backend_model = "existing-model"
    local result = turn_result()
    result.goal = nil
    result.model = nil
    with_stubbed_show(function()
      session.apply_turn_result(result)
      t.eq(state.goal.statement, "existing goal", "goal")
      t.eq(state.backend_model, "existing-model", "backend_model")
    end)
    state.reset()
  end)

  t.test("apply_turn_result can skip the model update (patch results)", function()
    state.reset()
    state.backend_model = "existing-model"
    with_stubbed_show(function()
      session.apply_turn_result(turn_result(), { update_model = false })
      t.eq(state.backend_model, "existing-model", "backend_model")
      t.eq(state.token_usage.total_tokens, 20, "token_usage")
    end)
    state.reset()
  end)
end
