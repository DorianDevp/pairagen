return function(t)
  local prompt = require("loopbiotic.prompt")
  local session = require("loopbiotic.session")
  local state = require("loopbiotic.state")
  local util = require("loopbiotic.util")

  -- Run fn with vim.notify captured into a list; always restores the original.
  local function with_notify(fn)
    local original = vim.notify
    local notifications = {}
    vim.notify = function(message, level)
      table.insert(notifications, { message = message, level = level })
    end
    local ok, err = pcall(fn, notifications)
    vim.notify = original
    if not ok then
      error(err, 0)
    end
  end

  t.test("guard swallows errors, logs every one, notifies once per label", function()
    local log = require("loopbiotic.log")
    local original_event = log.event
    local events = {}
    log.event = function(kind, data)
      table.insert(events, { kind = kind, data = data })
    end

    with_notify(function(notifications)
      local calls = 0
      local wrapped = util.guard("test.boom", function()
        calls = calls + 1
        error("kaboom")
      end)

      t.eq(wrapped(), nil, "returns nil on error")
      t.eq(wrapped(), nil, "returns nil on repeated error")
      t.eq(calls, 2, "inner function ran each time")

      t.eq(#events, 2, "every error is logged")
      t.eq(events[1].kind, "client_error", "event kind")
      t.eq(events[1].data.label, "test.boom", "event label")
      t.eq(type(events[1].data.traceback), "string", "traceback string")
      t.eq(events[1].data.traceback:find("kaboom", 1, true) ~= nil, true, "traceback carries the message")

      t.eq(#notifications, 1, "notified once despite two errors")
      t.eq(notifications[1].level, vim.log.levels.ERROR, "notify level")
      t.eq(notifications[1].message:find("test.boom", 1, true) ~= nil, true, "notify names the label")
      t.eq(notifications[1].message:find("session preserved", 1, true) ~= nil, true, "notify reassures")

      -- A different label is its own notification budget.
      util.guard("test.other", function()
        error("kaboom")
      end)()
      t.eq(#notifications, 2, "new label notifies again")
    end)

    log.event = original_event
  end)

  t.test("guard passes results through on success", function()
    local wrapped = util.guard("test.pass", function(a, b)
      return a + b, "ok"
    end)
    local sum, tag = wrapped(2, 3)
    t.eq(sum, 5)
    t.eq(tag, "ok")
  end)

  t.test("decoded JSON nulls become ordinary absent Lua values", function()
    local decoded = vim.json.decode('{"context_report":null,"location":{"annotation":null},"items":[1,null,3]}')
    local normalized = util.normalize_json_nulls(decoded)

    t.eq(normalized.context_report, nil, "top-level null")
    t.eq(normalized.location.annotation, nil, "nested null")
    t.eq(normalized.items[1], 1, "array prefix")
    t.eq(normalized.items[2], nil, "array null")
    t.eq(normalized.items[3], 3, "array suffix")
  end)

  t.test("attempt logs keep violation classes while redacting contract content", function()
    local sanitized = require("loopbiotic.log").sanitize({
      outcome = "contract_retry",
      violation_class = "context_mismatch",
      detail = "private source line",
      candidate_card = { explanation = "private patch" },
    })

    t.eq(sanitized.outcome, "contract_retry", "outcome")
    t.eq(sanitized.violation_class, "context_mismatch", "violation class")
    t.eq(sanitized.detail.redacted, true, "detail redacted")
    t.eq(sanitized.candidate_card.redacted, true, "candidate redacted")
  end)

  t.test("repeated_error escalates only on identical consecutive messages", function()
    t.eq(session.repeated_error(nil, "boom"), false, "first error")
    t.eq(session.repeated_error("boom", "boom"), true, "same error twice")
    t.eq(session.repeated_error("boom", "other"), false, "different error")
    t.eq(session.repeated_error("boom", nil), false, "no current message")
    t.eq(session.repeated_error("", ""), false, "empty messages never escalate")
    t.eq(session.repeated_error(nil, nil), false, "nothing at all")
  end)

  t.test("apply_turn_result escalates the second identical backend error", function()
    state.reset()
    local card = require("loopbiotic.card")
    local original_show = card.show
    local shown = {}
    card.show = function(shown_card)
      table.insert(shown, shown_card)
    end

    local function error_result(message)
      return {
        session_id = "s1",
        card = {
          id = "e1",
          kind = "error",
          title = "Backend request failed",
          message = message,
          actions = { "retry", "edit_prompt", "stop" },
        },
      }
    end

    local ok, err = pcall(function()
      with_notify(function(notifications)
        session.apply_turn_result(error_result("boom"))
        t.eq(state.last_backend_error, "boom", "first error tracked")
        t.eq(shown[1].warnings, nil, "no warning on first occurrence")
        t.eq(#notifications, 0, "no escalation on first occurrence")

        session.apply_turn_result(error_result("boom"))
        t.eq(#shown[2].warnings, 1, "warning appended on repeat")
        t.eq(shown[2].warnings[1], session.repeat_guidance, "warning text")
        t.eq(#notifications, 1, "escalated notification")
        t.eq(notifications[1].level, vim.log.levels.ERROR, "escalation level")
        t.eq(notifications[1].message:find("boom", 1, true) ~= nil, true, "full message included")
        t.eq(notifications[1].message:find(":checkhealth loopbiotic", 1, true) ~= nil, true, "guidance included")

        session.apply_turn_result(error_result("other"))
        t.eq(state.last_backend_error, "other", "tracking follows the message")
        t.eq(shown[3].warnings, nil, "different message does not escalate")

        state.backend_preflight_error = "stale preflight"
        session.apply_turn_result({ card = { id = "f1", kind = "finding", title = "ok" } })
        t.eq(state.last_backend_error, nil, "non-error card clears tracking")
        t.eq(state.backend_preflight_error, nil, "successful turn clears the preflight error")
      end)
    end)

    card.show = original_show
    state.reset()
    if not ok then
      error(err, 0)
    end
  end)

  t.test("on_warmup error sets the preflight state; success clears it", function()
    state.reset()
    with_notify(function(notifications)
      prompt.on_warmup({ error = { code = -32098, message = "backend is down" } })
      t.eq(state.backend_preflight_error, "backend is down", "error stored")
      t.eq(#notifications, 1, "warned once")
      t.eq(notifications[1].level, vim.log.levels.WARN, "warn level")
      t.eq(notifications[1].message:find("backend is down", 1, true) ~= nil, true, "error in warning")

      prompt.on_warmup({ error = { code = -32098, message = "backend is down" } })
      t.eq(#notifications, 1, "identical failure does not re-warn")

      prompt.on_warmup({ error = { code = -32098, message = "another failure" } })
      t.eq(#notifications, 2, "new failure warns again")
      t.eq(state.backend_preflight_error, "another failure", "latest error stored")

      prompt.on_warmup({ result = { ok = true } })
      t.eq(state.backend_preflight_error, nil, "successful warmup clears the error")
    end)
    state.reset()
  end)

  t.test("preflight footer is one line, truncated, and points at checkhealth", function()
    local footer = prompt.preflight_footer("something\nbroke   badly")
    t.eq(footer, " backend not ready: something broke badly — :checkhealth loopbiotic ")

    local long = prompt.short_error(string.rep("x", 100))
    t.eq(#long, 60, "truncated to footer size")
    t.eq(long:sub(-3), "...", "ellipsis")
  end)

  t.test("prompt stash: set on submit, kept on failed start, cleared on success", function()
    t.eq(prompt.next_stash(nil, "submit", "fix the bug"), "fix the bug", "submit stashes")
    t.eq(prompt.next_stash("fix the bug", "start_error"), "fix the bug", "failed start keeps the stash")
    t.eq(prompt.next_stash("fix the bug", "start_ok"), nil, "successful start clears the stash")
    t.eq(prompt.next_stash("old", "submit", "new"), "new", "resubmit replaces the stash")
  end)
end
