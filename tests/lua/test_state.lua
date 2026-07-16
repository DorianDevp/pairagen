return function(t)
  local state = require("loopbiotic.state")

  t.test("state.reset restores defaults after mutation", function()
    state.session_id = "session-1"
    state.source_buf = 42
    state.card = { id = "c1", kind = "finding" }
    state.goal = { statement = "goal" }
    state.token_usage = { total_tokens = 12 }
    state.details_expanded = true
    state.thinking_frame = 7
    state.workspace_hints = { { file = "a.lua" } }

    state.reset()

    t.eq(state.session_id, nil, "session_id")
    t.eq(state.source_buf, nil, "source_buf")
    t.eq(state.card, nil, "card")
    t.eq(state.goal, nil, "goal")
    t.eq(state.token_usage, nil, "token_usage")
    t.eq(state.details_expanded, false, "details_expanded")
    t.eq(state.thinking_frame, nil, "thinking_frame")
    t.eq(state.workspace_hints, nil, "workspace_hints")
  end)

  t.test("state.reset keeps reset callable and idempotent", function()
    state.reset()
    state.reset()
    t.eq(type(state.reset), "function")
    t.eq(state.details_expanded, false)
  end)
end
